# PR 1 draft — feat/host-switch-actions → AprilNEA/OpenLogi master

Branch: `feat/host-switch-actions` (16 commits on top of upstream `d5e2b880`, v0.8.3; head `a4eea94e`).
Title: `feat: host switch actions and arrive hook for keyboards`

---

## Summary

Host-switch actions: a keyboard's new `host_switch_actions` table fires a configured action the instant a switch to a slot is validated (from the physical Easy-Switch key or a software switch), plus an `arrive` action when the keyboard reconnects to this host. Script actions receive the slot, device key, and trigger as environment variables. This lets external tools follow the keyboard — for example flip a monitor input — without OpenLogi knowing anything about monitors. Additive and optional: no schema bump, no IPC change.

## Changes

- core: `HostSwitchActions` (`slot_1..slot_3`, `arrive`, each an `Option<Action>`; `deny_unknown_fields`; omitted while empty) and `HostSwitchTrigger::{Key, Command, Arrive}` with `hook_env` producing `OPENLOGI_HOST_SLOT` (1-based, absent for arrive), `OPENLOGI_DEVICE_KEY`, `OPENLOGI_TRIGGER`; `DeviceConfig::host_switch_actions` (default + `skip_serializing_if`, raw twin, `From`), so an untouched `config.toml` round-trips unchanged; the table is folded by `adopt_route` like `host_switch_targets`, so a route-keyed entry keeps its actions when the keyboard's identity becomes readable (tested).
- inject: `execute_with_env(action, env)`; `execute` delegates with an empty env. Shell (`sh -c` / `cmd /C`), AppleScript, and workflow steps apply the env on all three platforms. `WorkflowStep::RunShellCommand(String)` untouched.
- hid: `switch_linked_hosts_with(…, on_validated)` — the callback runs exactly once, only when the keyboard's move is required and its slot passed the pairing check, before any device is moved. `switch_linked_hosts` stays as a thin delegate (no in-tree caller now, kept so the public surface is unchanged; happy to fold it if you prefer). Plus a no-hardware test proving the callback never fires when the keyboard cannot be opened.
- agent: `ActionDispatcher::dispatch_hook` (script actions → `execute_with_env`, everything else → `dispatch`); `HostSwitchLink` carries `keyboard_key` + `actions`, and a keyboard with actions but no `host_switch_targets` now gets a divert session; the transition fires the slot action via the validated callback with trigger `key`; `Orchestrator::refresh_inventory` returns `HostArrival`s (offline/absent → online edges observed after the first completed reconcile, never at startup) and the lifecycle dispatches `arrive` after releasing the orchestrator lock.
- docs: `CONFIGURATION.md` — new "Host switch actions" section and the device-field bullet.

## Testing

Run on macOS 27 / arm64, rustc 1.98.1, at `1cc0d9cc` (workspace gate) and re-run per crate at `a4eea94e` after review fixes (core 277, device 202, agent-core 226, agent 25+1 tests; fmt and clippy clean):

- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo clippy --target aarch64-unknown-linux-musl -p openlogi-inject -- -D warnings` — clean (Linux cfg of the inject change)
- `cargo test --workspace --all-targets` — 26 suites, 1355 tests passing, 0 failing, excluding `openlogi-agent`; `cargo test -p openlogi-agent -- --test-threads=1` — 25/25 + mock_agent 1/1. Under parallel execution `tray::tests::startup_stays_suspended_when_the_display_is_already_asleep` can fail: it and `overlapping_suspend_sources_all_clear_before_device_io_resumes` both post to the process-wide `NSWorkspace` notification center. Pre-existing on `d5e2b880`; `tray.rs` is untouched here. Happy to open a separate issue.
- `cargo xtask ci` — 7 passed (rustfmt, publish closure, clippy, MSRV macOS, rustdoc, tests macOS, wasm). Not run on this host: typos, shell, tests (linux), clippy (windows) proxy. `cargo-deny` fails on one pre-existing advisory (RUSTSEC-2026-0285, a transitive TLS dependency); identical on `d5e2b880`, and this branch changes no dependencies or `Cargo.lock`.
- Windows: not compiled locally; the three inject platform files were cross-checked by grep for every re-signatured function. Relying on CI's windows job.

Hardware (MX Keys S for Mac + MX Master 3S, both Bluetooth-direct, macOS):

TO BE FILLED BY MICHAEL: configured `slot_2 = { RunShellCommand = "printenv > /tmp/hs.txt" }` on the keyboard entry, pressed Easy-Switch 2 → `/tmp/hs.txt` contains `OPENLOGI_HOST_SLOT=2`, `OPENLOGI_TRIGGER=key`, `OPENLOGI_DEVICE_KEY=<keyboard key>`; the mouse followed; pressing the current slot's key fired nothing. `arrive` (`{ RunShellCommand = "date >> /tmp/arrive.txt" }`) fired on reconnect and not on agent restart.

Fixes #N (none — new capability; link the upstream discussion if one is opened first)
