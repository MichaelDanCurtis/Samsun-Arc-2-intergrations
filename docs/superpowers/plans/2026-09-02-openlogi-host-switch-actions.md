# OpenLogi Host-Switch Actions and `openlogi host` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give OpenLogi two upstreamable seams around its existing linked host switching: per-slot hook actions (plus an `arrive` fallback) fired when a keyboard switches hosts, and an `openlogi host list|switch` CLI backed by new IPC methods, so an external desk service can follow the keyboard to another machine.

**Architecture:** Upstream 0.8.3 already diverts a keyboard's three host controls, learns the pressed slot, moves linked mice, then the keyboard (`crates/openlogi-device/src/session/host_switch.rs`, driven by `crates/openlogi-agent-core/src/watchers/host_switch.rs`). PR 1 adds a `HostSwitchActions` config table on the keyboard, threads it into `HostSwitchLink`, fires the slot's action through the existing `ActionDispatcher` → `openlogi-inject` path (with hook environment variables) the moment the keyboard's move is validated, and detects offline→online keyboard arrivals in the orchestrator. PR 2 adds a command channel into the same watcher so a software-initiated switch stops the armed session with a new `Requested(host)` reason and reuses the identical completion path, exposes it over two appended tarpc methods, and wraps them in a `host` CLI subcommand with JSON output.

**Tech Stack:** Rust edition 2024 (MSRV 1.98), tokio, tarpc/bincode IPC, serde/toml, clap 4, `hidpp` (vendored `openlogi-hidpp`), the repo's `cargo xtask ci` gate.

## Global Constraints

Copied from the spec (`docs/superpowers/specs/2026-09-02-ark-desk-switch-design.md`) and the repo's `AGENTS.md` / `.claude/rules/*.md`:

