//! Turn a slot, machine name, or input into the input to show.

use crate::config::{Config, Machine};
use crate::input::Input;

/// What the caller asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Slot(u8),
    Machine(String),
    Input(Input),
}

/// The input to show, and the machine it belongs to when one is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub input: Input,
    pub machine: Option<Machine>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no machine is configured on slot {0}")]
    UnknownSlot(u8),
    #[error("{0:?} is not a configured machine")]
    UnknownMachine(String),
}

pub fn resolve(config: &Config, target: &Target) -> Result<Resolved, ResolveError> {
    let machine = match target {
        Target::Slot(slot) => config
            .machine_for_slot(*slot)
            .ok_or(ResolveError::UnknownSlot(*slot))?,
        Target::Machine(name) => config
            .machine_named(name)
            .ok_or_else(|| ResolveError::UnknownMachine(name.clone()))?,
        Target::Input(input) => {
            return Ok(Resolved {
                input: *input,
                machine: config.machines.iter().find(|m| m.input == *input).cloned(),
            });
        }
    };
    Ok(Resolved {
        input: machine.input,
        machine: Some(machine.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let config: Config = toml::from_str(crate::config::tests::SAMPLE).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn resolves_by_slot_machine_and_input() {
        let config = config();
        let by_slot = resolve(&config, &Target::Slot(2)).unwrap();
        assert_eq!(by_slot.input, Input::Hdmi3);
        assert_eq!(by_slot.machine.as_ref().unwrap().name, "oldwaterboy");
        let by_name = resolve(&config, &Target::Machine("avapc".into())).unwrap();
        assert_eq!(by_name.input, Input::Hdmi2);
        let by_input = resolve(&config, &Target::Input(Input::Dp)).unwrap();
        assert_eq!(by_input.input, Input::Dp);
        assert!(by_input.machine.is_none(), "DP is not a configured machine");
    }

    #[test]
    fn unknown_targets_fail_with_the_target_in_the_error() {
        let config = config();
        assert!(matches!(
            resolve(&config, &Target::Slot(3)).map(|_| ()),
            Ok(())
        ));
        assert!(matches!(
            resolve(&config, &Target::Machine("ghost".into())),
            Err(ResolveError::UnknownMachine(name)) if name == "ghost"
        ));
        assert!(matches!(
            resolve(&config, &Target::Slot(9)),
            Err(ResolveError::UnknownSlot(9))
        ));
    }
}
