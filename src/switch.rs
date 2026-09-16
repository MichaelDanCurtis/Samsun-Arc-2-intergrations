//! The two switch modes from the spec, `arrive`, and the status report.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tracing::{info, warn};

use crate::config::Config;
use crate::input::Input;
use crate::monitor::{Chain, ChainError, Ensure};
use crate::openlogi::{KeyboardAgent, Probe, SwitchResult};
use crate::resolve::{ResolveError, Target, resolve};
use crate::state::StateStore;

pub struct Deps {
    pub config: Config,
    pub chain: Chain,
    pub agent: Arc<dyn KeyboardAgent>,
    pub state: StateStore,
    pub relay: Arc<dyn Relay>,
}

#[derive(Debug)]
pub struct RelayOutcome {
    pub peer: String,
    pub result: Result<String, String>,
}

#[async_trait]
pub trait Relay: Send + Sync {
    async fn relay(&self, config: &Config, slot: u8) -> Vec<RelayOutcome>;
}

/// Relay nothing (no `[lan]` section, or tests).
pub struct NoRelay;

#[async_trait]
impl Relay for NoRelay {
    async fn relay(&self, _config: &Config, _slot: u8) -> Vec<RelayOutcome> {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Invoked by an `OpenLogi` hook: monitor only.
    Hook,
    /// Invoked by a person, the Stream Deck, or a peer.
    User,
}

/// Hook mode is exactly "`OpenLogi` set `OPENLOGI_TRIGGER`".
#[must_use]
pub fn mode_from_env() -> Mode {
    if std::env::var_os("OPENLOGI_TRIGGER").is_some() {
        Mode::Hook
    } else {
        Mode::User
    }
}

#[derive(Debug)]
pub enum Outcome {
    /// The local agent moved the peripherals; its hook moves the monitor.
    Delegated,
    Monitor {
        ensure: Ensure,
        relayed: Vec<RelayOutcome>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SwitchError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Monitor(#[from] ChainError),
    #[error("the OpenLogi agent refused the switch: {0}")]
    AgentRefused(String),
}

impl SwitchError {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Resolve(_) => 3,
            Self::Monitor(_) => 2,
            Self::AgentRefused(_) => 1,
        }
    }
}

pub async fn run_switch(deps: &Deps, target: &Target, mode: Mode) -> Result<Outcome, SwitchError> {
    let resolved = resolve(&deps.config, target)?;
    let target_slot = resolved.machine.as_ref().map(|machine| machine.slot);
    let mut keyboard_here = false;
    if mode == Mode::User {
        match deps.agent.probe().await {
            Probe::Here { current_slot, name } => {
                keyboard_here = true;
                if let Some(slot) = target_slot.filter(|slot| *slot != current_slot) {
                    match deps.agent.switch(slot).await {
                        SwitchResult::Switched => {
                            info!(%name, slot, "delegated to the OpenLogi agent; its hook moves the monitor");
                            return Ok(Outcome::Delegated);
                        }
                        SwitchResult::AlreadyCurrent => {}
                        SwitchResult::Refused(message) => {
                            return Err(SwitchError::AgentRefused(message));
                        }
                        SwitchResult::NotFound => {
                            warn!(
                                "keyboard vanished between probe and switch; moving the monitor only"
                            );
                            keyboard_here = false;
                        }
                        SwitchResult::Unavailable(message) => {
                            warn!(%message, "agent unavailable for the switch; moving the monitor only");
                            keyboard_here = false;
                        }
                    }
                }
            }
            Probe::Elsewhere => {}
            Probe::Unavailable(message) => {
                warn!(%message, "OpenLogi unavailable; treating the keyboard as elsewhere");
            }
        }
    }
    let ensure = deps.chain.ensure(resolved.input).await?;
    let now = Utc::now();
    if let Err(error) = deps.state.update(|state| {
        state.last_target_slot = target_slot;
        state.last_switch_at = Some(now);
        state.last_known_input = Some(resolved.input);
    }) {
        warn!(%error, "could not write state.json");
    }
    let relayed = match (mode, keyboard_here, target_slot) {
        (Mode::User, false, Some(slot)) => deps.relay.relay(&deps.config, slot).await,
        _ => Vec::new(),
    };
    for outcome in &relayed {
        match &outcome.result {
            Ok(result) => info!(peer = %outcome.peer, %result, "relayed"),
            Err(error) => warn!(peer = %outcome.peer, %error, "relay failed"),
        }
    }
    Ok(Outcome::Monitor { ensure, relayed })
}

#[derive(Debug, Clone, Copy)]
pub struct ArriveTiming {
    pub rate_limit: Duration,
    pub grace: Duration,
}

impl Default for ArriveTiming {
    fn default() -> Self {
        Self {
            rate_limit: Duration::from_secs(30),
            grace: Duration::from_millis(2500),
        }
    }
}

#[derive(Debug)]
pub enum ArriveOutcome {
    RateLimited,
    AlreadyMine,
    Switched,
    Failed(ChainError),
}

/// The keyboard just connected here: after a grace period, make sure the
/// monitor shows this machine. Never a second writer on the happy path.
pub async fn run_arrive(deps: &Deps, timing: ArriveTiming) -> ArriveOutcome {
    let now = Utc::now();
    let state = deps.state.load();
    if let Some(last) = state.last_arrive_at
        && now.signed_duration_since(last)
            < chrono::Duration::from_std(timing.rate_limit).unwrap_or_default()
    {
        return ArriveOutcome::RateLimited;
    }
    if let Err(error) = deps.state.update(|state| state.last_arrive_at = Some(now)) {
        warn!(%error, "could not write state.json");
    }
    tokio::time::sleep(timing.grace).await;
    let mine = deps.config.this().input;
    if deps.chain.current().await == Some(mine) {
        return ArriveOutcome::AlreadyMine;
    }
    match deps.chain.switch(mine).await {
        Ok(_) => {
            let _ = deps.state.update(|state| {
                state.last_known_input = Some(mine);
                state.last_switch_at = Some(Utc::now());
            });
            ArriveOutcome::Switched
        }
        Err(error) => ArriveOutcome::Failed(error),
    }
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub machine: String,
    pub input: Option<Input>,
    pub keyboard_here: bool,
    pub current_slot: Option<u8>,
    pub last_target_slot: Option<u8>,
    pub last_switch_at: Option<DateTime<Utc>>,
}

pub async fn status(deps: &Deps) -> StatusReport {
    let input = deps.chain.current().await;
    if input.is_some() {
        let _ = deps.state.update(|state| state.last_known_input = input);
    }
    let (keyboard_here, current_slot) = match deps.agent.probe().await {
        Probe::Here { current_slot, .. } => (true, Some(current_slot)),
        _ => (false, None),
    };
    let state = deps.state.load();
    StatusReport {
        machine: deps.config.this_machine.clone(),
        input,
        keyboard_here,
        current_slot,
        last_target_slot: state.last_target_slot,
        last_switch_at: state.last_switch_at,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::monitor::test_support::Fake;
    use crate::openlogi::{Probe, SwitchResult};

    struct FakeAgent {
        probe: Mutex<Option<Probe>>, // None = Unavailable
        switches: Mutex<Vec<u8>>,
        refuse: Option<u8>,
    }

    impl FakeAgent {
        fn here(slot: u8) -> Arc<Self> {
            Arc::new(Self {
                probe: Mutex::new(Some(Probe::Here {
                    current_slot: slot,
                    name: "K".into(),
                })),
                switches: Mutex::new(vec![]),
                refuse: None,
            })
        }
        fn away() -> Arc<Self> {
            Arc::new(Self {
                probe: Mutex::new(Some(Probe::Elsewhere)),
                switches: Mutex::new(vec![]),
                refuse: None,
            })
        }
        fn down() -> Arc<Self> {
            Arc::new(Self {
                probe: Mutex::new(None),
                switches: Mutex::new(vec![]),
                refuse: None,
            })
        }
        fn here_refusing(slot: u8, refuse: u8) -> Arc<Self> {
            Arc::new(Self {
                probe: Mutex::new(Some(Probe::Here {
                    current_slot: slot,
                    name: "K".into(),
                })),
                switches: Mutex::new(vec![]),
                refuse: Some(refuse),
            })
        }
    }

    #[async_trait]
    impl KeyboardAgent for FakeAgent {
        async fn probe(&self) -> Probe {
            match self.probe.lock().unwrap().as_ref() {
                Some(Probe::Here { current_slot, name }) => Probe::Here {
                    current_slot: *current_slot,
                    name: name.clone(),
                },
                Some(Probe::Elsewhere) => Probe::Elsewhere,
                Some(Probe::Unavailable(m)) => Probe::Unavailable(m.clone()),
                None => Probe::Unavailable("down".into()),
            }
        }
        async fn switch(&self, slot: u8) -> SwitchResult {
            self.switches.lock().unwrap().push(slot);
            if self.refuse == Some(slot) {
                return SwitchResult::Refused("unpaired".into());
            }
            SwitchResult::Switched
        }
    }

    struct RecordingRelay(Mutex<Vec<u8>>);

    #[async_trait]
    impl Relay for RecordingRelay {
        async fn relay(&self, _config: &Config, slot: u8) -> Vec<RelayOutcome> {
            self.0.lock().unwrap().push(slot);
            vec![RelayOutcome {
                peer: "avapc".into(),
                result: Ok("delegated".into()),
            }]
        }
    }

    fn deps(
        agent: Arc<dyn KeyboardAgent>,
        backend: Arc<Fake>,
        relay: Arc<RecordingRelay>,
    ) -> (tempfile::TempDir, Deps) {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let chain = Chain::new(vec![backend])
            .with_timing(Duration::from_millis(50), Duration::from_millis(1));
        let deps = Deps {
            config,
            chain,
            agent,
            state: StateStore::in_dir(dir.path()),
            relay,
        };
        (dir, deps)
    }

    fn relay() -> Arc<RecordingRelay> {
        Arc::new(RecordingRelay(Mutex::new(vec![])))
    }

    #[tokio::test]
    async fn user_mode_delegates_when_the_keyboard_is_here_on_another_slot() {
        let agent = FakeAgent::here(1);
        let backend = Fake::new("st");
        let (_d, deps) = deps(agent.clone(), backend.clone(), relay());
        let outcome = run_switch(&deps, &Target::Slot(2), Mode::User)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Delegated));
        assert_eq!(*agent.switches.lock().unwrap(), vec![2]);
        assert_eq!(
            backend.switches.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the hook does the monitor"
        );
    }