- Work happens in a worktree of `/Volumes/Main External/Development/Logi-Open`, on branches off upstream v0.8.3 (`origin/master`, commit `d5e2b880`). `origin` = AprilNEA/OpenLogi, `fork` = MichaelDanCurtis/OpenLogi. Never touch the existing `feature/function-key-remapper` branch.
- Conventional commits: `type(scope): imperative lowercase description`; scopes used here: `core`, `inject`, `agent`, `hid`, `ipc`, `cli`, `hidpp`, `docs`. No AI attribution anywhere.
- Rust rules: `unsafe_code = "deny"`, `clippy::pedantic` at warn with `-D warnings` in CI, `unwrap_used`/`expect_used` warned outside tests, every lint suppression is `#[expect(..., reason = "…")]`, rustdoc on every public item, files split around ~500 lines, no banner comments.
- Slot numbering: 1-based everywhere a person sees it (config `slot_1..slot_3`, CLI `<SLOT>`, `OPENLOGI_HOST_SLOT`, IPC `slot`); 0-based HID++ host indexes stay internal.
- Config is additive and optional: `schema_version` stays 7; an untouched `config.toml` must serialize byte-for-byte as before.
- IPC is append-only: new methods go at the END of the `Agent` trait, new enum variants at the END, `PROTOCOL_VERSION` 29 → 30, goldens in `crates/openlogi-ipc/tests/wire_format.rs` regenerated, `cargo test -p openlogi-ipc --test wire_format` green before any push.
- Hook environment variables: `OPENLOGI_HOST_SLOT` (1-based, absent for arrive), `OPENLOGI_DEVICE_KEY` (the keyboard's config key), `OPENLOGI_TRIGGER` (`key` | `command` | `arrive`).
- The slot action is fire-and-forget and must never delay or block the device moves. `AlreadyCurrent` (the keyboard already uses the requested slot) fires no action.
- `arrive` fires only for an offline→online (or absent→online) transition observed after the agent's first completed inventory reconcile; never at startup.
- Hook-run shell commands are `sh -c` / `cmd /C` with the extra environment; the `WorkflowStep::RunShellCommand(String)` type is untouched.
- Verification commands (run from the worktree root): `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p <crate>`, and before each PR `cargo xtask ci` (name any job it cannot run on macOS as "not run" in the PR body). macOS-green proves nothing about Linux/Windows cfg code; the inject task re-reads the three platform files against current master.
- Hardware verification is Michael's job; every PR body lists the exact keys to press.

---

## File Structure

**PR 1 — `feat/host-switch-actions`**

| File | Responsibility |
|---|---|
| Create `crates/openlogi-core/src/config/host_switch.rs` | `HostSwitchActions` (config table), `HostSwitchTrigger` + its `hook_env` (pure data, no I/O) |
| Modify `crates/openlogi-core/src/config.rs` | declare the module, re-export the two types |
| Modify `crates/openlogi-core/src/config/device.rs` | `DeviceConfig::host_switch_actions` + raw twin + round-trip test |
| Modify `crates/openlogi-inject/src/inject.rs` | `execute_with_env` public entry; `execute` delegates |
| Modify `crates/openlogi-inject/src/inject/macos.rs`, `linux.rs`, `windows.rs` | thread `env` into script execution |
| Modify `crates/openlogi-device/src/session/host_switch.rs` | `switch_linked_hosts_with(…, on_validated)`; `switch_linked_hosts` delegates |
| Modify `crates/openlogi-device/src/lib.rs` | re-export `switch_linked_hosts_with` |
| Modify `crates/openlogi-agent-core/src/runtime.rs` | `ActionDispatcher::dispatch_hook` |
| Modify `crates/openlogi-agent-core/src/watchers/host_switch.rs` | `HostSwitchLink` gains `keyboard_key` + `actions`; `spawn` takes a dispatcher; the transition fires the slot action |
| Modify `crates/openlogi-agent-core/src/orchestrator.rs` (+ `orchestrator/tests.rs`) | links carry actions; links exist for actions-only keyboards; `HostArrival` edge detection returned by `refresh_inventory` |
| Modify `crates/openlogi-agent/src/startup.rs` | pass the dispatcher to the watcher |
| Modify `crates/openlogi-agent/src/lifecycle.rs` | dispatch `arrive` actions |
| Modify `docs/CONFIGURATION.md` | document the table + env vars |

**PR 2 — `feat/host-cli`** (branched from PR 1's branch until PR 1 merges, then rebased)

| File | Responsibility |
|---|---|
| Modify `crates/openlogi-hidpp/src/feature/hosts_info.rs` | `get_host_friendly_name` (function 3) |
| Create `crates/openlogi-core/src/hid/host.rs` | wire DTOs: `HostSlots`, `HostSlotInfo`, `HostSwitchOutcome`, `HostSwitchCommandError` |
| Modify `crates/openlogi-core/src/hid.rs`, `crates/openlogi-core/src/hid/error.rs` | re-exports; `HidppOperation::ReadHostSlots`, `::SwitchHost` appended |
| Create `crates/openlogi-device/src/write/hosts.rs` | `read_host_slots_on(&SharedChannel)` |
| Modify `crates/openlogi-device/src/write.rs`, `crates/openlogi-device/src/lib.rs` | module + re-exports |
| Modify `crates/openlogi-device/src/session/host_switch.rs` | `HostSwitchStopReason::Requested(u8)`; `HostSwitchError::InvalidHost` |
| Modify `crates/openlogi-ipc/src/ipc.rs`, `crates/openlogi-ipc/tests/wire_format.rs` | two appended methods, version 30, goldens |
| Modify `crates/openlogi-agent-core/src/watchers/host_switch.rs` | `HostSwitchCommand` channel, pending reply, `run_transition` returns the result |
| Modify `crates/openlogi-agent-core/src/orchestrator.rs` | `SharedRuntime::host_switch_commands` sender, receiver handed out once |
| Modify `crates/openlogi-agent/src/pairing/tests.rs` | hand-built `SharedRuntime` literal gains the field |
| Modify `crates/openlogi-agent/src/startup.rs`, `lifecycle.rs`, `server.rs`, `bin/mock_agent.rs` | wire the receiver into the watcher; implement the two RPCs |
| Create `crates/openlogi-cli/src/cmd/host.rs` | `openlogi host list [--json]`, `openlogi host switch <SLOT> [--device NAME]` |
| Modify `crates/openlogi-cli/src/cmd/mod.rs`, `crates/openlogi-cli/src/lib.rs`, `crates/openlogi-cli/Cargo.toml` | register the subcommand, parse tests, `serde`/`serde_json` deps |
| Modify `docs/USAGE.md` | document the commands and exit codes |

---

### Task 0: Sync the fork to upstream v0.8.3 and open a worktree

**Files:**
- No source files. Git state only.

**Interfaces:**
- Produces: worktree `/Volumes/Main External/Development/Logi-Open-hs` on branch `feat/host-switch-actions` at upstream `d5e2b880`; local `master` fast-forwarded.

- [ ] **Step 1: Update the toolchain (upstream MSRV is 1.98; this Mac has 1.96)**

```bash
rustup update stable && rustc --version
```
Expected: `rustc 1.98.x` or newer.

- [ ] **Step 2: Fetch upstream and fast-forward the local master without touching the checked-out branch**

```bash
cd "/Volumes/Main External/Development/Logi-Open"
git fetch origin
git branch -f master origin/master
git log --oneline -1 master
```
Expected: `d5e2b880 perf(i18n): avoid copying borrowed translations (#1184)`.

- [ ] **Step 3: Push the fast-forwarded master to the fork (ask Michael first; it is his GitHub)**

```bash
git push fork master
```
Expected: `fork/master` now equals `origin/master`. If Michael says not now, continue; nothing below depends on it.

- [ ] **Step 4: Create the PR 1 worktree**

```bash
git worktree add "/Volumes/Main External/Development/Logi-Open-hs" -b feat/host-switch-actions origin/master
cd "/Volumes/Main External/Development/Logi-Open-hs"
cargo check -p openlogi-core
```
Expected: `cargo check` finishes without errors (first build takes minutes).

- [ ] **Step 5: Read the repo rules once**

```bash
cat AGENTS.md .claude/rules/rust.md .claude/rules/cross-platform.md crates/openlogi-ipc/AGENTS.md | head -400
```
No output to assert; these are the constraints every task below follows.

---

## PR 1 — host switch actions

### Task 1: `HostSwitchActions` and `HostSwitchTrigger` in openlogi-core

**Files:**
- Create: `crates/openlogi-core/src/config/host_switch.rs`
- Modify: `crates/openlogi-core/src/config.rs` (module declarations near line 13–34)

**Interfaces:**
- Produces:
  - `pub struct HostSwitchActions { pub slot_1: Option<Action>, pub slot_2: Option<Action>, pub slot_3: Option<Action>, pub arrive: Option<Action> }` with `fn for_host_index(&self, index: u8) -> Option<&Action>` and `fn is_empty(&self) -> bool`.
  - `pub enum HostSwitchTrigger { Key, Command, Arrive }` with `fn as_str(self) -> &'static str` and `fn hook_env(self, keyboard_key: &str, host_index: Option<u8>) -> Vec<(String, String)>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/openlogi-core/src/config/host_switch.rs` with only the test module for now:

```rust
//! Actions fired around a keyboard's host switching, plus the environment
//! those actions receive.

#[cfg(test)]
mod tests {
    use super::{HostSwitchActions, HostSwitchTrigger};
    use crate::binding::Action;

    #[test]
    fn slots_are_addressed_by_zero_based_host_index() {
        let actions = HostSwitchActions {
            slot_1: Some(Action::RunShellCommand("one".into())),
            slot_3: Some(Action::RunShellCommand("three".into())),
            ..HostSwitchActions::default()
        };
        assert_eq!(
            actions.for_host_index(0),
            Some(&Action::RunShellCommand("one".into()))
        );
        assert_eq!(actions.for_host_index(1), None);
        assert_eq!(
            actions.for_host_index(2),
            Some(&Action::RunShellCommand("three".into()))
        );
        assert_eq!(actions.for_host_index(3), None, "a fourth slot never exists");
    }

    #[test]
    fn empty_means_no_slot_and_no_arrive() {
        assert!(HostSwitchActions::default().is_empty());
        let arrive_only = HostSwitchActions {
            arrive: Some(Action::RunShellCommand("hi".into())),
            ..HostSwitchActions::default()
        };
        assert!(!arrive_only.is_empty());
    }

    #[test]
    fn round_trips_through_toml_and_omits_absent_entries() -> Result<(), Box<dyn std::error::Error>> {
        let actions: HostSwitchActions = toml::from_str(
            r#"slot_2 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 2" }
arrive = { RunShellCommand = "/usr/local/bin/deskswitch arrive" }"#,
        )?;
        assert_eq!(
            actions.slot_2,
            Some(Action::RunShellCommand(
                "/usr/local/bin/deskswitch switch --slot 2".into()
            ))
        );
        assert_eq!(actions.slot_1, None);
        let serialized = toml::to_string(&actions)?;
        assert!(serialized.contains("slot_2"));
        assert!(!serialized.contains("slot_1"));
        assert!(!serialized.contains("slot_3"));
        Ok(())
    }

    #[test]
    fn unknown_slot_keys_are_rejected() {
        let result: Result<HostSwitchActions, _> =
            toml::from_str(r#"slot_4 = { RunShellCommand = "x" }"#);
        result.expect_err("slot_4 must not be silently accepted");
    }

    #[test]
    fn hook_env_carries_slot_key_and_trigger() {
        let env = HostSwitchTrigger::Key.hook_env("unit:7b520554", Some(1));
        assert_eq!(
            env,
            vec![
                ("OPENLOGI_HOST_SLOT".to_string(), "2".to_string()),
                ("OPENLOGI_DEVICE_KEY".to_string(), "unit:7b520554".to_string()),
                ("OPENLOGI_TRIGGER".to_string(), "key".to_string()),
            ]
        );
    }

    #[test]
    fn arrive_env_has_no_slot() {
        let env = HostSwitchTrigger::Arrive.hook_env("unit:7b520554", None);
        assert!(env.iter().all(|(name, _)| name != "OPENLOGI_HOST_SLOT"));
        assert!(env.contains(&("OPENLOGI_TRIGGER".to_string(), "arrive".to_string())));
    }
}
```

Then in `crates/openlogi-core/src/config.rs`, next to the existing `mod device;` line add `mod host_switch;`, and next to `pub use device::{DeviceConfig, DeviceIdentity, LinkConfig, LinkOverrides};` add:

```rust
pub use host_switch::{HostSwitchActions, HostSwitchTrigger};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p openlogi-core config::host_switch`
Expected: compile error — `HostSwitchActions` and `HostSwitchTrigger` are not defined.

- [ ] **Step 3: Write the implementation**

Insert above the `#[cfg(test)]` module in `crates/openlogi-core/src/config/host_switch.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::binding::Action;

/// Actions fired around a keyboard's host switching.
///
/// `slot_N` fires the instant a switch to Easy-Switch slot `N` is observed —
/// from the keyboard's own host key or from a software-initiated switch — in
/// parallel with the device moves and never delaying them. `arrive` fires when
/// the keyboard transitions offline → online after the agent's first inventory
/// reconcile, as a fallback for a machine that was left without an agent.
///
/// Every field is optional and the whole table is omitted from `config.toml`
/// while empty, so existing files serialize unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSwitchActions {
    /// Fired on a switch to Easy-Switch slot 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_1: Option<Action>,
    /// Fired on a switch to Easy-Switch slot 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_2: Option<Action>,
    /// Fired on a switch to Easy-Switch slot 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_3: Option<Action>,
    /// Fired when this keyboard connects to this host after the agent's first
    /// inventory reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive: Option<Action>,
}

impl HostSwitchActions {
    /// The action for a 0-based HID++ host index (`0` → [`Self::slot_1`]).
    #[must_use]
    pub fn for_host_index(&self, index: u8) -> Option<&Action> {
        match index {
            0 => self.slot_1.as_ref(),
            1 => self.slot_2.as_ref(),
            2 => self.slot_3.as_ref(),
            _ => None,
        }
    }

    /// Whether no slot and no arrive action is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slot_1.is_none() && self.slot_2.is_none() && self.slot_3.is_none() && self.arrive.is_none()
    }
}

/// What caused a host-switch hook to fire, as reported to the hook in
/// `OPENLOGI_TRIGGER`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSwitchTrigger {
    /// The keyboard's own host key was pressed.
    Key,
    /// A software-initiated switch (`openlogi host switch`).
    Command,
    /// The keyboard connected to this host.
    Arrive,
}

impl HostSwitchTrigger {
    /// The `OPENLOGI_TRIGGER` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Command => "command",
            Self::Arrive => "arrive",
        }
    }

    /// The environment a hook action receives: `OPENLOGI_HOST_SLOT` (1-based,
    /// only when `host_index` is known), `OPENLOGI_DEVICE_KEY`, and
    /// `OPENLOGI_TRIGGER`.
    #[must_use]
    pub fn hook_env(self, keyboard_key: &str, host_index: Option<u8>) -> Vec<(String, String)> {
        let mut env = Vec::with_capacity(3);
        if let Some(index) = host_index {
            env.push((
                "OPENLOGI_HOST_SLOT".to_string(),
                (u16::from(index) + 1).to_string(),
            ));
        }
        env.push(("OPENLOGI_DEVICE_KEY".to_string(), keyboard_key.to_string()));
        env.push(("OPENLOGI_TRIGGER".to_string(), self.as_str().to_string()));
        env
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p openlogi-core config::host_switch`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-core/src/config/host_switch.rs crates/openlogi-core/src/config.rs
git commit -m "feat(core): add HostSwitchActions config table and hook trigger env"
```

---

### Task 2: `DeviceConfig::host_switch_actions`

**Files:**
- Modify: `crates/openlogi-core/src/config/device.rs` — public struct (after `host_switch_targets`, before `fn_lock`, around line 268), raw struct (around line 490), `From<RawDeviceConfig>` (around line 551), tests (end of file)

**Interfaces:**
- Consumes: `HostSwitchActions` from Task 1.
- Produces: `DeviceConfig::host_switch_actions: HostSwitchActions`.

- [ ] **Step 1: Write the failing tests**

Append to the existing `mod tests` at the bottom of `crates/openlogi-core/src/config/device.rs`:

```rust
    #[test]
    fn host_switch_actions_round_trip_and_stay_absent_when_empty()
    -> Result<(), Box<dyn std::error::Error>> {
        let config: DeviceConfig = toml::from_str(
            r#"host_switch_targets = ["unit:mouse"]

[host_switch_actions]
slot_2 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 2" }
"#,
        )?;
        assert_eq!(
            config.host_switch_actions.slot_2,
            Some(crate::binding::Action::RunShellCommand(
                "/usr/local/bin/deskswitch switch --slot 2".into()
            ))
        );
        let serialized = toml::to_string(&config)?;
        assert!(serialized.contains("[host_switch_actions]"));

        let plain: DeviceConfig = toml::from_str(r#"host_switch_targets = ["unit:mouse"]"#)?;
        assert!(plain.host_switch_actions.is_empty());
        assert!(
            !toml::to_string(&plain)?.contains("host_switch_actions"),
            "an empty table must not appear in config.toml"
        );
        Ok(())
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p openlogi-core config::device::tests::host_switch_actions`
Expected: compile error — no field `host_switch_actions`.

- [ ] **Step 3: Add the field in three places**

In the `pub struct DeviceConfig` block, directly after the `host_switch_targets` field:

```rust
    /// Actions fired around this keyboard's host switching; see
    /// [`HostSwitchActions`]. Omitted from `config.toml` while empty.
    #[serde(default, skip_serializing_if = "HostSwitchActions::is_empty")]
    pub host_switch_actions: HostSwitchActions,
```

In `struct RawDeviceConfig`, directly after `host_switch_targets: Vec<String>,`:

```rust
    #[serde(default)]
    host_switch_actions: HostSwitchActions,
```

In `impl From<RawDeviceConfig> for DeviceConfig`, directly after `host_switch_targets: raw.host_switch_targets,`:

```rust
            host_switch_actions: raw.host_switch_actions,
```

Add to the `use super::…` imports at the top of the file:

```rust
use super::host_switch::HostSwitchActions;
```

Check whether `DeviceConfig` has any other constructor (`Default` impl or a `new`) that lists every field: run `grep -n "host_switch_targets: Vec::new()" crates/openlogi-core/src/config/device.rs`. For every hit, add `host_switch_actions: HostSwitchActions::default(),` on the following line.

- [ ] **Step 4: Run the core tests**

Run: `cargo test -p openlogi-core`
Expected: all pass, including the new one and the existing `host_switch_targets_round_trip_as_physical_keys`.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-core/src/config/device.rs
git commit -m "feat(core): persist host_switch_actions on keyboard device entries"
```

---

### Task 3: `openlogi-inject` runs script actions with extra environment

**Files:**
- Modify: `crates/openlogi-inject/src/inject.rs` (the `pub fn execute` at ~line 209)
- Modify: `crates/openlogi-inject/src/inject/macos.rs` (`execute` ~line 43, `dispatch_script` ~line 166, `run_*` ~lines 500–537)
- Modify: `crates/openlogi-inject/src/inject/linux.rs` (`execute` ~line 35, `dispatch_script` ~line 172, `run_*` ~lines 194–236)
- Modify: `crates/openlogi-inject/src/inject/windows.rs` (`execute` ~line 56, `dispatch_script` ~line 163, `run_*` ~lines 173–204)

**Interfaces:**
- Produces: `pub fn execute_with_env(action: &Action, env: &[(String, String)])` in `openlogi_inject`. `execute(action)` becomes `execute_with_env(action, &[])`.

No automated test: the crate's `execute` is deliberately outside the test suite (it would need the OS event queue; see its rustdoc). The verification is a compile on all three cfgs plus the manual check in Step 5.

- [ ] **Step 1: Add the public entry in `inject.rs`**

Replace the body of `pub fn execute(action: &Action)` so it reads:

```rust
pub fn execute(action: &Action) {
    execute_with_env(action, &[]);
}

/// Like [`execute`], but script actions (`RunShellCommand`, `RunAppleScript`,
/// `Workflow`) run with `env` added to the child process environment. The
/// agent's host-switch hooks use this to hand a script the slot, device key,
/// and trigger; every other action ignores `env`.
pub fn execute_with_env(action: &Action, env: &[(String, String)]) {
    if let Action::OpenApplication(target) = action {
        let expanded = shellexpand::tilde(target.path());
        if let Err(error) = opener::open(expanded.as_ref()) {
            tracing::warn!(
                %error,
                path = target.path(),
                "could not open configured application, folder, or URL"
            );
        }
        return;
    }

    cfg_select! {
        target_os = "macos" => {
            macos::execute(action, env);
        }
        target_os = "linux" => {
            linux::execute(action, env);
        }
        target_os = "windows" => {
            windows::execute(action, env);
        }
        _ => {
            let _ = env;
            tracing::warn!(
                action = action.label(),
                "execute unsupported on this platform"
            );
        }
    }
}
```

Keep the existing rustdoc above `execute`. Add `execute_with_env` to the `pub use inject::{…}` list in `crates/openlogi-inject/src/lib.rs`.

- [ ] **Step 2: Thread `env` through macOS**

In `crates/openlogi-inject/src/inject/macos.rs`:

Change `pub(super) fn execute(action: &Action) {` to `pub(super) fn execute(action: &Action, env: &[(String, String)]) {` and its `Effect::Script(script) => dispatch_script(script),` arm to `Effect::Script(script) => dispatch_script(script, env),`.

Replace `fn dispatch_script` and the four `run_*` helpers with:

```rust
fn dispatch_script(script: Script<'_>, env: &[(String, String)]) {
    match script {
        Script::AppleScript(src) => run_apple_script_async(src.to_string(), env.to_vec()),
        Script::ShellCommand(cmd) => run_shell_command_async(cmd.to_string(), env.to_vec()),
        Script::Workflow(steps) => run_workflow_async(steps.to_vec(), env.to_vec()),
    }
}

fn run_apple_script_async(src: String, env: Vec<(String, String)>) {
    std::thread::spawn(move || run_apple_script(&src, &env));
}

fn run_shell_command_async(cmd: String, env: Vec<(String, String)>) {
    std::thread::spawn(move || run_shell_command(&cmd, &env));
}

fn run_workflow_async(steps: Vec<WorkflowStep>, env: Vec<(String, String)>) {
    std::thread::spawn(move || run_workflow(&steps, &env));
}

/// Run workflow steps on a worker thread, so `Delay` never stalls the event tap.
fn run_workflow(steps: &[WorkflowStep], env: &[(String, String)]) {
    for step in steps {
        match step {
            WorkflowStep::TypeText(text) => post_unicode(text),
            WorkflowStep::PressKey(combo) => post_keycombo(combo),
            WorkflowStep::Delay { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
            }
            WorkflowStep::RunAppleScript(src) => run_apple_script(src, env),
            WorkflowStep::RunShellCommand(cmd) => run_shell_command(cmd, env),
        }
    }
}

fn run_apple_script(src: &str, env: &[(String, String)]) {
    let _ = std::process::Command::new("osascript")
        .args(["-e", src])
        .envs(env.iter().map(|(name, value)| (name, value)))
        .output();
}

fn run_shell_command(cmd: &str, env: &[(String, String)]) {
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", cmd])
        .envs(env.iter().map(|(name, value)| (name, value)))
        .output();
}
```

- [ ] **Step 3: Thread `env` through Linux and Windows the same way**

`crates/openlogi-inject/src/inject/linux.rs`: same signature change on `execute` and its `Effect::Script` arm; `dispatch_script(script, env)` keeps the existing `AppleScript` warning arm and passes `env.to_vec()` to the shell and workflow helpers; `run_shell_command(cmd: &str, env: &[(String, String)])` uses `/bin/sh -c` with `.envs(env.iter().map(|(name, value)| (name, value)))`; `run_workflow(steps, env)` passes `env` to `run_shell_command` (its `RunAppleScript` arm stays a warning).

`crates/openlogi-inject/src/inject/windows.rs`: identical, except the shell is `std::process::Command::new("cmd").args(["/C", cmd])`.

- [ ] **Step 4: Compile-check every platform this host can reach**

Run: `cargo clippy -p openlogi-inject --all-targets -- -D warnings`
Expected: clean.

Run: `cargo clippy --target aarch64-unknown-linux-musl -p openlogi-inject -- -D warnings`
(If the target is missing: `rustup target add aarch64-unknown-linux-musl` first.) Expected: clean. If a Linux-only name mismatch appears, fix it before continuing — this is the trap `.claude/rules/cross-platform.md` describes.

For Windows, re-read `windows.rs` against `git show origin/master:crates/openlogi-inject/src/inject/windows.rs` and confirm every changed function name and signature matches a caller. CI's Windows job is the authoritative check; say so in the PR.

- [ ] **Step 5: Manual verification hook**

Note for the PR body: bind any mouse button to `{ RunShellCommand = "printenv > /tmp/openlogi-env.txt" }`, press it, and confirm the file exists (proves the plain path still works). The env-carrying path is verified in Task 7.

- [ ] **Step 6: Commit**

```bash
git add crates/openlogi-inject
git commit -m "feat(inject): run script actions with caller-supplied environment"
```

---

### Task 4: `switch_linked_hosts_with` fires a callback once the keyboard's move is validated

**Files:**
- Modify: `crates/openlogi-device/src/session/host_switch.rs` (`switch_linked_hosts`, ~line 178)
- Modify: `crates/openlogi-device/src/lib.rs` (the `pub use session::host_switch::{…}` list, ~line 57)

**Interfaces:**
- Produces: `pub async fn switch_linked_hosts_with(keyboard: &DeviceRoute, targets: &[DeviceRoute], host: u8, channel_pool: &ChannelPool, on_validated: impl FnOnce()) -> Result<bool, HostSwitchError>`. `on_validated` runs exactly once, only when the keyboard's move to `host` is required (not already current) and its slot passed the pairing check, and before any device is moved.

- [ ] **Step 1: Write the failing test**

The file already has a `mod tests` using `HidppChannel` fixtures. Add a pure test that pins the callback contract on the decision helper, which needs no hardware:

```rust
    #[test]
    fn validated_callback_fires_only_when_a_move_is_required() {
        assert!(matches!(super::host_change_required(0, 3, 1), Ok(true)));
        assert!(matches!(super::host_change_required(1, 3, 1), Ok(false)));
        assert!(matches!(
            super::host_change_required(0, 3, 3),
            Err(super::HostSwitchError::Hidpp(_))
        ));
    }
```

(Task 13 tightens the last arm to a typed `InvalidHost` error.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p openlogi-device session::host_switch::tests::validated_callback_fires_only_when_a_move_is_required`
Expected: FAIL — `host_change_required` is private to the module but the test module is inside it, so this compiles; it fails only if the helper's semantics differ. If it passes immediately, that is fine: it documents the contract the next step relies on. Continue.

- [ ] **Step 3: Implement**

Replace `pub async fn switch_linked_hosts` with:

```rust
/// Move reachable targets to `host`, then move the keyboard last.
///
/// Returns whether the keyboard actually changed hosts.
pub async fn switch_linked_hosts(
    keyboard: &DeviceRoute,
    targets: &[DeviceRoute],
    host: u8,
    channel_pool: &ChannelPool,
) -> Result<bool, HostSwitchError> {
    switch_linked_hosts_with(keyboard, targets, host, channel_pool, || {}).await
}

/// [`switch_linked_hosts`], calling `on_validated` once the keyboard's own move
/// has been validated — the slot is inside the host count, paired, and not the
/// current one — and before any device is moved. Hooks that must fire the
/// instant a switch is certain (the agent's `host_switch_actions`) hang off
/// this; a switch that turns out to be a no-op never reaches it.
pub async fn switch_linked_hosts_with(
    keyboard: &DeviceRoute,
    targets: &[DeviceRoute],
    host: u8,
    channel_pool: &ChannelPool,
    on_validated: impl FnOnce(),
) -> Result<bool, HostSwitchError> {
    let channel = open_channel(channel_pool, keyboard, "opening keyboard channel")
        .await?
        .ok_or(HostSwitchError::KeyboardNotFound)?;
    // Validate the keyboard's own move before touching anything: preparation is
    // read-only, but it is the step that rejects an unpaired host slot, and
    // discovering that *after* the mice have moved would strand them on a host
    // the keyboard never reaches. Applying it still happens last, because once
    // the keyboard leaves this host its channel can no longer command a mouse
    // sharing the same receiver.
    let keyboard_change = prepare_host_change_on(&channel, keyboard.device_index(), host).await?;
    if keyboard_change.required {
        on_validated();
    }
    for target in targets {
        match prepare_host_change(target, host, keyboard, &channel, channel_pool).await {
            Ok(change) => {
                if let Err(error) = apply_host_change(change).await {
                    debug!(%error, route = %target, host, "linked device host switch failed");
                }
            }
            Err(error) => {
                debug!(%error, route = %target, host, "linked device host switch preparation failed");
            }
        }
    }
    let changed = apply_host_change(keyboard_change).await?;
    if changed {
        debug!(host, route = %keyboard, "keyboard host switched");
    }
    Ok(changed)
}
```

In `crates/openlogi-device/src/lib.rs`, extend the re-export:

```rust
pub use session::host_switch::{
    HostSwitchError, HostSwitchStopReason, run_host_switch_session, switch_linked_hosts,
    switch_linked_hosts_with,
};
```

- [ ] **Step 4: Run the device tests and clippy**

Run: `cargo test -p openlogi-device && cargo clippy -p openlogi-device --all-targets -- -D warnings`
Expected: all pass, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-device/src/session/host_switch.rs crates/openlogi-device/src/lib.rs
git commit -m "feat(hid): expose a validated-move callback on switch_linked_hosts"
```

---

### Task 5: `ActionDispatcher::dispatch_hook`

**Files:**
- Modify: `crates/openlogi-agent-core/src/runtime.rs` (`impl ActionDispatcher`, ~line 258)

**Interfaces:**
- Consumes: `openlogi_inject::execute_with_env` (Task 3).
- Produces: `pub fn dispatch_hook(&self, action: &Action, device_key: Option<&str>, env: &[(String, String)])`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `runtime.rs`:

```rust
    #[test]
    fn script_actions_are_the_only_env_carrying_kind() {
        use openlogi_core::binding::WorkflowStep;
        assert!(super::carries_hook_env(&Action::RunShellCommand("x".into())));
        assert!(super::carries_hook_env(&Action::RunAppleScript("x".into())));
        assert!(super::carries_hook_env(&Action::Workflow(vec![WorkflowStep::Delay { millis: 1 }])));
        assert!(!super::carries_hook_env(&Action::Copy));
        assert!(!super::carries_hook_env(&Action::ShowActionsRing));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p openlogi-agent-core runtime::tests::script_actions_are_the_only_env_carrying_kind`
Expected: compile error — `carries_hook_env` not found.

- [ ] **Step 3: Implement**

Add inside `impl ActionDispatcher`, directly after `pub fn dispatch`:

```rust
    /// Route one hook-fired action. Script actions run through
    /// [`openlogi_inject::execute_with_env`] so the child process sees `env`
    /// (the host-switch slot, device key, and trigger); every other action is
    /// dispatched exactly as [`Self::dispatch`] would.
    pub fn dispatch_hook(&self, action: &Action, device_key: Option<&str>, env: &[(String, String)]) {
        if carries_hook_env(action) {
            openlogi_inject::execute_with_env(action, env);
        } else {
            self.dispatch(action, device_key);
        }
    }
```

And a free function near `browser_nav_debounce_ok`:

```rust
/// Whether `action` spawns a child process that can receive hook environment.
fn carries_hook_env(action: &Action) -> bool {
    matches!(
        action,
        Action::RunShellCommand(_) | Action::RunAppleScript(_) | Action::Workflow(_)
    )
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p openlogi-agent-core runtime`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-agent-core/src/runtime.rs
git commit -m "feat(agent): dispatch hook actions with environment"
```

---

### Task 6: Links carry the keyboard key and actions; links exist for actions-only keyboards

**Files:**
- Modify: `crates/openlogi-agent-core/src/watchers/host_switch.rs` (`HostSwitchLink`, ~line 22; its two test literals ~lines 350 and 365)
- Modify: `crates/openlogi-agent-core/src/orchestrator.rs` (`host_switch_links`, ~line 1025)
- Modify: `crates/openlogi-agent-core/src/orchestrator/tests.rs` (~line 380)

**Interfaces:**
- Produces: `pub struct HostSwitchLink { pub keyboard_key: String, pub keyboard: DeviceRoute, pub targets: Vec<DeviceRoute>, pub actions: HostSwitchActions }`.

- [ ] **Step 1: Write the failing tests**

In `crates/openlogi-agent-core/src/orchestrator/tests.rs`, after `host_switch_links_keep_sleeping_targets_but_require_online_keyboard`, add:

```rust
#[test]
fn host_switch_links_exist_for_actions_only_keyboards_and_carry_the_actions() {
    let mut config = Config::default();
    config
        .devices
        .entry("keyboard".into())
        .or_default()
        .host_switch_actions
        .slot_2 = Some(Action::RunShellCommand("deskswitch switch --slot 2".into()));
    let devices = [dev("keyboard", 1, true)];

    let links = host_switch_links(&config, &devices);

    assert_eq!(links.len(), 1, "actions alone must arm a divert session");
    assert_eq!(links[0].keyboard_key, "keyboard");
    assert!(links[0].targets.is_empty());
    assert_eq!(
        links[0].actions.for_host_index(1),
        Some(&Action::RunShellCommand("deskswitch switch --slot 2".into()))
    );
}

#[test]
fn host_switch_links_skip_keyboards_with_neither_targets_nor_actions() {
    let mut config = Config::default();
    config.devices.entry("keyboard".into()).or_default();
    let devices = [dev("keyboard", 1, true)];

    assert!(host_switch_links(&config, &devices).is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p openlogi-agent-core orchestrator::tests::host_switch_links`
Expected: compile error — no field `keyboard_key` / `actions`.

- [ ] **Step 3: Implement**

In `watchers/host_switch.rs`, replace the struct:

```rust
/// One resolved link. Config keys are converted to live routes by the
/// orchestrator so the transport watcher never needs to understand inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchLink {
    /// The keyboard's config key, handed to hook actions as
    /// `OPENLOGI_DEVICE_KEY`.
    pub keyboard_key: String,
    /// Keyboard whose host switch keys initiate the transition.
    pub keyboard: DeviceRoute,
    /// Pointing devices that follow the keyboard.
    pub targets: Vec<DeviceRoute>,
    /// Actions fired around the transition.
    pub actions: HostSwitchActions,
}
```

and add `use openlogi_core::config::HostSwitchActions;` to its imports. Update the two test literals in that file to `HostSwitchLink { keyboard_key: "keyboard".into(), keyboard: route(1), targets: vec![route(2)], actions: HostSwitchActions::default() }`.

In `orchestrator.rs`, replace `host_switch_links`:

```rust
fn host_switch_links(config: &Config, devices: &[AgentDevice]) -> Vec<HostSwitchLink> {
    config
        .devices
        .iter()
        .filter_map(|(keyboard_key, settings)| {
            let keyboard = devices
                .iter()
                .find(|device| device.config_key == *keyboard_key && device.online)?
                .route
                .clone()?;
            let targets = settings
                .host_switch_targets
                .iter()
                .filter_map(|target_key| {
                    devices
                        .iter()
                        .find(|device| device.config_key == *target_key)
                        .and_then(|device| device.route.clone())
                })
                .collect::<Vec<_>>();
            // A keyboard with actions but no linked mouse still needs the divert
            // session: the actions are what observe the pressed slot.
            (!targets.is_empty() || !settings.host_switch_actions.is_empty()).then(|| {
                HostSwitchLink {
                    keyboard_key: keyboard_key.clone(),
                    keyboard,
                    targets,
                    actions: settings.host_switch_actions.clone(),
                }
            })
        })
        .collect()
}
```

Add `use openlogi_core::binding::Action;` to the orchestrator tests imports if it is not already there (it is imported at line 10 of `tests.rs`; keep it).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p openlogi-agent-core`
Expected: all pass, including the pre-existing `host_switch_links_keep_sleeping_targets_but_require_online_keyboard` (a keyboard with targets still links).

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-agent-core
git commit -m "feat(agent): carry host switch actions on resolved links"
```

---

### Task 7: Fire the slot action during the transition

**Files:**
- Modify: `crates/openlogi-agent-core/src/watchers/host_switch.rs` (`spawn`, `manage`, `run_transition`)
- Modify: `crates/openlogi-agent/src/startup.rs` (`spawn_hidpp_watchers`, ~line 185)

**Interfaces:**
- Consumes: `ActionDispatcher::dispatch_hook` (Task 5), `switch_linked_hosts_with` (Task 4), `HostSwitchTrigger::hook_env` (Task 1).
- Produces: `pub fn spawn(links: &HostSwitchLinks, channel_pool: ChannelPool, receiver_access: ReceiverAccess, device_io: DeviceIoGate, dispatcher: ActionDispatcher)`; a private `fn fire_slot_action(dispatcher: &ActionDispatcher, link: &HostSwitchLink, host: u8, trigger: HostSwitchTrigger) -> bool` (returns whether an action was configured).

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `watchers/host_switch.rs`:

```rust
    #[test]
    fn slot_action_env_names_the_keyboard_and_one_based_slot() {
        let link = HostSwitchLink {
            keyboard_key: "unit:7b520554".into(),
            keyboard: route(1),
            targets: Vec::new(),
            actions: HostSwitchActions {
                slot_3: Some(openlogi_core::binding::Action::RunShellCommand("x".into())),
                ..HostSwitchActions::default()
            },
        };
        let (action, env) = plan_slot_action(&link, 2, HostSwitchTrigger::Key)
            .expect("slot 3 is configured");
        assert_eq!(action, &openlogi_core::binding::Action::RunShellCommand("x".into()));
        assert!(env.contains(&("OPENLOGI_HOST_SLOT".to_string(), "3".to_string())));
        assert!(env.contains(&("OPENLOGI_DEVICE_KEY".to_string(), "unit:7b520554".to_string())));
        assert!(env.contains(&("OPENLOGI_TRIGGER".to_string(), "key".to_string())));
        assert!(plan_slot_action(&link, 0, HostSwitchTrigger::Key).is_none());
    }
```

Add `use openlogi_core::config::HostSwitchTrigger;` to the test imports (the `use super::*;` covers the rest once Step 3 imports it at module level).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p openlogi-agent-core watchers::host_switch::tests::slot_action_env`
Expected: compile error — `plan_slot_action` not found.

- [ ] **Step 3: Implement**

At the top of `watchers/host_switch.rs` add imports:

```rust
use openlogi_core::binding::Action;
use openlogi_core::config::{HostSwitchActions, HostSwitchTrigger};

use crate::runtime::ActionDispatcher;
```

Change `spawn` to take and forward a dispatcher:

```rust
pub fn spawn(
    links: &HostSwitchLinks,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
    dispatcher: ActionDispatcher,
) {
```

and pass `dispatcher` as the last argument of `manage(...)`; give `manage` a matching `dispatcher: ActionDispatcher` parameter and forward `&dispatcher` into `run_transition`.

Replace `run_transition`:

```rust
async fn run_transition(
    links: &mut watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>,
    channel_pool: &ChannelPool,
    receiver_access: &ReceiverAccess,
    dispatcher: &ActionDispatcher,
    link: HostSwitchLink,
    host: u8,
    trigger: HostSwitchTrigger,
) {
    let _lease = receiver_access
        .acquire_exclusive(ExclusiveAccessReason::HostTransition)
        .await;
    let fire = || {
        if !fire_slot_action(dispatcher, &link, host, trigger) {
            debug!(host, route = %link.keyboard, "no host switch action configured for slot");
        }
    };
    match switch_linked_hosts_with(&link.keyboard, &link.targets, host, channel_pool, fire).await {
        Ok(true) => wait_for_departure(links, &link.keyboard).await,
        Ok(false) => {}
        Err(error) => {
            debug!(%error, route = %link.keyboard, host, "keyboard host switch failed");
        }
    }
}

/// The action and environment for a switch of `link`'s keyboard to 0-based
/// `host`, or `None` when that slot has no action configured.
fn plan_slot_action(
    link: &HostSwitchLink,
    host: u8,
    trigger: HostSwitchTrigger,
) -> Option<(&Action, Vec<(String, String)>)> {
    let action = link.actions.for_host_index(host)?;
    Some((action, trigger.hook_env(&link.keyboard_key, Some(host))))
}

/// Fire the configured slot action, fire-and-forget. Returns whether one was
/// configured. Never awaited by the transition: the dispatcher hands script
/// actions to a worker thread and returns immediately.
fn fire_slot_action(
    dispatcher: &ActionDispatcher,
    link: &HostSwitchLink,
    host: u8,
    trigger: HostSwitchTrigger,
) -> bool {
    let Some((action, env)) = plan_slot_action(link, host, trigger) else {
        return false;
    };
    debug!(host, route = %link.keyboard, trigger = trigger.as_str(), "firing host switch action");
    dispatcher.dispatch_hook(action, Some(&link.keyboard_key), &env);
    true
}
```

Update the import line from `switch_linked_hosts` to `switch_linked_hosts_with` (the plain function is no longer used here). In `manage`, the completion arm becomes:

```rust
                    if let Some((link, host)) = completion.request {
                        if !device_io.wait_until_allowed().await {
                            return;
                        }
                        stop_all(&mut state.sessions, HostSwitchStopReason::Graceful).await;
                        run_transition(
                            &mut links,
                            &channel_pool,
                            &receiver_access,
                            &dispatcher,
                            link,
                            host,
                            HostSwitchTrigger::Key,
                        )
                        .await;
                    }
```

In `crates/openlogi-agent/src/startup.rs`, `spawn_hidpp_watchers`:

```rust
    watchers::host_switch::spawn(
        &shared.host_switch_links,
        shared.channel_pool.clone(),
        shared.receiver_access.clone(),
        shared.device_io.clone(),
        inputs.dispatcher.clone(),
    );
```

- [ ] **Step 4: Run tests and clippy for the two crates**

Run: `cargo test -p openlogi-agent-core -p openlogi-agent && cargo clippy -p openlogi-agent-core -p openlogi-agent --all-targets -- -D warnings`
Expected: pass, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-agent-core/src/watchers/host_switch.rs crates/openlogi-agent/src/startup.rs
git commit -m "feat(agent): fire configured slot actions when a host switch is validated"
```

---

### Task 8: `arrive` — offline→online keyboard edges after the first reconcile

**Files:**
- Modify: `crates/openlogi-agent-core/src/orchestrator.rs` (`refresh_inventory`, ~line 479; new `HostArrival` + `host_arrivals`)
- Modify: `crates/openlogi-agent-core/src/orchestrator/tests.rs`
- Modify: `crates/openlogi-agent/src/lifecycle.rs` (`apply_inventory`, ~line 342)

**Interfaces:**
- Produces: `pub struct HostArrival { pub keyboard_key: String, pub action: Action }`; `Orchestrator::refresh_inventory(...) -> Vec<HostArrival>`.

- [ ] **Step 1: Write the failing tests**

In `orchestrator/tests.rs` add:

```rust
fn arrive_config(key: &str) -> Config {
    let mut config = Config::default();
    config
        .devices
        .entry(key.into())
        .or_default()
        .host_switch_actions
        .arrive = Some(Action::RunShellCommand("deskswitch arrive".into()));
    config
}

#[test]
fn host_arrivals_fire_on_offline_to_online_and_absent_to_online_edges() {
    let config = arrive_config("keyboard");
    let woke = host_arrivals(&config, &[dev("keyboard", 1, false)], &[dev("keyboard", 1, true)]);
    assert_eq!(woke.len(), 1);
    assert_eq!(woke[0].keyboard_key, "keyboard");
    assert_eq!(woke[0].action, Action::RunShellCommand("deskswitch arrive".into()));

    let appeared = host_arrivals(&config, &[], &[dev("keyboard", 1, true)]);
    assert_eq!(appeared.len(), 1, "a BLE keyboard is absent, not offline, while away");
}

#[test]
fn host_arrivals_ignore_steady_state_offline_and_unconfigured_devices() {
    let config = arrive_config("keyboard");
    assert!(host_arrivals(&config, &[dev("keyboard", 1, true)], &[dev("keyboard", 1, true)]).is_empty());
    assert!(host_arrivals(&config, &[dev("keyboard", 1, true)], &[dev("keyboard", 1, false)]).is_empty());
    assert!(host_arrivals(&config, &[], &[dev("mouse", 2, true)]).is_empty());
}

#[test]
fn refresh_inventory_never_reports_arrivals_before_the_first_reconcile() {
    let mut orch = orchestrator(arrive_config("keyboard"));
    let first = orch.refresh_inventory(&[], &[], false);
    assert!(first.is_empty(), "the very first reconcile is a baseline, not an edge");
}
```

Import `host_arrivals` in the `use super::{…}` list at the top of `tests.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p openlogi-agent-core orchestrator::tests::host_arrivals orchestrator::tests::refresh_inventory_never`
Expected: compile errors — `host_arrivals` / `HostArrival` missing, `refresh_inventory` returns `()`.

- [ ] **Step 3: Implement**

In `orchestrator.rs`, above `struct AgentDevice`, add:

```rust
/// A keyboard that just connected to this host and has an `arrive` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostArrival {
    /// The keyboard's config key (`OPENLOGI_DEVICE_KEY`).
    pub keyboard_key: String,
    /// The configured `arrive` action.
    pub action: Action,
}
```

(Import `openlogi_core::binding::Action` if the file does not already; it does at the `binding.click_action()` call — check the `use` block.)

Change `refresh_inventory`'s signature to `-> Vec<HostArrival>` and its body: capture the baseline flag first, compute arrivals against the previous device set, and return them on both exits.

```rust
    pub fn refresh_inventory(
        &mut self,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        hid_open_failures: bool,
    ) -> Vec<HostArrival> {
        // An `arrive` edge needs a previous completed snapshot to compare
        // against; the first reconcile is a baseline, never an edge.
        let had_baseline = matches!(self.inventory, InventoryState::Ready { .. });
        self.hid_open_failures = hid_open_failures;
        self.inventory = InventoryState::Ready {
            inventories: inventories.to_vec(),
            standalone: standalone.to_vec(),
        };
        self.publish_inventory();
        let devices = build_devices(&self.config, inventories, standalone);
        let arrivals = if had_baseline {
            host_arrivals(&self.config, &self.devices, &devices)
        } else {
            Vec::new()
        };
```

…keep the existing body unchanged from `let reapply_all = …` onward, except that the early `return;` inside `if !changed { … }` becomes `return arrivals;` and the final line of the function becomes `arrivals`.

Add the pure helper next to `host_switch_links`:

```rust
/// Keyboards with an `arrive` action that went from offline or absent in
/// `prev` to online in `next`.
fn host_arrivals(config: &Config, prev: &[AgentDevice], next: &[AgentDevice]) -> Vec<HostArrival> {
    next.iter()
        .filter(|device| device.online)
        .filter_map(|device| {
            let action = config
                .devices
                .get(&device.config_key)?
                .host_switch_actions
                .arrive
                .clone()?;
            let was_online = prev
                .iter()
                .any(|before| before.config_key == device.config_key && before.online);
            (!was_online).then(|| HostArrival {
                keyboard_key: device.config_key.clone(),
                action,
            })
        })
        .collect()
}
```

In `crates/openlogi-agent/src/lifecycle.rs`, `apply_inventory`'s `Snapshot` arm becomes:

```rust
            InventoryEvent::Snapshot {
                inventories,
                standalone,
                hid_open_failures,
            } => {
                let mut orchestrator = self.orchestrator.lock().await;
                let arrivals =
                    orchestrator.refresh_inventory(&inventories, &standalone, hid_open_failures);
                let confirm_settings = orchestrator.needs_reapply_confirmation();
                drop(orchestrator);
                for arrival in arrivals {
                    let env = HostSwitchTrigger::Arrive.hook_env(&arrival.keyboard_key, None);
                    info!(keyboard = %arrival.keyboard_key, "keyboard arrived — firing arrive action");
                    self.inputs
                        .dispatcher
                        .dispatch_hook(&arrival.action, Some(&arrival.keyboard_key), &env);
                }
                if confirm_settings {
                    refresh.request_settings_confirmation();
                }
            }
```

with `use openlogi_core::config::HostSwitchTrigger;` added to the file's imports. Every other caller of `refresh_inventory` (grep `refresh_inventory(` across `crates/`) keeps compiling because the return value can be ignored; leave them as they are.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p openlogi-agent-core -p openlogi-agent && cargo clippy -p openlogi-agent-core -p openlogi-agent --all-targets -- -D warnings`
Expected: pass, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-agent-core crates/openlogi-agent/src/lifecycle.rs
git commit -m "feat(agent): fire the arrive action on keyboard reconnect edges"
```

---

### Task 9: Document the table and run the PR 1 gate

**Files:**
- Modify: `docs/CONFIGURATION.md` (the device-field bullet at line 81; a new short section before `## Actions`)

- [ ] **Step 1: Update the docs**

Change the bullet `- \`host_switch_targets\` and \`fn_lock\` for compatible keyboards` to:

```markdown
- `host_switch_targets`, `host_switch_actions`, and `fn_lock` for compatible
  keyboards
```

Insert before `## Actions`:

```markdown
## Host switch actions

A keyboard entry may fire actions around Easy-Switch host changes:

```toml
[devices."<keyboard key>".host_switch_actions]
slot_1 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 1" }
slot_2 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 2" }
slot_3 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 3" }
arrive = { RunShellCommand = "/usr/local/bin/deskswitch arrive" }
```

`slot_N` fires the instant a switch to slot `N` is observed — from the
keyboard's host key or from `openlogi host switch N` — concurrently with the
device moves, never delaying them, and only when the keyboard actually changes
host. `arrive` fires when the keyboard connects to this machine after the agent
has completed its first inventory pass; it never fires at agent startup.

Script actions receive `OPENLOGI_HOST_SLOT` (1–3; absent for `arrive`),
`OPENLOGI_DEVICE_KEY` (the keyboard's config key), and `OPENLOGI_TRIGGER`
(`key`, `command`, or `arrive`). The agent runs hooks with a minimal `PATH`, so
use absolute binary paths.
```

- [ ] **Step 2: Run the full local gate**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --all-targets`
Expected: clean and green. Then `cargo xtask ci --list` and run every job the host can (`cargo xtask ci`); note the ones it skips.

- [ ] **Step 3: Commit and push the branch to the fork**

```bash
git add docs/CONFIGURATION.md
git commit -m "docs: describe host_switch_actions and hook environment"
git push -u fork feat/host-switch-actions
```

- [ ] **Step 4: Draft the PR body for Michael to post upstream**

Write to `/Users/michaelcurtis/Samsun Arc 2 intergrations/docs/pr-drafts/openlogi-pr1.md`:

```markdown
## Summary
Host-switch actions: a keyboard's `host_switch_actions` table fires a configured action the instant a switch to a slot is validated (from the host key or a software switch), plus an `arrive` action when the keyboard reconnects. Lets external tools follow the keyboard — e.g. flip a monitor input — without OpenLogi knowing anything about monitors.

## Changes
- core: `HostSwitchActions` (`slot_1..3`, `arrive`), `HostSwitchTrigger::hook_env`; `DeviceConfig::host_switch_actions` (optional, omitted while empty, no schema bump)
- inject: `execute_with_env` — script actions get caller environment (`OPENLOGI_HOST_SLOT`, `OPENLOGI_DEVICE_KEY`, `OPENLOGI_TRIGGER`)
- hid: `switch_linked_hosts_with(…, on_validated)` — callback once the keyboard's move is validated and required
- agent: `ActionDispatcher::dispatch_hook`; links carry actions and exist for actions-only keyboards; the transition fires the slot action; `refresh_inventory` reports `arrive` edges after the first reconcile
- docs: CONFIGURATION.md

## Testing
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` on macOS arm64
- `cargo clippy --target aarch64-unknown-linux-musl -p openlogi-inject` (Linux cfg)
- Not run locally: Windows clippy, typos, MSRV matrix — relying on CI
- Hardware (MX Keys S + MX Master 3S, Bluetooth-direct, macOS): configured `slot_2 = { RunShellCommand = "printenv > /tmp/hs.txt" }`, pressed Easy-Switch 2 → `/tmp/hs.txt` contains `OPENLOGI_HOST_SLOT=2`, `OPENLOGI_TRIGGER=key`; the mouse followed; pressing the current slot's key fires nothing. `arrive` fired on reconnect and not on agent restart.
```

(Michael fills in the hardware lines only after doing them.)

---

## PR 2 — `openlogi host` over IPC

### Task 10: Open the PR 2 branch

- [ ] **Step 1: Branch from PR 1's head**

```bash
cd "/Volumes/Main External/Development/Logi-Open-hs"
git checkout -b feat/host-cli
```

If PR 1 has merged upstream by the time this starts, instead run `git fetch origin && git checkout -b feat/host-cli origin/master`.

---

### Task 11: `HostsInfoFeature::get_host_friendly_name`

**Files:**
- Modify: `crates/openlogi-hidpp/src/feature/hosts_info.rs` (append to `impl HostsInfoFeature`; new struct near `HostInfo`)

**Interfaces:**
- Produces: `pub struct HostFriendlyNameChunk { pub host_index: HostIndex, pub byte_index: u8, pub bytes: [u8; 14] }`; `pub async fn get_host_friendly_name(&self, host: HostIndex, byte_index: u8) -> Result<HostFriendlyNameChunk, Hidpp20Error>`.

The vendored `hidpp` crate has no in-process device fixture for feature calls; its existing feature methods carry no unit tests either. The verification is Task 12's decode test plus hardware in Task 20.

- [ ] **Step 1: Add the type**

Below `pub struct HostInfo { … }` add:

```rust
/// One chunk of a host slot's friendly name, as returned by
/// `getHostFriendlyName` (function 3): up to 14 bytes starting at `byte_index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HostFriendlyNameChunk {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// Offset of `bytes[0]` within the full name.
    pub byte_index: u8,
    /// Raw UTF-8 bytes; only the first `name_len - byte_index` are meaningful.
    pub bytes: [u8; 14],
}
```

- [ ] **Step 2: Add the method**

Inside `impl HostsInfoFeature`, after `get_host_descriptor`:

```rust
    /// Retrieves the friendly-name bytes of `host` starting at `byte_index`.
    /// Callers loop until they have [`HostInfo::name_len`] bytes.
    pub async fn get_host_friendly_name(
        &self,
        host: HostIndex,
        byte_index: u8,
    ) -> Result<HostFriendlyNameChunk, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(3, [u8::from(host), byte_index, 0])
            .await?
            .extend_payload();
        let mut bytes = [0; 14];
        bytes.copy_from_slice(&payload[2..16]);
        Ok(HostFriendlyNameChunk {
            host_index: HostIndex::from(payload[0]),
            byte_index: payload[1],
            bytes,
        })
    }
```

- [ ] **Step 3: Build and lint**

Run: `cargo clippy -p openlogi-hidpp --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/openlogi-hidpp/src/feature/hosts_info.rs
git commit -m "feat(hidpp): read host friendly names from HostsInfo"
```

---

### Task 12: Wire DTOs in openlogi-core and `read_host_slots_on` in openlogi-device

**Files:**
- Create: `crates/openlogi-core/src/hid/host.rs`
- Modify: `crates/openlogi-core/src/hid.rs` (module list and re-exports; follow the existing `route`/`dpi` lines)
- Modify: `crates/openlogi-core/src/hid/error.rs` (`HidppOperation`, append two variants at the END of the enum)
- Create: `crates/openlogi-device/src/write/hosts.rs`
- Modify: `crates/openlogi-device/src/write.rs` (`mod hosts;` + `pub use hosts::{read_host_slots, read_host_slots_on};`)
- Modify: `crates/openlogi-device/src/lib.rs` (add the two names to the `pub use write::{…}` list)

**Interfaces:**
- Produces (core, all `Serialize + Deserialize + Debug + Clone + PartialEq + Eq`):
  - `pub struct HostSlotInfo { pub slot: u8, pub paired: Option<bool>, pub name: Option<String> }` (1-based `slot`)
  - `pub struct HostSlots { pub current: u8, pub slots: Vec<HostSlotInfo> }` (1-based `current`)
  - `pub enum HostSwitchOutcome { Switched, AlreadyCurrent }`
  - `pub enum HostSwitchCommandError { KeyboardNotFound, InvalidSlot, UnpairedSlot, Busy, Io(String) }` (`thiserror`, matches core's error style)
  - `HidppOperation::ReadHostSlots`, `HidppOperation::SwitchHost`
- Produces (device): `pub async fn read_host_slots_on(shared: &SharedChannel) -> Result<HostSlots, WriteError>`; `pub async fn read_host_slots(backend: &dyn HidBackend, route: &DeviceRoute) -> Result<HostSlots, WriteError>`; pure `fn assemble_name(name_len: u8, chunks: impl Iterator<Item = [u8; 14]>) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

`crates/openlogi-core/src/hid/host.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::{HostSlotInfo, HostSlots, HostSwitchCommandError, HostSwitchOutcome};

    #[test]
    fn slots_serialize_with_one_based_numbers() -> Result<(), serde_json::Error> {
        let slots = HostSlots {
            current: 1,
            slots: vec![HostSlotInfo {
                slot: 1,
                paired: Some(true),
                name: Some("Michael’s MacBook Pro".into()),
            }],
        };
        let json = serde_json::to_string(&slots)?;
        assert!(json.contains("\"current\":1"));
        assert!(json.contains("\"slot\":1"));
        Ok(())
    }

    #[test]
    fn errors_render_for_humans() {
        assert_eq!(
            HostSwitchCommandError::UnpairedSlot.to_string(),
            "the requested host slot is not paired on the keyboard"
        );
        assert_eq!(HostSwitchOutcome::AlreadyCurrent, HostSwitchOutcome::AlreadyCurrent);
    }
}
```

`crates/openlogi-device/src/write/hosts.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::assemble_name;

    fn chunk(text: &str) -> [u8; 14] {
        let mut bytes = [0; 14];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        bytes
    }

    #[test]
    fn names_span_chunks_and_stop_at_name_len() {
        let name = assemble_name(20, [chunk("Michael’s Mac"), chunk("Book Pro-junk")].into_iter());
        assert_eq!(name.as_deref(), Some("Michael’s MacBook Pr"));
    }

    #[test]
    fn empty_and_invalid_names_are_none() {
        assert_eq!(assemble_name(0, std::iter::empty()), None);
        assert_eq!(assemble_name(3, [[0xff, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]].into_iter()), None);
    }
}
```

(`serde_json` is already a dev-dependency wherever core tests use it; if `cargo test -p openlogi-core` complains, add `serde_json = { workspace = true }` under `[dev-dependencies]` in `crates/openlogi-core/Cargo.toml`.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p openlogi-core hid::host && cargo test -p openlogi-device write::hosts`
Expected: compile errors for the missing items.

- [ ] **Step 3: Implement the core DTOs**

`crates/openlogi-core/src/hid/host.rs`:

```rust
//! Wire DTOs for Easy-Switch host slots and software-initiated host switches.
//! These cross the agent↔client IPC boundary: append-only, never reorder.

use serde::{Deserialize, Serialize};

/// One Easy-Switch slot as the keyboard reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSlotInfo {
    /// 1-based slot number, matching the key cap.
    pub slot: u8,
    /// Whether the slot has a pairing; `None` when the device did not say.
    pub paired: Option<bool>,
    /// The host's friendly name as stored on the device, when readable.
    pub name: Option<String>,
}

/// The keyboard's host slots and which one it is talking to now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSlots {
    /// 1-based slot number the device currently uses.
    pub current: u8,
    /// Every slot the device has, in slot order.
    pub slots: Vec<HostSlotInfo>,
}

/// What a software-initiated host switch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostSwitchOutcome {
    /// The keyboard (and its linked devices) moved to the requested slot.
    Switched,
    /// The keyboard already used the requested slot; nothing moved and no
    /// hook action fired.
    AlreadyCurrent,
}

/// Why a software-initiated host switch was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum HostSwitchCommandError {
    /// No online keyboard matched the route.
    #[error("no connected keyboard matched the route")]
    KeyboardNotFound,
    /// The slot is outside the device's host count.
    #[error("the requested host slot does not exist on the keyboard")]
    InvalidSlot,
    /// The device reports the slot as empty; switching would strand it.
    #[error("the requested host slot is not paired on the keyboard")]
    UnpairedSlot,
    /// Pairing or another switch holds exclusive access, or the agent is
    /// dormant.
    #[error("the agent is busy; retry shortly")]
    Busy,
    /// Transport or protocol failure, as text.
    #[error("host switch failed: {0}")]
    Io(String),
}
```

In `crates/openlogi-core/src/hid.rs`, add `mod host;` beside the other `mod` lines and `pub use host::{HostSlotInfo, HostSlots, HostSwitchCommandError, HostSwitchOutcome};` beside the other `pub use` lines. Confirm `thiserror` is a dependency of `openlogi-core` (it is — `WriteError` derives it).

In `crates/openlogi-core/src/hid/error.rs`, append to the END of `pub enum HidppOperation` (after its current last variant, never in the middle):

```rust
    /// Read HostsInfo slots and names.
    ReadHostSlots,
    /// Software-initiated ChangeHost.
    SwitchHost,
```

- [ ] **Step 4: Implement the device read**

`crates/openlogi-device/src/write/hosts.rs`:

```rust
//! Easy-Switch host slots: HostsInfo (`0x1815`) slot status and friendly
//! names, read on an open channel.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::hosts_info::{HostIndex, HostSlotStatus, HostsInfoFeature},
};
use openlogi_core::hid::{HostSlotInfo, HostSlots};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

const FEATURE: u16 = 0x1815;

/// Read the keyboard's host slots at `route`.
pub async fn read_host_slots(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<HostSlots, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        read_host_slots_on_channel(&channel, index).await
    })
    .await
}

