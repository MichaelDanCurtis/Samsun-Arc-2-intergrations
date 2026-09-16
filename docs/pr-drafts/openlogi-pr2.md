# PR 2 draft — feat/host-cli → AprilNEA/OpenLogi master (stacked on PR 1)

Branch: `feat/host-cli` (16 commits on top of `feat/host-switch-actions` at `a4eea94e`; head `e9acf4e0`). Two `fix(…)` commits repair code introduced earlier on the same branch (`a12c9633`, `63cebb1b`); squash them into the commits they fix if you prefer a linear story before opening.
Title: `feat: software-initiated host switching and openlogi host`
Depends on: PR 1 (host switch actions). Open after PR 1 merges, or as a stacked PR with PR 1 as base.

---

## Summary

`openlogi host list` shows each keyboard's Easy-Switch slots (current slot, paired status, the host names the keyboard stores), and `openlogi host switch N` moves the keyboard and its linked mice to a slot from software. A software switch reuses the keyboard-press path: the armed divert session is stopped with a new `Requested(host)` reason, restores the diverted controls, and completes exactly as if the key had been pressed, so the keyboard's `host_switch_actions` fire with `OPENLOGI_TRIGGER=command`. Protocol v30 (two appended methods, goldens regenerated).

## Changes

- hidpp: `HostsInfoFeature::get_host_friendly_name` (function 3) and `HostFriendlyNameChunk`.
- core (`hid`): wire DTOs `HostSlots { current: Option<u8>, slots }`, `HostSlotInfo`, `HostSwitchOutcome`, `HostSwitchCommandError`; `HidppOperation::{ReadHostSlots, SwitchHost}` appended. `current` is `None` when the device does not report a slot (no `0` sentinel on the read side).
- hid (`openlogi-device`): `read_host_slots_on` — slot status via HostsInfo and friendly names assembled from 14-byte chunks (exactly `name_len` bytes, trailing NULs trimmed, valid UTF-8 required; an unreadable slot degrades to `paired: None`, never an error). `HostSwitchStopReason::Requested(u8)`; typed `HostSwitchError::InvalidHost { host, host_count }` replaces the stringly out-of-range error.
- ipc: `host_slots(route)` and `switch_host(keyboard, slot)` appended at the end of `Agent`; `PROTOCOL_VERSION` 29 → 30; `HOST_SWITCH_BUDGET` / `HOST_SLOTS_BUDGET` exported next to `OBSERVE_HOLD` so clients set a tarpc deadline above the agent's wait (tarpc's default 10 s deadline would otherwise cancel the handler mid-switch); wire goldens regenerated and every variant of the new enums pinned (new requests encode as `0x1a`/`0x1b`, hand-checked).
- agent-core: the host-switch watcher accepts `HostSwitchCommand`s over a channel exposed as `SharedRuntime::host_switch_commands`. A command for a keyboard with an armed session stops it with `Requested`; the completion path runs the transition with trigger `command` and answers the reply. With no session it stops every armed session first (the exclusive receiver lease would otherwise deadlock against their shared leases), then runs the transition directly. Pending replies are keyed by session generation so a session swept by `stop_all`/`stop_unwanted` before its completion is matched can never strand a caller; a panicked session task or an abandoned request (closed reply) is answered or skipped rather than left pending. `Busy` while device I/O is suspended, pairing or another host transition holds exclusive access, or another command is pending.
- agent: the receiver is handed to the watcher once at arming; `switch_host` converts the 1-based wire slot to the HID++ index, sends the command, and maps every outcome (send failure → `Busy`; dropped reply → `Io`; 20 s timeout → `Io`); `host_slots` reads through the shared channel like `read_dpi`. The mock agent scripts both.
- cli: `openlogi host list [--json] [--device NAME]` and `openlogi host switch <1..=3> [--device NAME]`. Exit codes: 0 switched/already current, 2 refused (unpaired or invalid slot), 3 no online keyboard matched (both output modes), 4 no compatible agent, 1 busy/io. JSON: `{"keyboards":[{"name","route","online","current_slot","slots":[{"slot","paired","name"}]}]}`, offline keyboards carry `"online": false` and `"error"`.
- docs: `USAGE.md` (the commands and exit codes); `CONFIGURATION.md` (`command` trigger now real).

## Testing

Run on macOS 27 / arm64, rustc 1.98.1, at `63cebb1b` (workspace gate) and re-run per crate at `e9acf4e0` after review fixes (fmt/clippy clean, wire_format 15/15, core+device+agent-core+cli 769 tests, agent 26/26 single-threaded, mock round-trip repeated):

- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test -p openlogi-ipc --test wire_format` — 15/15
- `cargo test --workspace --all-targets --exclude openlogi-agent` — 26 suites, 1373 passing, 0 failing; `cargo test -p openlogi-agent -- --test-threads=1` — 25/25 + mock 1/1 (the parallel-execution `tray.rs` NSWorkspace race noted in PR 1 is pre-existing on upstream master)
- `cargo xtask ci` — 7 passed (rustfmt, publish closure, clippy, MSRV macOS, rustdoc, tests macOS, wasm). Not run on this host: typos, shell, tests (linux), clippy (windows) proxy. `cargo-deny` fails on the pre-existing RUSTSEC-2026-0285 advisory (identical on upstream master; this branch's only lockfile change is `openlogi-cli` gaining the existing workspace `serde`/`serde_json`).
- Watcher state machine: unit tests cover the direct-command path stopping armed sessions before taking the exclusive lease (fails without the fix), orphaned-pending completion answering, the reply mapping, and a no-session command reaching a genuine `KeyboardNotFound`.
- Mock agent (`openlogi-agent-mock`): `openlogi host list --json` prints the scripted three slots; `openlogi host switch 2` → exit 0 (`Switched`); `switch 3` → exit 2 (`UnpairedSlot`); no agent → exit 4; `--device nonexistent` → exit 3.
- Windows: not compiled locally; relying on CI.

Hardware (MX Keys S for Mac + MX Master 3S, Bluetooth-direct, macOS):

TO BE FILLED BY MICHAEL: `openlogi host list` shows three slots with the stored host names; `openlogi host switch 2` moves mouse and keyboard, the slot action's `/tmp/hs.txt` shows `OPENLOGI_TRIGGER=command`, exit 0; `openlogi host switch 1` while already on 1 prints "already on slot 1", fires nothing, exit 0; with slot 3 unpaired on the mouse, `openlogi host switch 3` exits 2 and nothing moves.

Fixes #N (none — new capability)