    #[tokio::test]
    async fn hook_mode_never_calls_the_agent_and_only_moves_the_monitor() {
        let agent = FakeAgent::here(1);
        let backend = Fake::new("st");
        let r = relay();
        let (_d, deps) = deps(agent.clone(), backend.clone(), r.clone());
        let outcome = run_switch(&deps, &Target::Slot(2), Mode::Hook)
            .await
            .unwrap();
        assert!(
            matches!(outcome, Outcome::Monitor { ensure: Ensure::Switched { .. }, ref relayed } if relayed.is_empty())
        );
        assert!(agent.switches.lock().unwrap().is_empty());
        assert!(r.0.lock().unwrap().is_empty());
        assert_eq!(deps.state.load().last_known_input, Some(Input::Hdmi3));
    }

    #[tokio::test]
    async fn keyboard_here_on_the_target_slot_means_monitor_only() {
        let agent = FakeAgent::here(2);
        let backend = Fake::new("st");
        let (_d, deps) = deps(agent.clone(), backend.clone(), relay());
        let outcome = run_switch(&deps, &Target::Slot(2), Mode::User)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Monitor { .. }));
        assert!(agent.switches.lock().unwrap().is_empty());
        assert_eq!(
            backend.switches.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn keyboard_elsewhere_or_agent_down_moves_the_monitor_and_relays() {
        for agent in [FakeAgent::away(), FakeAgent::down()] {
            let backend = Fake::new("st");
            let r = relay();
            let (_d, deps) = deps(agent, backend.clone(), r.clone());
            let outcome = run_switch(&deps, &Target::Machine("avapc".into()), Mode::User)
                .await
                .unwrap();
            assert!(matches!(outcome, Outcome::Monitor { ref relayed, .. } if relayed.len() == 1));
            assert_eq!(*r.0.lock().unwrap(), vec![3]);
        }
    }

    #[tokio::test]
    async fn a_refused_agent_switch_is_an_error_with_exit_code_one() {
        let (_d, deps) = deps(FakeAgent::here_refusing(1, 3), Fake::new("st"), relay());
        let error = run_switch(&deps, &Target::Slot(3), Mode::User)
            .await
            .unwrap_err();
        assert!(matches!(error, SwitchError::AgentRefused(_)));
        assert_eq!(error.exit_code(), 1);
    }

    #[tokio::test]
    async fn unresolvable_targets_and_dead_backends_have_their_exit_codes() {
        let (_d, deps) = deps(FakeAgent::away(), Fake::new("st"), relay());
        assert_eq!(
            run_switch(&deps, &Target::Slot(9), Mode::Hook)
                .await
                .unwrap_err()
                .exit_code(),
            3
        );
        let (_d2, dead) = self::deps(FakeAgent::away(), Fake::failing("st"), relay());
        assert_eq!(
            run_switch(&dead, &Target::Input(Input::Dp), Mode::Hook)
                .await
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[tokio::test]
    async fn arrive_reads_first_rate_limits_and_switches_to_my_input() {
        let backend = Fake::seen("st", Input::Hdmi1);
        let (_d, deps) = deps(FakeAgent::here(1), backend.clone(), relay());
        let fast = ArriveTiming {
            rate_limit: Duration::from_secs(30),
            grace: Duration::ZERO,
        };
        assert!(matches!(
            run_arrive(&deps, fast).await,
            ArriveOutcome::AlreadyMine
        ));
        assert!(matches!(
            run_arrive(&deps, fast).await,
            ArriveOutcome::RateLimited
        ));
        let elsewhere = Fake::seen("st", Input::Hdmi2);
        let (_d2, deps2) = self::deps(FakeAgent::here(1), elsewhere.clone(), relay());
        assert!(matches!(
            run_arrive(&deps2, fast).await,
            ArriveOutcome::Switched
        ));
        assert_eq!(*elsewhere.current.lock().unwrap(), Some(Input::Hdmi1));
    }

    #[test]
    fn hook_mode_is_the_presence_of_the_trigger_variable() {
        temp_env::with_var("OPENLOGI_TRIGGER", Some("key"), || {
            assert_eq!(mode_from_env(), Mode::Hook);
        });
        temp_env::with_var("OPENLOGI_TRIGGER", None::<&str>, || {
            assert_eq!(mode_from_env(), Mode::User);
        });
    }
}