/// Read host slots on an already-open [`SharedChannel`].
pub async fn read_host_slots_on(shared: &SharedChannel) -> Result<HostSlots, WriteError> {
    read_host_slots_on_channel(shared.channel(), shared.device_index()).await
}

async fn read_host_slots_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<HostSlots, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let hosts = open_feature::<HostsInfoFeature>(&mut device).await?;
    let info = hosts
        .get_feature_info()
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ReadHostSlots, FEATURE))?;
    let current = match info.current_host {
        HostIndex::Slot(index) => index.saturating_add(1),
        HostIndex::Current => 0,
    };
    let mut slots = Vec::with_capacity(usize::from(info.host_count));
    for host in 0..info.host_count {
        // A slot whose status is unreadable is reported, not fatal: the
        // switch path treats an unknown status as "allowed", and so does this.
        let (paired, name) = match hosts.get_host_info(HostIndex::Slot(host)).await {
            Ok(slot) => {
                let paired = Some(slot.status == HostSlotStatus::Paired);
                let name = read_name(&hosts, host, slot.name_len).await;
                (paired, name)
            }
            Err(error) => {
                debug!(host, %error, "host slot status unreadable");
                (None, None)
            }
        };
        slots.push(HostSlotInfo {
            slot: host.saturating_add(1),
            paired,
            name,
        });
    }
    Ok(HostSlots { current, slots })
}

