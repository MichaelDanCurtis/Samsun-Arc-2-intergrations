//! Long-running mode: LAN listener, periodic input refresh, and the deck.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::Config;
use crate::input::Input;
#[cfg(test)]
use crate::monitor::Chain;
use crate::switch::{Deps, status};

const REFRESH_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeckButton {
    pub machine: String,
    pub lit: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DeckView {
    pub buttons: Vec<DeckButton>,
    /// Read by the Stream Deck driver (Task 13) to show a transient error state.
    #[allow(dead_code)]
    pub error_until: Option<Instant>,
}

/// One button per `[streamdeck].buttons` entry (or every machine when that
/// list is empty), lit when the machine owns the current input.
#[must_use]
pub fn view_for(config: &Config, input: Option<Input>, error_until: Option<Instant>) -> DeckView {
    let names: Vec<&str> = if config.streamdeck.buttons.is_empty() {
        config.machines.iter().map(|m| m.name.as_str()).collect()
    } else {
        config
            .streamdeck
            .buttons
            .iter()
            .map(String::as_str)
            .collect()
    };
    let buttons = names
        .into_iter()
        .filter_map(|name| config.machine_named(name))
        .map(|machine| DeckButton {
            machine: machine.name.clone(),
            lit: input == Some(machine.input),
        })
        .collect();
    DeckView {
        buttons,
        error_until,
    }
}

pub async fn refresh_once(
    deps: &Deps,
    view: &watch::Sender<DeckView>,
    error_until: Option<Instant>,
) {
    let report = status(deps).await;
    let _ = view.send(view_for(&deps.config, report.input, error_until));
}

pub async fn run(deps: Arc<Deps>, lan_secret: Option<String>) -> anyhow::Result<()> {
    let (view_tx, view_rx) = watch::channel(DeckView::default());
    match (&deps.config.lan, lan_secret) {
        (Some(lan), Some(secret)) => {
            let router = crate::lan::router(Arc::clone(&deps), secret);
            let listen = lan.listen.clone();
            tokio::spawn(async move {
                if let Err(error) = crate::lan::serve(&listen, router).await {
                    warn!(%error, "LAN listener stopped");
                }
            });
        }
        (Some(_), None) => {
            warn!("[lan] configured but no secret stored; run `deskswitch lan set-secret`");
        }
        (None, _) => info!("no [lan] section; relayed switches disabled"),
    }
    #[cfg(feature = "streamdeck")]
    let mut deck_events = if deps.config.streamdeck.enabled {
        crate::streamdeck::spawn(view_rx.clone(), deps.config.streamdeck.font.clone())
    } else {
        None
    };
    refresh_once(&deps, &view_tx, None).await;
    let mut ticker = tokio::time::interval(REFRESH_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` fires at once; the explicit refresh above already covered startup.
    ticker.tick().await;
    loop {
        #[cfg(feature = "streamdeck")]
        let deck_event = async {
            match deck_events.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        #[cfg(not(feature = "streamdeck"))]
        let deck_event = std::future::pending::<Option<usize>>();
        tokio::select! {
            _ = ticker.tick() => refresh_once(&deps, &view_tx, None).await,
            Some(index) = deck_event => {
                let machine = view_rx.borrow().buttons.get(index).map(|b| b.machine.clone());
                let Some(machine) = machine else { continue };
                let target = crate::resolve::Target::Machine(machine.clone());
                let error_until = match crate::switch::run_switch(&deps, &target, crate::switch::Mode::User).await {
                    Ok(_) => None,
                    Err(error) => {
                        warn!(%machine, %error, "deck switch failed");
                        Some(Instant::now() + Duration::from_secs(1))
                    }
                };
                refresh_once(&deps, &view_tx, error_until).await;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("daemon stopping");
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::test_support::Fake;
    use crate::openlogi::{KeyboardAgent, Probe, SwitchResult};
    use crate::switch::NoRelay;

    struct Away;
    #[async_trait::async_trait]
    impl KeyboardAgent for Away {
        async fn probe(&self) -> Probe {
            Probe::Elsewhere
        }
        async fn switch(&self, _slot: u8) -> SwitchResult {
            SwitchResult::NotFound
        }
    }

    #[test]
    fn the_lit_button_is_the_machine_on_the_current_input() {
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let view = view_for(&config, Some(Input::Hdmi2), None);
        let lit: Vec<&str> = view
            .buttons
            .iter()
            .filter(|b| b.lit)
            .map(|b| b.machine.as_str())
            .collect();
        assert_eq!(lit, ["avapc"]);
        assert_eq!(
            view.buttons
                .iter()
                .map(|b| b.machine.as_str())
                .collect::<Vec<_>>(),
            ["mac", "avapc", "oldwaterboy"],
            "config order"
        );
        assert!(view_for(&config, None, None).buttons.iter().all(|b| !b.lit));

        // SAMPLE lists buttons in the same order as machines, so the above
        // alone can't tell `buttons` order from `machines` order apart.
        let mut reordered = config.clone();
        reordered.streamdeck.buttons = vec!["oldwaterboy".into(), "mac".into()];
        let view = view_for(&reordered, Some(Input::Hdmi3), None);
        assert_eq!(
            view.buttons
                .iter()
                .map(|b| (b.machine.as_str(), b.lit))
                .collect::<Vec<_>>(),
            [("oldwaterboy", true), ("mac", false)],
            "buttons order, and avapc absent"
        );

        reordered.streamdeck.buttons = vec![];
        let view = view_for(&reordered, None, None);
        assert_eq!(
            view.buttons
                .iter()
                .map(|b| b.machine.as_str())
                .collect::<Vec<_>>(),
            ["mac", "avapc", "oldwaterboy"],
            "falls back to machines order"
        );
    }

    #[tokio::test]
    async fn refresh_publishes_the_input_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        let deps = Deps {
            config,
            chain: Chain::new(vec![Fake::seen("st", Input::Hdmi3)]),
            agent: Arc::new(Away),
            state: crate::state::StateStore::in_dir(dir.path()),
            relay: Arc::new(NoRelay),
        };
        let (tx, rx) = tokio::sync::watch::channel(DeckView::default());
        refresh_once(&deps, &tx, None).await;
        let lit: Vec<String> = rx
            .borrow()
            .buttons
            .iter()
            .filter(|b| b.lit)
            .map(|b| b.machine.clone())
            .collect();
        assert_eq!(lit, ["oldwaterboy"]);
        assert_eq!(deps.state.load().last_known_input, Some(Input::Hdmi3));
    }
}
