//! The Ark's canonical input names.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// One physical input on the One Connect box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Input {
    Hdmi1,
    Hdmi2,
    Hdmi3,
    Dp,
}

impl Input {
    /// Every input, in port order.
    pub const ALL: [Input; 4] = [Input::Hdmi1, Input::Hdmi2, Input::Hdmi3, Input::Dp];

    /// The canonical name, which is also the `SmartThings` input id.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Input::Hdmi1 => "HDMI1",
            Input::Hdmi2 => "HDMI2",
            Input::Hdmi3 => "HDMI3",
            Input::Dp => "DP",
        }
    }
}

/// An input name outside the canonical set.
#[derive(Debug, thiserror::Error)]
#[error("unknown input {0:?}; expected one of HDMI1, HDMI2, HDMI3, DP")]
pub struct UnknownInput(pub String);

impl FromStr for Input {
    type Err = UnknownInput;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Input::ALL
            .into_iter()
            .find(|input| input.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| UnknownInput(s.to_string()))
    }
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for Input {
    type Error = UnknownInput;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Input> for String {
    fn from(input: Input) -> Self {
        input.as_str().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::Input;

    #[test]
    fn parses_case_insensitively_and_prints_canonically() {
        assert_eq!("hdmi2".parse::<Input>().unwrap(), Input::Hdmi2);
        assert_eq!("DP".parse::<Input>().unwrap(), Input::Dp);
        assert_eq!(Input::Hdmi3.to_string(), "HDMI3");
        assert!("HDMI4".parse::<Input>().is_err());
    }

    #[test]
    fn toml_round_trip_uses_the_canonical_string() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Row { input: Input }
        let row: Row = toml::from_str(r#"input = "hdmi1""#).unwrap();
        assert_eq!(row.input, Input::Hdmi1);
        assert_eq!(toml::to_string(&row).unwrap().trim(), r#"input = "HDMI1""#);
    }
}