async fn read_name(hosts: &HostsInfoFeature, host: u8, name_len: u8) -> Option<String> {
    let mut chunks = Vec::new();
    let mut offset: u8 = 0;
    while offset < name_len {
        match hosts.get_host_friendly_name(HostIndex::Slot(host), offset).await {
            Ok(chunk) => chunks.push(chunk.bytes),
            Err(error) => {
                debug!(host, offset, %error, "host name chunk unreadable");
                return None;
            }
        }
        offset = offset.saturating_add(14);
    }
    assemble_name(name_len, chunks.into_iter())
}

/// Join 14-byte name chunks, keep exactly `name_len` bytes, and require valid
/// UTF-8. `None` for an empty or undecodable name.
fn assemble_name(name_len: u8, chunks: impl Iterator<Item = [u8; 14]>) -> Option<String> {
    if name_len == 0 {
        return None;
    }
    let bytes: Vec<u8> = chunks.flatten().take(usize::from(name_len)).collect();
    String::from_utf8(bytes).ok().filter(|name| !name.is_empty())
}
```

Register it: in `crates/openlogi-device/src/write.rs` add `mod hosts;` in the module list and `pub use hosts::{read_host_slots, read_host_slots_on};` among the re-exports; in `crates/openlogi-device/src/lib.rs` add `read_host_slots, read_host_slots_on` to the `pub use write::{…}` list (keep the list alphabetical if it is).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p openlogi-core hid::host && cargo test -p openlogi-device write::hosts && cargo clippy -p openlogi-core -p openlogi-device --all-targets -- -D warnings`
Expected: pass, clean.

