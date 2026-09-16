//! `state.json`: what this machine last did and last saw, written atomically.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::input::Input;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct State {
    #[serde(default)]
    pub last_target_slot: Option<u8>,
    #[serde(default)]
    pub last_switch_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_known_input: Option<Input>,
    #[serde(default)]
    pub last_arrive_at: Option<DateTime<Utc>>,
}

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("state.json"),
        }
    }

    /// The saved state, or the default when the file is missing or unreadable.
    #[must_use]
    pub fn load(&self) -> State {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                warn!(path = %self.path.display(), %error, "state.json unreadable; starting fresh");
                State::default()
            }),
            Err(_) => State::default(),
        }
    }

    /// Write `state` via a sibling temp file and rename, so a crash mid-write
    /// never leaves a torn file.
    pub fn save(&self, state: &State) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(state)?)?;
        std::fs::rename(&temp, &self.path)
    }

    /// Load, mutate, save; returns the saved state.
    pub fn update(&self, f: impl FnOnce(&mut State)) -> std::io::Result<State> {
        let mut state = self.load();
        f(&mut state);
        self.save(&state)?;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_corrupt_files_read_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::in_dir(dir.path());
        assert_eq!(store.load(), State::default());
        std::fs::write(dir.path().join("state.json"), "{not json").unwrap();
        assert_eq!(store.load(), State::default());
    }

    #[test]
    fn update_persists_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::in_dir(dir.path());
        let saved = store
            .update(|state| {
                state.last_target_slot = Some(2);
                state.last_known_input = Some(Input::Hdmi3);
            })
            .unwrap();
        assert_eq!(saved.last_target_slot, Some(2));
        assert_eq!(store.load().last_known_input, Some(Input::Hdmi3));
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["state.json"]);
    }
}