- [ ] **Step 6: Commit**

```bash
git add crates/openlogi-core/src/hid.rs crates/openlogi-core/src/hid/host.rs crates/openlogi-core/src/hid/error.rs crates/openlogi-device/src/write.rs crates/openlogi-device/src/write/hosts.rs crates/openlogi-device/src/lib.rs
git commit -m "feat(hid): read Easy-Switch host slots and names"
```

---

### Task 13: `HostSwitchStopReason::Requested` and a typed invalid-host error

**Files:**
- Modify: `crates/openlogi-device/src/session/host_switch.rs` (`HostSwitchStopReason` ~line 36, `HostSwitchError` ~line 71, `run_host_switch_session` select ~line 160, `host_change_required` ~line 471, the test from Task 4)

**Interfaces:**
- Produces: `HostSwitchStopReason::Requested(u8)` (0-based host); `HostSwitchError::InvalidHost { host: u8, host_count: u8 }`.

- [ ] **Step 1: Update the failing test**

Change the Task 4 test's last assertion to:

```rust
        assert!(matches!(
            super::host_change_required(0, 3, 3),
            Err(super::HostSwitchError::InvalidHost { host: 3, host_count: 3 })
        ));
```

Run: `cargo test -p openlogi-device session::host_switch::tests::validated_callback`
Expected: compile error — no variant `InvalidHost`.

- [ ] **Step 2: Implement**

Add to `HostSwitchStopReason`:

```rust
    /// Software asked for a switch to this 0-based host: the session restores
    /// the diverted controls and completes exactly as if that host key had
    /// been pressed.
    Requested(u8),
```

Add to `HostSwitchError` (after `HostSlotEmpty`):

```rust
    /// The requested host slot is outside the device's host count.
    #[error("host {host} is outside the device's {host_count} host slots")]
    InvalidHost {
        /// The zero-based host slot that was requested.
        host: u8,
        /// How many host slots the device reports.
        host_count: u8,
    },
```

Replace `host_change_required`'s error arm:

```rust
    if requested_host >= host_count {
        return Err(HostSwitchError::InvalidHost {
            host: requested_host,
            host_count,
        });
    }
```

In `run_host_switch_session`, replace the `shutdown` select arm:

```rust
        reason = shutdown => match reason {
            Ok(HostSwitchStopReason::Requested(host)) => (Some(host), true),
            Ok(HostSwitchStopReason::Graceful) => (None, true),
            Ok(HostSwitchStopReason::DeviceLost) | Err(_) => (None, false),
        },
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p openlogi-device && cargo clippy -p openlogi-device --all-targets -- -D warnings`
Expected: pass, clean.

- [ ] **Step 4: Commit**

```bash
git add crates/openlogi-device/src/session/host_switch.rs
git commit -m "feat(hid): let software stop a host switch session with a requested host"
```

---

### Task 14: Append `host_slots` and `switch_host` to the IPC contract

**Files:**
- Modify: `crates/openlogi-ipc/src/ipc.rs` (imports ~line 19, version doc + `PROTOCOL_VERSION` ~line 64, the END of `trait Agent`)
- Modify: `crates/openlogi-ipc/tests/wire_format.rs` (`protocol_version_is_pinned`, `request_variant_order`, a new DTO test)

**Interfaces:**
- Produces: `async fn host_slots(route: DeviceRoute) -> Result<HostSlots, WriteError>;` and `async fn switch_host(keyboard: DeviceRoute, slot: u8) -> Result<HostSwitchOutcome, HostSwitchCommandError>;` as the last two methods; `PROTOCOL_VERSION == 30`.

- [ ] **Step 1: Write the failing goldens**

In `wire_format.rs`, change `protocol_version_is_pinned` to assert `30`. Append to `request_variant_order`:

```rust
    assert_wire(
        &AgentRequest::HostSlots {
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb37c,
            },
        },
        "",
    );
    assert_wire(
        &AgentRequest::SwitchHost {
            keyboard: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb37c,
            },
            slot: 2,
        },
        "",
    );
```

Add a new test:

```rust
#[test]
fn host_slot_types() {
    assert_wire(
        &HostSlots {
            current: 1,
            slots: vec![HostSlotInfo {
                slot: 1,
                paired: Some(true),
                name: Some("Mac".into()),
            }],
        },
        "",
    );
    assert_wire(&Ok::<HostSwitchOutcome, HostSwitchCommandError>(HostSwitchOutcome::Switched), "");
    assert_wire(
        &Err::<HostSwitchOutcome, HostSwitchCommandError>(HostSwitchCommandError::Io("x".into())),
        "",
    );
}
```

Import `HostSlotInfo, HostSlots, HostSwitchCommandError, HostSwitchOutcome` from `openlogi_core::hid` in the test's `use` block.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p openlogi-ipc --test wire_format`
Expected: compile error — no `AgentRequest::HostSlots`.

- [ ] **Step 3: Implement the contract**

In `ipc.rs`, extend the `openlogi_core::hid::{…}` import with `HostSlots, HostSwitchCommandError, HostSwitchOutcome`. Add to the version doc comment:

```rust
/// v30: `host_slots` + `switch_host` appended — Easy-Switch slot reads and
///      software-initiated host switches for `openlogi host`.
pub const PROTOCOL_VERSION: u32 = 30;
```

Append at the END of `pub trait Agent` (after `declare_client`):

```rust
    /// Read the Easy-Switch host slots of the keyboard at `route`: current
    /// slot, per-slot pairing status, and friendly names.
    async fn host_slots(route: DeviceRoute) -> Result<HostSlots, WriteError>;
    /// Move `keyboard` and its linked devices to 1-based `slot`, firing the
    /// keyboard's configured slot action. Completes after the switch is
    /// applied or refused; the caller should budget about ten seconds.
    async fn switch_host(
        keyboard: DeviceRoute,
        slot: u8,
    ) -> Result<HostSwitchOutcome, HostSwitchCommandError>;
```

- [ ] **Step 4: Regenerate the goldens**

Run: `cargo test -p openlogi-ipc --test wire_format 2>&1 | grep -E "left|right|panicked" | head`
Each failing `assert_wire` prints the actual hex as `left`. Paste each into its `""` slot, then rerun:

Run: `cargo test -p openlogi-ipc --test wire_format`
Expected: all pass. (The agent and mock agent do not compile yet; that is Task 16.)

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-ipc
git commit -m "feat(ipc): append host_slots and switch_host (protocol v30)"
```

---

### Task 15: Command channel into the host-switch watcher

**Files:**
- Modify: `crates/openlogi-agent-core/src/watchers/host_switch.rs`
- Modify: `crates/openlogi-agent-core/src/orchestrator.rs` (`SharedRuntime` ~line 73, `Orchestrator` fields ~line 145, `new` ~line 213)
- Modify: `crates/openlogi-agent/src/pairing/tests.rs` (`shared_runtime()` literal ~line 16)

**Interfaces:**
- Produces:
  - `pub struct HostSwitchCommand { pub keyboard: DeviceRoute, pub host: u8 /* 0-based */, pub reply: oneshot::Sender<Result<HostSwitchOutcome, HostSwitchCommandError>> }`
  - `pub type HostSwitchCommands = mpsc::UnboundedSender<HostSwitchCommand>;`
  - `SharedRuntime::host_switch_commands: HostSwitchCommands`
  - `Orchestrator::take_host_switch_commands(&mut self) -> Option<mpsc::UnboundedReceiver<HostSwitchCommand>>` (Some exactly once)
  - `spawn(links, channel_pool, receiver_access, device_io, dispatcher, commands: mpsc::UnboundedReceiver<HostSwitchCommand>)`
  - pure `fn outcome_of(result: &Result<bool, HostSwitchError>) -> Result<HostSwitchOutcome, HostSwitchCommandError>`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `watchers/host_switch.rs`:

```rust
    #[test]
    fn switch_results_map_to_wire_outcomes() {
        use openlogi_core::hid::{HostSwitchCommandError, HostSwitchOutcome};
        use openlogi_hid::HostSwitchError;
        assert_eq!(outcome_of(&Ok(true)), Ok(HostSwitchOutcome::Switched));
        assert_eq!(outcome_of(&Ok(false)), Ok(HostSwitchOutcome::AlreadyCurrent));
        assert_eq!(
            outcome_of(&Err(HostSwitchError::KeyboardNotFound)),
            Err(HostSwitchCommandError::KeyboardNotFound)
        );
        assert_eq!(
            outcome_of(&Err(HostSwitchError::HostSlotEmpty { host: 1 })),
            Err(HostSwitchCommandError::UnpairedSlot)
        );
        assert_eq!(
            outcome_of(&Err(HostSwitchError::InvalidHost { host: 3, host_count: 3 })),
            Err(HostSwitchCommandError::InvalidSlot)
        );
        assert!(matches!(
            outcome_of(&Err(HostSwitchError::UnsupportedKeyboard)),
            Err(HostSwitchCommandError::Io(_))
        ));
    }

    #[tokio::test]
    async fn a_command_with_no_armed_session_and_no_keyboard_is_not_found() {
        use openlogi_core::hid::HostSwitchCommandError;
        let (links_tx, links) = watch::channel(std::sync::Arc::new(Vec::new()));
        let _keep = links_tx;
        let (reply_tx, reply_rx) = oneshot::channel();
        let mut state = HostSwitchManagerState::new();
        let (_, dispatcher) = test_dispatcher();
        let mut links = links;
        state
            .handle_command(
                HostSwitchCommand {
                    keyboard: route(9),
                    host: 1,
                    reply: reply_tx,
                },
                &mut links,
                &openlogi_hid::host::channel_pool(),
                &ReceiverAccess::default(),
                &openlogi_hid::device_io_channel().1,
                &dispatcher,
            )
            .await;
        assert_eq!(
            reply_rx.await.expect("a reply is always sent"),
            Err(HostSwitchCommandError::KeyboardNotFound)
        );
    }
```

`test_dispatcher()` needs an `ActionRuntime`: add this helper to the test module, mirroring how `ActionRuntime::new` is called in `startup.rs`:

```rust
    fn test_dispatcher() -> (crate::runtime::ActionRuntime, ActionDispatcher) {
        let (ring, _triggers) = mpsc::unbounded_channel();
        let runtime = crate::runtime::ActionRuntime::new(
            std::sync::Arc::new(std::sync::RwLock::new(crate::DpiCycles::default())),
            std::sync::Arc::new(std::sync::RwLock::new(None)),
            openlogi_hid::ChannelRegistry::default(),
            ReceiverAccess::default(),
            openlogi_hid::device_io_channel().1,
            ring,
        )
        .expect("button worker starts");
        let dispatcher = runtime.dispatcher();
        (runtime, dispatcher)
    }
```

Note: `openlogi_hid::device_io_channel().1` is what `pairing/tests.rs` uses for a test `DeviceIoGate`; the gate defaults to allowing I/O. If `handle_command` sees `allows_io() == false` in this fixture, the reply would be `Busy`: the test asserts `KeyboardNotFound`, so the gate must allow I/O — check `device_io_channel`'s default in `crates/openlogi-device/src/device_io.rs` and, if it starts closed, call its signal's "allow" method in the helper.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p openlogi-agent-core watchers::host_switch`
Expected: compile errors — `outcome_of`, `HostSwitchCommand`, `handle_command` missing.

- [ ] **Step 3: Implement the watcher side**

Imports to add in `watchers/host_switch.rs`:

```rust
use openlogi_core::hid::{HostSwitchCommandError, HostSwitchOutcome};
use openlogi_hid::HostSwitchError;
```

Types:

```rust
/// A software-initiated host switch, answered once applied or refused.
pub struct HostSwitchCommand {
    /// The keyboard to move.
    pub keyboard: DeviceRoute,
    /// 0-based HID++ host index to move to.
    pub host: u8,
    /// Where the outcome goes; dropped by a caller that stopped waiting.
    pub reply: oneshot::Sender<Result<HostSwitchOutcome, HostSwitchCommandError>>,
}

/// Sender half handed to the IPC server through `SharedRuntime`.
pub type HostSwitchCommands = mpsc::UnboundedSender<HostSwitchCommand>;

struct PendingCommand {
    keyboard: DeviceRoute,
    reply: oneshot::Sender<Result<HostSwitchOutcome, HostSwitchCommandError>>,
}
```

Make `RunningSession.stop` an `Option<oneshot::Sender<HostSwitchStopReason>>`; `spawn_session` wraps it in `Some`; `stop_all` and `stop_unwanted` become `if let Some(stop) = stop { let _ = stop.send(reason); }` (destructure `stop` as before). Add `pending: Option<PendingCommand>` to `HostSwitchManagerState` (initialised `None`).

`spawn` and `manage` each gain `commands: mpsc::UnboundedReceiver<HostSwitchCommand>` after `dispatcher`; `manage` binds it `mut commands`. Add this arm to the `tokio::select!` in `manage`:

```rust
            Some(command) = commands.recv() => {
                state
                    .handle_command(
                        command,
                        &mut links,
                        &channel_pool,
                        &receiver_access,
                        &device_io,
                        &dispatcher,
                    )
                    .await;
                reconcile = true;
            }
```

Add to `impl HostSwitchManagerState`:

```rust
    /// Apply one software-initiated switch. An armed session for the keyboard
    /// is stopped with `Requested`, so the switch runs through the very same
    /// completion path as a key press and replies from there; a keyboard with
    /// no armed session is switched directly and answered here.
    async fn handle_command(
        &mut self,
        command: HostSwitchCommand,
        links: &mut watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>,
        channel_pool: &ChannelPool,
        receiver_access: &ReceiverAccess,
        device_io: &DeviceIoGate,
        dispatcher: &ActionDispatcher,
    ) {
        if !device_io.allows_io() || self.pending.is_some() {
            let _ = command.reply.send(Err(HostSwitchCommandError::Busy));
            return;
        }
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.link.keyboard == command.keyboard)
        {
            match session.stop.take() {
                Some(stop) => {
                    let _ = stop.send(HostSwitchStopReason::Requested(command.host));
                    self.pending = Some(PendingCommand {
                        keyboard: command.keyboard,
                        reply: command.reply,
                    });
                }
                None => {
                    let _ = command.reply.send(Err(HostSwitchCommandError::Busy));
                }
            }
            return;
        }
        let link = links
            .borrow()
            .iter()
            .find(|link| link.keyboard == command.keyboard)
            .cloned()
            .unwrap_or_else(|| HostSwitchLink {
                keyboard_key: String::new(),
                keyboard: command.keyboard.clone(),
                targets: Vec::new(),
                actions: HostSwitchActions::default(),
            });
        run_transition(
            links,
            channel_pool,
            receiver_access,
            dispatcher,
            link,
            command.host,
            HostSwitchTrigger::Command,
            Some(command.reply),
        )
        .await;
    }
```

Change `run_transition` to take `reply: Option<oneshot::Sender<Result<HostSwitchOutcome, HostSwitchCommandError>>>` as its last parameter and answer it right after the switch returns, before waiting for departure:

```rust
    let result =
        switch_linked_hosts_with(&link.keyboard, &link.targets, host, channel_pool, fire).await;
    if let Some(reply) = reply {
        let _ = reply.send(outcome_of(&result));
    }
    match result {
        Ok(true) => wait_for_departure(links, &link.keyboard).await,
        Ok(false) => {}
        Err(error) => {
            debug!(%error, route = %link.keyboard, host, "keyboard host switch failed");
        }
    }
```

In the completion arm of `manage`, decide the trigger and hand over the pending reply:

```rust
                    if let Some((link, host)) = completion.request {
                        if !device_io.wait_until_allowed().await {
                            return;
                        }
                        stop_all(&mut state.sessions, HostSwitchStopReason::Graceful).await;
                        let pending = state
                            .pending
                            .take_if(|pending| pending.keyboard == link.keyboard);
                        let trigger = if pending.is_some() {
                            HostSwitchTrigger::Command
                        } else {
                            HostSwitchTrigger::Key
                        };
                        run_transition(
                            &mut links,
                            &channel_pool,
                            &receiver_access,
                            &dispatcher,
                            link,
                            host,
                            trigger,
                            pending.map(|pending| pending.reply),
                        )
                        .await;
                    } else {
                        if let Some(pending) = state
                            .pending
                            .take_if(|pending| pending.keyboard == completed.link.keyboard)
                        {
                            let _ = pending.reply.send(Err(HostSwitchCommandError::Io(
                                "host switch session ended before the switch was applied".into(),
                            )));
                        }
                        if device_io.allows_io() {
                            state.restart_after.push((completed.link, Instant::now() + RETRY_DELAY));
                        }
                    }
```

(`Option::take_if` is stable since Rust 1.80.) Add the mapping helper:

```rust
/// Translate a switch result into the wire outcome.
fn outcome_of(
    result: &Result<bool, HostSwitchError>,
) -> Result<HostSwitchOutcome, HostSwitchCommandError> {
    match result {
        Ok(true) => Ok(HostSwitchOutcome::Switched),
        Ok(false) => Ok(HostSwitchOutcome::AlreadyCurrent),
        Err(HostSwitchError::KeyboardNotFound) => Err(HostSwitchCommandError::KeyboardNotFound),
        Err(HostSwitchError::HostSlotEmpty { .. }) => Err(HostSwitchCommandError::UnpairedSlot),
        Err(HostSwitchError::InvalidHost { .. }) => Err(HostSwitchCommandError::InvalidSlot),
        Err(error) => Err(HostSwitchCommandError::Io(error.to_string())),
    }
}
```

- [ ] **Step 4: Implement the orchestrator side**

In `orchestrator.rs`: import `crate::watchers::host_switch::{HostSwitchCommand, HostSwitchCommands, HostSwitchLink, HostSwitchLinks}`. Add to `SharedRuntime`:

```rust
    /// Software-initiated host switches, consumed by the host-switch watcher.
    /// The one command capability on this otherwise read-only handle: the IPC
    /// server enqueues, the watcher applies and replies.
    pub host_switch_commands: HostSwitchCommands,
```

Add to `Orchestrator`:

```rust
    /// The receiving half of [`SharedRuntime::host_switch_commands`], handed
    /// to the watcher exactly once at arming.
    host_switch_command_rx: Option<mpsc::UnboundedReceiver<HostSwitchCommand>>,
```

In `Orchestrator::new`, before building `shared`: `let (host_switch_commands, host_switch_command_rx) = mpsc::unbounded_channel();`, put `host_switch_commands` in the `SharedRuntime` literal and `host_switch_command_rx: Some(host_switch_command_rx)` in the `Self` literal. Add the method:

```rust
    /// Take the host-switch command receiver for the watcher. `None` after the
    /// first call.
    pub fn take_host_switch_commands(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<HostSwitchCommand>> {
        self.host_switch_command_rx.take()
    }
```

(`tokio::sync::mpsc` is already imported as `mpsc` in this file? If not, add `use tokio::sync::mpsc;`.)

In `crates/openlogi-agent/src/pairing/tests.rs`, add to the `SharedRuntime` literal:

```rust
        host_switch_commands: tokio::sync::mpsc::unbounded_channel().0,
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p openlogi-agent-core && cargo clippy -p openlogi-agent-core --all-targets -- -D warnings`
Expected: pass (including the two new tests), clean. `openlogi-agent` still fails to compile until Task 16; that is expected here.

- [ ] **Step 6: Commit**

```bash
git add crates/openlogi-agent-core crates/openlogi-agent/src/pairing/tests.rs
git commit -m "feat(agent): accept software-initiated host switch commands in the watcher"
```

---

### Task 16: Agent wiring: watcher receiver, IPC handlers, mock agent

**Files:**
- Modify: `crates/openlogi-agent/src/startup.rs` (`spawn_hidpp_watchers`)
- Modify: `crates/openlogi-agent/src/lifecycle.rs` (`Armed::run`, ~line 257)
- Modify: `crates/openlogi-agent/src/server.rs` (imports; two handlers at the end of `impl Agent for AgentServer`)
- Modify: `crates/openlogi-agent/src/bin/mock_agent.rs` (two handlers)

**Interfaces:**
- Consumes: `take_host_switch_commands`, `HostSwitchCommand`, `read_host_slots_on`, `HidppOperation::ReadHostSlots`.

- [ ] **Step 1: Thread the receiver at arming**

`startup.rs`:

```rust
/// Start the HID++ background sessions that do not need Accessibility.
pub(crate) fn spawn_hidpp_watchers(
    shared: &SharedRuntime,
    inputs: &InputServices,
    host_switch_commands: Option<tokio::sync::mpsc::UnboundedReceiver<HostSwitchCommand>>,
) {
```

with `use openlogi_agent_core::watchers::host_switch::HostSwitchCommand;` imported, and the host-switch spawn becoming:

```rust
    match host_switch_commands {
        Some(commands) => watchers::host_switch::spawn(
            &shared.host_switch_links,
            shared.channel_pool.clone(),
            shared.receiver_access.clone(),
            shared.device_io.clone(),
            inputs.dispatcher.clone(),
            commands,
        ),
        None => warn!("host switch watcher not started: command receiver already taken"),
    }
```

`lifecycle.rs`, in `Armed::run` before the existing `spawn_hidpp_watchers` call:

```rust
        let host_switch_commands = running
            .orchestrator
            .lock()
            .await
            .take_host_switch_commands();
        startup::spawn_hidpp_watchers(&running.shared, &running.inputs, host_switch_commands);
```

- [ ] **Step 2: Implement the two handlers in `server.rs`**

Extend imports: `use openlogi_core::hid::{HostSlots, HostSwitchCommandError, HostSwitchOutcome};` and `use openlogi_agent_core::watchers::host_switch::HostSwitchCommand;` and `use tokio::sync::oneshot;`. Append inside `impl Agent for AgentServer`:

```rust
    async fn host_slots(self, _: Context, route: DeviceRoute) -> Result<HostSlots, WriteError> {
        self.shared
            .device(&route)
            .run(HidppOperation::ReadHostSlots, |c| async move {
                openlogi_hid::read_host_slots_on(&c).await
            })
            .await
    }

    async fn switch_host(
        self,
        _: Context,
        keyboard: DeviceRoute,
        slot: u8,
    ) -> Result<HostSwitchOutcome, HostSwitchCommandError> {
        // The wire carries the 1-based slot people see; the watcher speaks
        // HID++ host indexes.
        let host = slot
            .checked_sub(1)
            .ok_or(HostSwitchCommandError::InvalidSlot)?;
        let (reply, outcome) = oneshot::channel();
        self.shared
            .host_switch_commands
            .send(HostSwitchCommand {
                keyboard,
                host,
                reply,
            })
            .map_err(|_| HostSwitchCommandError::Busy)?;
        match tokio::time::timeout(Duration::from_secs(20), outcome).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(HostSwitchCommandError::Io(
                "the host switch watcher dropped the request".into(),
            )),
            Err(_) => Err(HostSwitchCommandError::Io(
                "the host switch did not complete within 20 seconds".into(),
            )),
        }
    }
```

A dormant agent (never armed) has no watcher draining the channel, so the send succeeds and the 20 s timeout is what answers; the CLI's own timeout in Task 17 is shorter, which is fine.

- [ ] **Step 3: Implement the mock**

In `bin/mock_agent.rs`, import the same three core types and append inside `impl Agent for MockAgent`:

```rust
    async fn host_slots(self, _: Context, route: DeviceRoute) -> Result<HostSlots, WriteError> {
        let state = self.state.lock().await;
        state.settings_for(&route)?;
        Ok(HostSlots {
            current: 1,
            slots: vec![
                HostSlotInfo { slot: 1, paired: Some(true), name: Some("Mock Mac".into()) },
                HostSlotInfo { slot: 2, paired: Some(true), name: Some("Mock PC".into()) },
                HostSlotInfo { slot: 3, paired: Some(false), name: None },
            ],
        })
    }

    async fn switch_host(
        self,
        _: Context,
        keyboard: DeviceRoute,
        slot: u8,
    ) -> Result<HostSwitchOutcome, HostSwitchCommandError> {
        let state = self.state.lock().await;
        state
            .settings_for(&keyboard)
            .map_err(|_| HostSwitchCommandError::KeyboardNotFound)?;
        match slot {
            1 => Ok(HostSwitchOutcome::AlreadyCurrent),
            2 => Ok(HostSwitchOutcome::Switched),
            3 => Err(HostSwitchCommandError::UnpairedSlot),
            _ => Err(HostSwitchCommandError::InvalidSlot),
        }
    }
```

(`settings_for` is the mock's existing route lookup returning `WriteError::DeviceNotFound` for unknown routes — see its `read_dpi`.) Import `HostSlotInfo` too.

- [ ] **Step 4: Build, lint, test the agent crate**

Run: `cargo test -p openlogi-agent && cargo clippy -p openlogi-agent --all-targets -- -D warnings`
Expected: pass, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/openlogi-agent
git commit -m "feat(agent): serve host_slots and switch_host over IPC"
```

---

### Task 17: `openlogi host list` and `openlogi host switch`

**Files:**
- Create: `crates/openlogi-cli/src/cmd/host.rs`
- Modify: `crates/openlogi-cli/src/cmd/mod.rs` (module + variant + dispatch)
- Modify: `crates/openlogi-cli/Cargo.toml` (`serde = { workspace = true }`, `serde_json = { workspace = true }`)
- Modify: `crates/openlogi-cli/src/lib.rs` (parse tests)

**Interfaces:**
- Produces: `openlogi host list [--json] [--device NAME]`, `openlogi host switch <SLOT> [--device NAME]`; exit codes 0 switched/already current, 2 refused (unpaired or invalid slot), 3 keyboard not found, 4 agent unavailable, 1 anything else. JSON shape:

```json
{"keyboards":[{"name":"MX KEYS S MAC","route":"direct 046d:b37c","online":true,"current_slot":1,"slots":[{"slot":1,"paired":true,"name":"Michael’s MacBook Pro"}]}]}
```

- [ ] **Step 1: Write the failing parse tests**

Append to `mod tests` in `crates/openlogi-cli/src/lib.rs`:

```rust
    #[test]
    fn host_switch_takes_a_one_based_slot_and_optional_device() {
        let cli = Cli::try_parse_from(["openlogi", "host", "switch", "2", "--device", "MX KEYS"])
            .expect("host switch parses");
        match cli.cmd.expect("subcommand present") {
            Command::Host(cmd::host::HostCmd::Switch(args)) => {
                assert_eq!(args.slot, 2);
                assert_eq!(args.device.as_deref(), Some("MX KEYS"));
            }
            other => panic!("expected Host(Switch), got {other:?}"),
        }
    }

    #[test]
    fn host_switch_rejects_slots_outside_one_to_three() {
        Cli::try_parse_from(["openlogi", "host", "switch", "0"]).expect_err("slot 0 rejected");
        Cli::try_parse_from(["openlogi", "host", "switch", "4"]).expect_err("slot 4 rejected");
    }

    #[test]
    fn host_list_accepts_json() {
        let cli = Cli::try_parse_from(["openlogi", "host", "list", "--json"]).expect("parses");
        assert!(matches!(
            cli.cmd,
            Some(Command::Host(cmd::host::HostCmd::List(cmd::host::HostListArgs { json: true, .. })))
        ));
    }
```

And a unit test for the pure JSON assembly, inside `host.rs`'s own `mod tests`:

```rust
#[cfg(test)]
mod tests {
    use openlogi_core::hid::{HostSlotInfo, HostSlots};

    use super::{KeyboardReport, exit_code_for};

    #[test]
    fn report_serializes_the_documented_shape() -> Result<(), serde_json::Error> {
        let report = KeyboardReport::from_slots(
            "MX KEYS S MAC".into(),
            "direct 046d:b37c".into(),
            true,
            Some(HostSlots {
                current: 1,
                slots: vec![HostSlotInfo { slot: 1, paired: Some(true), name: Some("Mac".into()) }],
            }),
            None,
        );
        let json = serde_json::to_value(&report)?;
        assert_eq!(json["current_slot"], 1);
        assert_eq!(json["slots"][0]["slot"], 1);
        assert_eq!(json["slots"][0]["paired"], true);
        Ok(())
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        use openlogi_core::hid::{HostSwitchCommandError as E, HostSwitchOutcome as O};
        assert_eq!(exit_code_for(&Ok(O::Switched)), 0);
        assert_eq!(exit_code_for(&Ok(O::AlreadyCurrent)), 0);
        assert_eq!(exit_code_for(&Err(E::UnpairedSlot)), 2);
        assert_eq!(exit_code_for(&Err(E::InvalidSlot)), 2);
        assert_eq!(exit_code_for(&Err(E::KeyboardNotFound)), 3);
        assert_eq!(exit_code_for(&Err(E::Busy)), 1);
        assert_eq!(exit_code_for(&Err(E::Io("x".into()))), 1);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p openlogi-cli`
Expected: compile errors — `cmd::host` missing.

- [ ] **Step 3: Implement the command**

`crates/openlogi-cli/Cargo.toml`: add `serde = { workspace = true }` and `serde_json = { workspace = true }` under `[dependencies]` (both are workspace deps already).

`crates/openlogi-cli/src/cmd/mod.rs`: add `pub mod host;`, the variant

```rust
    /// Inspect and switch a keyboard's Easy-Switch host slots.
    #[command(subcommand)]
    Host(host::HostCmd),
```

and the dispatch arm `Self::Host(cmd) => return cmd.run().await,` (it owns its exit code like `list`).

`crates/openlogi-cli/src/cmd/host.rs`:

```rust
//! `openlogi host` — Easy-Switch host slots over the running agent.
//!
//! `list` reads slot status and names; `switch` moves the keyboard (and its
//! linked devices) to a slot and fires the keyboard's configured slot action.
//! Both need the agent: the agent owns the divert session, and a direct write
//! from this process would race it.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_core::device::DeviceKind;
use openlogi_core::hid::{DeviceRoute, HostSlotInfo, HostSlots, HostSwitchCommandError, HostSwitchOutcome};
use openlogi_ipc::{AgentClient, AgentSnapshot, ClientKind, PROTOCOL_VERSION, client};
use serde::Serialize;
use tarpc::context;

/// Exit status when the agent cannot be reached or speaks another protocol.
const AGENT_UNAVAILABLE: u8 = 4;
/// Exit status when no online keyboard matched.
const KEYBOARD_NOT_FOUND: u8 = 3;
/// Exit status when the agent refused the slot.
const REFUSED: u8 = 2;

#[derive(Debug, Subcommand)]
pub enum HostCmd {
    /// Show every online keyboard's host slots.
    List(HostListArgs),
    /// Move the keyboard and its linked devices to a slot (1–3).
    Switch(HostSwitchArgs),
}

#[derive(Debug, Args)]
pub struct HostListArgs {
    /// Print JSON instead of a table.
    #[arg(long)]
    pub json: bool,
    /// Only keyboards whose name contains this text (case-insensitive).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

#[derive(Debug, Args)]
pub struct HostSwitchArgs {
    /// Easy-Switch slot, as printed on the key cap.
    #[arg(value_parser = clap::value_parser!(u8).range(1..=3))]
    pub slot: u8,
    /// Pick the keyboard whose name contains this text (case-insensitive)
    /// instead of requiring exactly one online keyboard.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

impl HostCmd {
    /// Run the subcommand and report the process exit status.
    pub async fn run(self) -> Result<ExitCode> {
        match self {
            Self::List(args) => list(args).await,
            Self::Switch(args) => switch(args).await,
        }
    }
}

/// One keyboard in `host list --json`.
#[derive(Debug, Serialize)]
pub(crate) struct KeyboardReport {
    pub name: String,
    pub route: String,
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_slot: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slots: Option<Vec<HostSlotInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl KeyboardReport {
    fn from_slots(name: String, route: String, online: bool, slots: Option<HostSlots>, error: Option<String>) -> Self {
        let (current_slot, slots) = match slots {
            Some(slots) => (Some(slots.current), Some(slots.slots)),
            None => (None, None),
        };
        Self { name, route, online, current_slot, slots, error }
    }
}

#[derive(Debug, Serialize)]
struct ListReport {
    keyboards: Vec<KeyboardReport>,
}

struct Keyboard {
    name: String,
    route: DeviceRoute,
    online: bool,
}

async fn list(args: HostListArgs) -> Result<ExitCode> {
    let Some((agent, snapshot)) = connect().await else {
        return Ok(ExitCode::from(AGENT_UNAVAILABLE));
    };
    let mut keyboards = Vec::new();
    for keyboard in keyboards_in(&snapshot, args.device.as_deref()) {
        let (slots, error) = if keyboard.online {
            match tokio::time::timeout(Duration::from_secs(10), agent.host_slots(context::current(), keyboard.route.clone())).await {
                Ok(Ok(Ok(slots))) => (Some(slots), None),
                Ok(Ok(Err(error))) => (None, Some(error.to_string())),
                Ok(Err(error)) => (None, Some(error.to_string())),
                Err(_) => (None, Some("timed out reading host slots".into())),
            }
        } else {
            (None, Some("offline".into()))
        };
        keyboards.push(KeyboardReport::from_slots(
            keyboard.name,
            keyboard.route.to_string(),
            keyboard.online,
            slots,
            error,
        ));
    }
    if args.json {
        println!("{}", serde_json::to_string(&ListReport { keyboards })?);
        return Ok(ExitCode::SUCCESS);
    }
    if keyboards.is_empty() {
        println!("No keyboards found.");
        return Ok(ExitCode::from(KEYBOARD_NOT_FOUND));
    }
    for keyboard in &keyboards {
        println!("{} ({})", keyboard.name, keyboard.route);
        match (&keyboard.slots, &keyboard.error) {
            (Some(slots), _) => {
                for slot in slots {
                    let marker = if Some(slot.slot) == keyboard.current_slot { "●" } else { "○" };
                    let paired = match slot.paired {
                        Some(true) => "paired",
                        Some(false) => "empty",
                        None => "unknown",
                    };
                    println!("  {marker} slot {}: {paired} {}", slot.slot, slot.name.as_deref().unwrap_or("—"));
                }
            }
            (None, Some(error)) => println!("  {error}"),
            (None, None) => {}
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn switch(args: HostSwitchArgs) -> Result<ExitCode> {
    let Some((agent, snapshot)) = connect().await else {
        return Ok(ExitCode::from(AGENT_UNAVAILABLE));
    };
    let candidates: Vec<Keyboard> = keyboards_in(&snapshot, args.device.as_deref())
        .into_iter()
        .filter(|keyboard| keyboard.online)
        .collect();
    let keyboard = match candidates.as_slice() {
        [keyboard] => keyboard,
        [] => {
            eprintln!("no online keyboard matched");
            return Ok(ExitCode::from(KEYBOARD_NOT_FOUND));
        }
        many => {
            let names: Vec<&str> = many.iter().map(|keyboard| keyboard.name.as_str()).collect();
            return Err(anyhow!(
                "several online keyboards match ({}); pick one with --device",
                names.join(", ")
            ));
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        agent.switch_host(context::current(), keyboard.route.clone(), args.slot),
    )
    .await
    .context("the agent did not answer the host switch within 15 seconds")?
    .context("host switch RPC failed")?;
    match &result {
        Ok(HostSwitchOutcome::Switched) => println!("{} → slot {}", keyboard.name, args.slot),
        Ok(HostSwitchOutcome::AlreadyCurrent) => println!("{} already on slot {}", keyboard.name, args.slot),
        Err(error) => eprintln!("{error}"),
    }
    Ok(ExitCode::from(exit_code_for(&result)))
}

/// The process exit status for a switch result.
pub(crate) fn exit_code_for(result: &Result<HostSwitchOutcome, HostSwitchCommandError>) -> u8 {
    match result {
        Ok(_) => 0,
        Err(HostSwitchCommandError::UnpairedSlot | HostSwitchCommandError::InvalidSlot) => REFUSED,
        Err(HostSwitchCommandError::KeyboardNotFound) => KEYBOARD_NOT_FOUND,
        Err(HostSwitchCommandError::Busy | HostSwitchCommandError::Io(_)) => 1,
    }
}

/// Keyboards in the agent's snapshot, optionally filtered by name.
fn keyboards_in(snapshot: &AgentSnapshot, filter: Option<&str>) -> Vec<Keyboard> {
    let needle = filter.map(str::to_lowercase);
    let mut keyboards = Vec::new();
    for inventory in &snapshot.inventory {
        for paired in &inventory.paired {
            if paired.kind != DeviceKind::Keyboard {
                continue;
            }
            let Some(route) = DeviceRoute::device_route_for(inventory, paired.slot) else {
                continue;
            };
            let name = paired
                .codename
                .clone()
                .unwrap_or_else(|| inventory.receiver.name.clone());
            if needle
                .as_deref()
                .is_some_and(|needle| !name.to_lowercase().contains(needle))
            {
                continue;
            }
            keyboards.push(Keyboard {
                name,
                route,
                online: paired.online,
            });
        }
    }
    keyboards
}

/// Connect, check the protocol, declare as a CLI, and fetch one snapshot.
/// `None` (with a message on stderr) when no compatible agent answers.
async fn connect() -> Option<(AgentClient, AgentSnapshot)> {
    let conn = match tokio::time::timeout(Duration::from_secs(2), client::connect()).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(error)) => {
            eprintln!("no agent reachable ({error}); `openlogi host` needs the running agent");
            return None;
        }
        Err(_) => {
            eprintln!("the agent did not answer the handshake");
            return None;
        }
    };
    if conn.version != PROTOCOL_VERSION {
        eprintln!(
            "the agent speaks protocol v{}, this CLI expects v{PROTOCOL_VERSION}",
            conn.version
        );
        return None;
    }
    tokio::time::timeout(
        Duration::from_secs(2),
        conn.client.declare_client(context::current(), ClientKind::Cli),
    )
    .await
    .ok()?
    .ok()?;
    let snapshot = tokio::time::timeout(Duration::from_secs(5), conn.client.snapshot(context::current()))
        .await
        .ok()?
        .ok()?;
    Some((conn.client, snapshot))
}
```

If `paired.kind`'s `DeviceKind` does not derive `PartialEq`, use `matches!(paired.kind, DeviceKind::Keyboard)` instead. `AgentClient` is generated by tarpc in `openlogi_ipc`; if it is not re-exported under that name, import it via `openlogi_ipc::AgentClient` as `client::Connection.client`'s type (check `crates/openlogi-ipc/src/client.rs`, which names it).

- [ ] **Step 4: Run the tests and lint**

Run: `cargo test -p openlogi-cli && cargo clippy -p openlogi-cli --all-targets -- -D warnings`
Expected: pass, clean.

- [ ] **Step 5: Exercise against the mock agent**

Run: `cargo run -p openlogi-agent --bin mock_agent` in one terminal (see `crates/openlogi-agent/src/bin/mock_agent.rs` header for its socket setup), then:

```bash
cargo run -p openlogi -- host list --json
cargo run -p openlogi -- host switch 2; echo "exit $?"
cargo run -p openlogi -- host switch 3; echo "exit $?"
```
Expected: JSON with the mock's three slots; `exit 0` for slot 2 (`Switched`); `exit 2` for slot 3 (`UnpairedSlot`). If the mock's inventory has no `Keyboard`-kind device, `host switch` exits 3: add a mock keyboard entry to the mock's scripted inventory (its `render_inventory`) in the same commit and note it in the PR.

- [ ] **Step 6: Commit**

```bash
git add crates/openlogi-cli crates/openlogi-agent/src/bin/mock_agent.rs
git commit -m "feat(cli): add openlogi host list and switch"
```

---

### Task 18: Document `openlogi host` and run the PR 2 gate

**Files:**
- Modify: `docs/USAGE.md`

- [ ] **Step 1: Update the docs**

Add to the command block in `docs/USAGE.md`:

```sh
openlogi host list            # each keyboard's Easy-Switch slots: current, paired, host names
openlogi host list --json     # the same, machine-readable
openlogi host switch 2        # move the keyboard (and linked mice) to slot 2
```

And a paragraph after the block:

```markdown
`openlogi host` talks to the running agent (exit status 4 when none answers).
`switch` fires the keyboard's `host_switch_actions` for that slot exactly as a
key press would, exits 0 when switched or already there, 2 when the slot is
unpaired or invalid, and 3 when no online keyboard matched. With more than one
online keyboard, pass `--device <name>`.
```

- [ ] **Step 2: Run the full gate**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --all-targets && cargo test -p openlogi-ipc --test wire_format && cargo xtask ci`
Expected: green on everything the host can run; list what it skipped.

- [ ] **Step 3: Commit and push**

```bash
git add docs/USAGE.md
git commit -m "docs: document openlogi host list and switch"
git push -u fork feat/host-cli
```

- [ ] **Step 4: Draft the PR body**

Write `/Users/michaelcurtis/Samsun Arc 2 intergrations/docs/pr-drafts/openlogi-pr2.md` following the PR 1 template: Summary (software-initiated host switches over IPC + `openlogi host`), Changes per crate (`hidpp`, `core`, `hid`, `ipc` with "protocol v30, goldens regenerated", `agent`, `cli`, `docs`), Testing (commands run, "Windows clippy / typos / MSRV not run locally, relying on CI"), and the hardware lines from Task 20.

---

### Task 19: Install the branch build on the Mac

**Files:** none in the repo.

- [ ] **Step 1: Build and launch the dev bundle from the PR 2 worktree**

Quit the installed OpenLogi 0.6.18 first (the singleton lock refuses a second instance), then:

```bash
cargo run -p openlogi-desktop --release
```

The Cargo runner wraps the build into `target/dev/OpenLogi.app` via `cargo xtask macos dev-bundle`; it shows as **OpenLogi Dev** in the menu bar and uses its own config at `~/.config/openlogi-dev/config.toml` (that is where Task 20's edits go). For the permanent install after verification, build the DMG with `cargo xtask macos dmg` (`cargo xtask macos --help` lists both), install it over 0.6.18, and move the verified `host_switch_*` tables into `~/.config/openlogi/config.toml`.

- [ ] **Step 2: Grant permissions and confirm both devices**

Launch the new app, grant Input Monitoring and Accessibility to the new agent when prompted, then:

```bash
./target/release/openlogi list
./target/release/openlogi host list
```
Expected: MX KEYS S MAC and MX Master 3S listed; `host list` shows three slots with the names the mouse stores (slot 1 the MacBook Pro, slot 2 OLDWATERBOY, slot 3 AVAPC-X825243P7).

---

### Task 20: Hardware verification checklist (Michael)

- [ ] **Step 1: Stage 2 of the rollout — upstream's divert on Bluetooth-direct**

In the dev bundle's `~/.config/openlogi-dev/config.toml` (or `~/.config/openlogi/config.toml` for the DMG install), on the MX Keys S entry, set `host_switch_targets = ["<mouse key>"]` (the mouse key is the map key of the MX Master 3S entry). Reload (`openlogi` GUI save, or restart the agent). Press Easy-Switch 2. Expected: the mouse follows within a second. If it does not, stop here: capture `OPENLOGI_LOG=debug` agent output and open an upstream issue/PR for Bluetooth-direct links before continuing.

- [ ] **Step 2: PR 1 behaviour**

Add `[devices."<keyboard key>".host_switch_actions]` with `slot_2 = { RunShellCommand = "printenv > /tmp/hs.txt" }`. Press 2 from the Mac. Expected: `/tmp/hs.txt` exists with `OPENLOGI_HOST_SLOT=2`, `OPENLOGI_TRIGGER=key`, `OPENLOGI_DEVICE_KEY=<keyboard key>`. Press 1 on OLDWATERBOY's keyboard session to come back; on the Mac, `arrive` (configured as `{ RunShellCommand = "date >> /tmp/arrive.txt" }`) appended a line. Restart the agent while the keyboard is connected: no new line.

- [ ] **Step 3: PR 2 behaviour**

`openlogi host switch 2` from the Mac: the mouse and keyboard move, `/tmp/hs.txt` now says `OPENLOGI_TRIGGER=command`, exit code 0. `openlogi host switch 1` while already on 1: prints "already on slot 1", no new file write, exit 0. Temporarily unpair slot 3 on the mouse only and run `openlogi host switch 3`: exit 2 with the unpaired message and nothing moves.

- [ ] **Step 4: Record the results in both PR drafts**

Fill the hardware lines of `docs/pr-drafts/openlogi-pr1.md` and `openlogi-pr2.md` with what actually happened, including anything that did not work.
