# Ark desk switch — design

Date: 2026-09-02. Status: approved in conversation, awaiting written review.

Press 1, 2, or 3 on the MX Keys S and the whole desk follows: mouse, keyboard, and the
Samsung Odyssey Ark 2nd Gen all land on that machine. A Stream Deck button or a shell
command does the same. Everything about the desk is configuration, nothing is hard-coded.

Raw probe results that this design rests on: [`docs/2026-09-02-desk-probe.md`](../../2026-09-02-desk-probe.md).

## 1. Goals and non-goals

Goals

- Keyboard-initiated switching: the physical Easy-Switch keys drive the monitor as well as
  the peripherals.
- Software-initiated switching: a Stream Deck Plus button, a hotkey, or `deskswitch switch`
  moves peripherals and monitor together, from whichever machine is live.
- Everything Logitech-side lands in OpenLogi as generic, upstreamable features, so the
  maintainer can adopt them. Everything Samsung-specific stays in a small separate service.
- Configurable: machine names, keyboard slots, Ark inputs, backend order, peers, and the
  Stream Deck layout all live in TOML.
- Each rollout stage leaves the desk usable.

Non-goals for v1

- Custom CEC hardware (deferred, interface reserved in §7.3).
- OpenLogi GUI work, including the 3D keyboard-and-mouse Easy-Switch view (deferred, §10).
- Waking or powering the Ark, OS notifications, mDNS peer discovery, the Ark's USB KVM.

## 2. Hardware and facts the design depends on

| Thing | Fact |
|---|---|
| Monitor | Samsung Odyssey Ark 2nd Gen, model LS55CG970NNXGO (G97NC), Tizen OS, 192.168.1.153 on Wi-Fi. Advertises SmartThings. |
| Inputs in use | Mac → HDMI1, AVAPC-X825243P7 (Windows) → HDMI2, OLDWATERBOY (Windows) → HDMI3. Configurable. |
| DDC/CI | Not available: BetterDisplay fails every DDC query. Monitor control must be network (or CEC later). |
| Keyboard | MX Keys S for Mac, Bluetooth LE direct, HID++ on usage page 0xFF43. Three host slots. |
| Mouse | MX Master 3S, Bluetooth LE direct. Reports ChangeHost (0x1814) and HostsInfo (0x1815); slots: 1 = MacBook Pro, 2 = OLDWATERBOY, 3 = AVAPC-X825243P7. |
| OpenLogi | Upstream v0.8.3 already implements linked host switching: a keyboard's `host_switch_targets` makes pointing devices follow it by diverting the keyboard's host controls (CIDs 0xD1–0xD3), reading the pressed slot, moving the mice, then the keyboard. Transport-agnostic in code; Bluetooth-direct is verified on hardware in rollout stage 2. |
| Fork | `/Volumes/Main External/Development/Logi-Open`, remote `origin` = AprilNEA/OpenLogi, remote `fork` = MichaelDanCurtis/OpenLogi. Fork master is 648 commits behind upstream. |
| Stream Deck | Stream Deck Plus on USB via the iVANKY dock. The Elgato app is installed but unwanted. |
| SmartThings | Personal access tokens expire after 24 hours; long-lived access needs the OAuth authorization-code flow with a refresh token. |

Slot numbering: everything a person sees is 1-based (key caps, config, CLI, environment
variables). HID++ host indexes are 0-based and stay internal to OpenLogi.

## 3. Architecture

Three pieces, one job each.

1. **OpenLogi (upstreamable).** Two PRs add the outward-facing seams that upstream's
   host switching lacks: per-slot actions fired when a switch to a slot is observed, an
   `arrive` fallback action, and an `openlogi host` CLI backed by new IPC methods. §5.
2. **Desk service `deskswitch` (this repository).** A Rust binary per machine that maps
   slots, machines, and Ark inputs, talks to the Ark through pluggable backends
   (SmartThings first), drives the Stream Deck Plus, and relays switches between
   machines over an authenticated LAN endpoint. §6–§7.
3. **CEC bridge (deferred).** An always-on microcontroller on the HDMI CEC line. The
   backend interface is fixed now so the hardware can be built later. §7.3.

Guiding rule: **whichever OpenLogi agent holds the keyboard is the single trigger point.**
The desk service hands the whole job to the local agent whenever it can, and the agent's
slot action moves the monitor. The desk service only moves the monitor directly when no
local agent holds the keyboard.

## 4. Configuration

### 4.1 OpenLogi side (per machine, `config.toml`, schema 7, no bump)

One optional table on a keyboard's existing device entry. Device keys are whatever the
agent already wrote for the device (identity-based: unit id or serial).

```toml
[devices."<mx-keys-s key>"]
custom_name = "MX Keys S"
host_switch_targets = ["<mx-master-3s key>"]      # upstream: mouse follows keyboard

[devices."<mx-keys-s key>".host_switch_actions]
# Fired the instant a switch to that slot is observed, whether from the physical
# 1/2/3 key or from `openlogi host switch N`. Runs concurrently with the device
# moves and never delays them.
slot_1 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 1" }
slot_2 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 2" }
slot_3 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 3" }
# Optional fallback: the keyboard connected to this machine without this agent
# having seen a key (the previous machine was asleep or agentless).
arrive = { RunShellCommand = "/usr/local/bin/deskswitch arrive" }
```

Absolute paths are required: the agent runs hooks with a minimal PATH. On Windows the
values are `C:\Program Files\deskswitch\deskswitch.exe switch --slot 1` and so on.

Every hook command receives these environment variables:

| Variable | Value |
|---|---|
| `OPENLOGI_HOST_SLOT` | target slot, `1`–`3` (absent for `arrive`) |
| `OPENLOGI_DEVICE_KEY` | the keyboard's config key |
| `OPENLOGI_TRIGGER` | `key`, `command`, or `arrive` |

### 4.2 Desk service (per machine)

Path: `~/.config/deskswitch/config.toml` on macOS and Linux,
`%APPDATA%\deskswitch\config.toml` on Windows. Secrets never live in the file; they go to
the OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service) via the
`keyring` crate.

```toml
schema_version = 1
this_machine = "mac"              # which [[machines]] entry is me

[[machines]]
name = "mac"
slot = 1                          # Easy-Switch slot on the keyboard
input = "HDMI1"                   # canonical Ark input name
address = "mac.local:7391"        # optional, for relayed switches

[[machines]]
name = "avapc"
slot = 3
input = "HDMI2"
address = "avapc-x825243p7.local:7391"

[[machines]]
name = "oldwaterboy"
slot = 2
input = "HDMI3"
address = "oldwaterboy.local:7391"

[monitor]
backends = ["smartthings"]        # tried in order until one succeeds;
                                  # "tizen" and "cec_bridge" slot in here later

[monitor.smartthings]
device_id = "<filled by `deskswitch smartthings devices`>"
client_id = "<from the SmartThings CLI app registration>"
# client secret, access token, and refresh token live in the keychain

[lan]
listen = "0.0.0.0:7391"
# shared secret in the keychain; peers send POST /switch {"slot": 2}
# with Authorization: Bearer <secret>

[streamdeck]
enabled = true                    # only true on the machine it is plugged into
buttons = ["mac", "avapc", "oldwaterboy"]
```

Validation (`deskswitch config check`): machine names unique; slots unique and within
1–3; inputs unique and drawn from `HDMI1`, `HDMI2`, `HDMI3`, `DP`; `this_machine` names an
entry; at least one backend listed and each listed backend has its table; every
`buttons` entry names a machine. With SmartThings configured, `config check` also
fetches the device's `supportedInputSourcesMap` and rejects inputs the Ark does not
report.

Resolution: `switch --slot N` finds the machine with that slot and uses its input;
`--to <name>` uses that machine; `--input <X>` bypasses the table. Backends translate the
canonical input names into their own vocabulary, so the machines table never changes when
the backend does.

## 5. Switch protocol

### 5.1 Happy path: press 2 while on the Mac

1. The Mac's agent receives the diverted key for slot 2.
2. It spawns the slot-2 action, `deskswitch switch --slot 2`, and in parallel starts the
   device moves: verify slot 2 is paired on the keyboard, move the mouse, then the
   keyboard. The keyboard reconnects to OLDWATERBOY in roughly a second.
3. `deskswitch` (hook mode, see §7.2) resolves slot 2 → OLDWATERBOY → HDMI3, reads the
   Ark's current input, skips if already there, otherwise sends the switch. With
   SmartThings the screen lands in about 2–3 seconds including HDMI resync. It records the
   target and time in the state file.
4. OLDWATERBOY's agent sees the keyboard come online, re-arms its own divert session, and
   runs `arrive`. Its `deskswitch` waits the grace period, then reads the input. Normally
   it is already HDMI3, so nothing happens. `arrive` is a safety net, never a second writer.

### 5.2 Software-initiated on the machine that holds the keyboard

`deskswitch switch --to oldwaterboy` (person, hotkey, or Stream Deck) asks the agent
whether the keyboard is here, then calls `openlogi host switch 2` and returns. The fired
slot action does the monitor. One path, no duplication.

### 5.3 Software-initiated on the Mac while another machine holds the keyboard

`deskswitch` flips the monitor itself immediately, then posts the switch to every
configured peer. The peer that holds the keyboard runs `openlogi host switch N`; its slot
action reads the input, finds it already correct, and stops. Peers without the keyboard
answer `ignored`. A relayed request is never relayed again.

### 5.4 Failure and edge cases

- **Machine being left is asleep or agentless.** No divert happens; the keyboard firmware
  switches on its own; `arrive` on the target moves the monitor after the grace period.
  Nobody can push the mouse in this case, so the user presses the mouse's own switch
  button. Documented limitation, identical to Logitech Flow.
- **Cloud slow or down.** Each backend gets a 5-second timeout, then the next configured
  backend, then one full retry pass after 3 seconds. On final failure the Stream Deck
  button flashes red and the error is logged. The peripherals still moved; the remote's
  Source button is the manual fallback.
- **Bluetooth idle reconnects.** `arrive` is rate-limited to once per 30 seconds and always
  reads before writing, so a keyboard waking on the same machine never flaps the monitor.
- **Agent startup.** `arrive` fires only on an offline→online transition observed after the
  agent's first inventory reconcile, never for devices already present, so restarting an
  agent while working elsewhere cannot steal the screen.
- **Unpaired slot.** The agent refuses (upstream already checks HostsInfo), the CLI exits
  non-zero with a message, and no action fires.
- **Two keys in quick succession.** The session captures one press, restores the controls,
  and switches. The second press is handled by the new host normally.
- **Hook re-entrancy.** In hook mode `deskswitch` never calls `openlogi host switch`, so a
  command-initiated switch cannot loop.

### 5.5 Timing budget

| Step | Expected |
|---|---|
| Key press → mouse on target | ~0.5 s |
| Key press → keyboard on target | ~1–1.5 s (BLE reconnect) |
| Key press → Ark on target, SmartThings | ~2–3 s (cloud + HDMI resync) |
| Key press → Ark on target, CEC bridge (later) | ~1 s |

## 6. OpenLogi changes

Prerequisite: fast-forward the fork's `master` to upstream v0.8.3 and branch each PR from
it (`type/kebab-description` off master, per the repo's `AGENTS.md`). The existing
`feature/function-key-remapper` branch stays untouched; its substance is upstream as #344.

### 6.1 PR 1 — host switch actions (scope `core`, `agent`)

`openlogi-core`, `config/device.rs`:

```rust
/// Actions fired around a keyboard's host switching.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSwitchActions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_1: Option<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_2: Option<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_3: Option<Action>,
    /// Fired when this keyboard transitions offline → online after the
    /// agent's first reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive: Option<Action>,
}

impl HostSwitchActions {
    /// Action for a 0-based HID++ host index (`0` → `slot_1`).
    pub fn for_host_index(&self, index: u8) -> Option<&Action>;
    pub fn is_empty(&self) -> bool;
}
```

Added to `DeviceConfig` and its raw serde twin as `host_switch_actions: HostSwitchActions`
with `#[serde(default, skip_serializing_if = "HostSwitchActions::is_empty")]`, so untouched
configs serialize byte-for-byte as before. Additive and optional: no schema bump.

`openlogi-agent-core`:

- `HostSwitchLink` gains `actions: HostSwitchActions`. `orchestrator::host_switch_links`
  creates a link when the keyboard is online and (targets non-empty **or** actions
  non-empty). Today a keyboard with no mouse target never gets a divert session at all.
- `watchers/host_switch.rs`: `run_transition` dispatches the slot's action first,
  fire-and-forget through the existing path that turns an `Action` into `openlogi-inject`
  workflow steps, with the hook environment of §4.1 (`OPENLOGI_TRIGGER=key`), then
  acquires the exclusive lease and moves the devices exactly as today. A dispatch failure
  is logged at warn and never blocks the transition.
- `arrive`: the orchestrator's reconcile keeps per-keyboard online state for keyboards
  whose config has `arrive`. After the first reconcile has completed, an offline→online
  edge dispatches `arrive` with `OPENLOGI_TRIGGER=arrive`. Latency is bounded by the
  inventory poll period, acceptable for a fallback.

`openlogi-inject`: the shell executor gains an entry point that accepts extra environment
variables for hook-triggered runs. `WorkflowStep::RunShellCommand(String)` is untouched,
so neither the config file format nor the IPC wire changes.

Docs: `docs/CONFIGURATION.md` device fields gain `host_switch_actions`, with the
environment variables listed and the absolute-path advice.

### 6.2 PR 2 — `openlogi host` (scope `ipc`, `cli`, `agent`, `device`)

`openlogi-device`: `read_host_slots(device) -> Result<HostSlots, _>` generalizes the
existing `host_slot_is_empty` HostsInfo read into a summary. As with the existing check, a
device that lacks HostsInfo, times out, or answers oddly yields `paired: None`, never an
error that blocks anything.

`openlogi-ipc` (append-only; `PROTOCOL_VERSION` 29 → 30; goldens regenerated; the mock
agent implements both):

```rust
async fn host_slots(route: DeviceRoute) -> Result<HostSlots, WriteError>;
async fn switch_host(keyboard: DeviceRoute, slot: u8) -> Result<HostSwitchOutcome, HostSwitchCommandError>;
// `slot` is 1-based on the wire; the agent converts to the 0-based HID++ host index.

pub struct HostSlots { pub current: u8, pub slots: Vec<HostSlotInfo> }          // 1-based
pub struct HostSlotInfo { pub slot: u8, pub paired: Option<bool>, pub name: Option<String> }
pub enum HostSwitchOutcome { Switched, AlreadyCurrent }
pub enum HostSwitchCommandError { KeyboardNotFound, InvalidSlot, UnpairedSlot, Busy, Io(String) }
```

Agent side:

- `openlogi-device`: `HostSwitchStopReason::Requested(host_index)`. When a running
  session receives it, it restores the diverted controls and returns `Ok(Some(host))`
  exactly as if the key had been pressed.
- The host-switch manager gets a command channel. `switch_host` looks up the running
  session for the keyboard: if found, it stops it with `Requested`, so the normal
  completion path runs `run_transition` (slot action with `OPENLOGI_TRIGGER=command`, then
  the device moves). If no session is armed, the handler runs the transition directly with
  the configured targets and actions. If the keyboard already reports the requested slot as
  current, the reply is `AlreadyCurrent` and no action fires. The reply resolves after
  `switch_linked_hosts` returns. `Busy` is returned while pairing holds exclusive access.
- `host_slots` opens the keyboard through the shared channel pool and reads HostsInfo.

`openlogi-cli`, new `host` subcommand:

```
openlogi host list [--json]
openlogi host switch <SLOT> [--keyboard <KEY>]
```

`list` prints, per keyboard that reports ChangeHost: config key, name, online, current
slot, and the slot table. `switch` defaults to the only online keyboard with ChangeHost
and errors when ambiguous. `list` uses the agent when one is running and otherwise reads
the devices directly, like `openlogi list`; `switch` requires a running agent, because a
direct write would race the agent's own divert session. Exit codes: 0 switched or already current,
2 refused (unpaired or invalid slot), 3 keyboard not found, 4 agent unavailable.

`--json` shape, consumed by the desk service:

```json
{ "keyboards": [ { "key": "…", "name": "MX Keys S", "online": true,
    "current_slot": 1,
    "slots": [ { "slot": 1, "paired": true, "name": "Michael’s MacBook Pro" }, … ],
    "targets": ["…"] } ] }
```

Docs: `docs/USAGE.md`.

### 6.3 PR 3 — later

A bindable `Action::SwitchHost(slot)` for buttons and key triggers, and an Easy-Switch
panel in the GUI. The 3D keyboard-and-mouse view belongs there.

### 6.4 Tests (per the repo's rules: failure and edge paths, no tautologies)

- core: config round-trips, including that an empty `host_switch_actions` is omitted and
  that an unknown slot key fails loudly.
- orchestrator: link created with actions and no targets; no link when the keyboard is
  offline; actions carried into the link.
- watcher/session: key press fires the slot action and switches; `Requested` stop fires
  the action with `command`; refused slot fires nothing; no armed session falls back to
  the direct transition; dispatch failure does not block the switch.
- arrive: no fire at startup; fires on a later edge; fires once per edge.
- ipc: wire goldens and the pinned-version assertion.
- cli: argument parsing, JSON shape, exit codes against the mock agent.

Hardware verification is the maintainer's job; each PR body lists the exact keys to press
and what to observe.

## 7. Desk service `deskswitch`

### 7.1 Crate shape

One Rust binary in this repository. Modules, each kept well under 500 lines:
`config`, `state`, `resolve`, `monitor` (trait plus one file per backend), `openlogi`
(CLI shim), `lan`, `streamdeck` (cargo feature `streamdeck`, on in the Mac build), `cli`.
Libraries return `thiserror` types; the binary uses `anyhow`. Logging via `tracing` to
stderr plus a daily-rotated file in the config directory, 7 days kept.

### 7.2 Commands

- `switch --slot N | --to <machine> | --input <X>`. Two modes, decided by the presence of
  `OPENLOGI_TRIGGER` in the environment.
  - **Hook mode** (variable present): monitor-only. Resolve, read, skip if equal, write.
  - **User mode** (absent): ask `openlogi host list --json` (3 s timeout). If the keyboard
    is online here and its current slot differs from the target, run
    `openlogi host switch N` (10 s timeout) and exit with its result: the slot action does
    the monitor. If the keyboard is here and already on the target slot, only the monitor
    is switched. If `openlogi` is missing or the agent is down, log a warning and continue
    as if the keyboard were elsewhere. Otherwise switch the monitor directly and, when
    peers are configured, POST `/switch` to all of them concurrently (2 s timeout each,
    failures logged).
  - Exit codes: 0 done, 1 usage or config error, 2 every backend failed, 3 target not
    resolvable.
- `arrive`: stop if the last arrive was under 30 s ago; otherwise record the time, sleep
  2.5 s, read, and switch only if the input is not already mine.
- `status [--json]`: current input from the backends, keyboard holder from `openlogi`,
  last switch from the state file.
- `daemon`: LAN listener, Stream Deck driver where enabled, input refresh every 30 s and
  after every switch. Runs on every machine, since the LAN relay needs a listener on each.
- `smartthings login`, `smartthings devices`, `lan set-secret`, `config check`.

### 7.3 Monitor backends

```rust
#[async_trait]
pub trait MonitorBackend {
    fn name(&self) -> &'static str;
    async fn current(&self) -> Result<Option<Input>, BackendError>;   // None = cannot read
    async fn switch(&self, input: Input) -> Result<(), BackendError>;
}
```

Chain semantics: `current` returns the first backend's `Some`. `switch` tries backends in
configured order with a 5 s timeout each; if all fail, one more full pass after 3 s; then
error. Before every write the chain reads and skips when the input already matches.

**SmartThings** (v1):

- One-time registration with the SmartThings CLI: `smartthings apps:create`, type
  OAuth-In / API only, scopes `r:devices:*` and `x:devices:*`, redirect
  `http://localhost:8765/callback`. The CLI prints client id and secret.
- `deskswitch smartthings login --client-id … --client-secret …` stores the secret in the
  keychain, opens the browser on the SmartThings authorize URL, receives the code on a
  local listener, exchanges it at the token endpoint (HTTP basic auth with the client
  credentials), and stores the access and refresh tokens in the keychain. Endpoints:
  authorize `https://api.smartthings.com/oauth/authorize`, token
  `https://auth-global.api.smartthings.com/oauth/token`, confirmed against the SmartThings
  developer docs during implementation.
- Refresh: on expiry or a 401, exchange the refresh token and store the rotated pair. If
  the refresh fails, the error says to run `smartthings login` again.
- `smartthings devices` lists devices exposing `samsungvd.mediaInputSource` so
  `device_id` can be chosen.
- Read: the device's `samsungvd.mediaInputSource` status, `inputSource` and
  `supportedInputSourcesMap`.
- Write: `POST /v1/devices/{id}/commands` with capability `samsungvd.mediaInputSource`,
  command `setInputSource`, argument the canonical input id.
- Canonical input names are the SmartThings ids (`HDMI1`…). `DP` is validated against
  the supported map, since the Ark's id for DisplayPort is confirmed at `config check`.
- If the Ark is powered off, the write fails and is reported like any other backend
  failure; power control is deferred (§10).

**Tizen local** (experiment, optional): pairing over the Ark's port-8002 remote API
stores a token in the keychain; `switch` sends the Source key and a deterministic
navigation sequence; `current` returns `None`. Go/no-go: kept only if 20 of 20 switches
succeed from every starting input. Not on the v1 critical path.

**CEC bridge** (deferred hardware, interface reserved): `[monitor.cec_bridge] url`;
`GET {url}/status` → `{"input":"HDMI2"}`; `POST {url}/switch` body `{"input":"HDMI3"}`.
An ESP32 running ESPHome's `hdmi_cec` component on a passive inline HDMI breakout can
implement it later without touching the desk service.

### 7.4 State

`state.json` next to the config, written atomically (temp file + rename):
`last_target_slot`, `last_switch_at`, `last_known_input`, `last_arrive_at` (RFC 3339).

### 7.5 LAN relay

HTTP/1.1 with JSON bodies on `[lan].listen`. Every request carries
`Authorization: Bearer <shared secret>`; anything else is rejected with 401 and logged.

- `POST /switch` body `{"slot": 2}` or `{"machine": "oldwaterboy"}`. Receiver: if the
  keyboard is here, run `openlogi host switch N` and answer `{"result":"delegated"}`;
  else `{"result":"ignored"}`. Never relays onward.
- `GET /status` → same JSON as `deskswitch status --json`.

The same secret is set on every machine with `deskswitch lan set-secret`.

### 7.6 Stream Deck Plus

Driven directly over USB HID with the `elgato-streamdeck` crate; the Elgato app must not
be running. One key per `[streamdeck].buttons` entry showing the machine name, lit when
`last_known_input` equals that machine's input. Press → `switch --to <machine>`. Failure →
red flash for one second. Redrawn after every switch and every status refresh.

### 7.7 Deployment

- macOS: binary at `/usr/local/bin/deskswitch`; `daemon` as a LaunchAgent
  (`RunAtLoad`, `KeepAlive`).
- Windows: `C:\Program Files\deskswitch\deskswitch.exe`; `daemon` as a logon task.
- Hook invocations are one-shot processes; startup cost is milliseconds.

## 8. Tests for the desk service

- Unit: config validation (duplicates, ranges, missing tables, unknown machine in
  `buttons`); resolution by slot, machine, input; backend chain ordering, timeout, retry,
  read-before-write skip, with fake backends; `arrive` rate limiting and grace; OpenLogi
  JSON parsing including missing or ambiguous keyboards; LAN bearer rejection and the
  never-relay rule; atomic state writes.
- Integration without hardware: a fake SmartThings HTTP server covering token exchange,
  refresh rotation, status, and commands; a fake `openlogi` executable on PATH scripting
  both switch modes end to end; hook-mode re-entrancy guard.
- Hardware: the rollout checklist below.

## 9. Rollout

1. Fast-forward the fork to upstream v0.8.3 (`git merge --ff-only origin/master`, push to
   `fork` after confirmation), build, install on the Mac replacing 0.6.18, confirm
   `openlogi list` sees both devices.
2. No code: set `host_switch_targets` on the keyboard, press 2, watch the mouse follow.
   This proves upstream's divert on Bluetooth-direct hardware. If it fails, the fix is the
   first upstream PR and takes priority over everything below.
3. Desk service with SmartThings. `deskswitch switch --input HDMI2` flips the Ark from a
   terminal on the Mac.
4. OpenLogi PR 1 built locally on the Mac. Press 2: everything moves.
5. Install OpenLogi and `deskswitch` on AVAPC and OLDWATERBOY. Test all six directions,
   then the `arrive` fallback from a sleeping machine.
6. OpenLogi PR 2, then the Stream Deck daemon and the LAN relay.
7. Open the upstream PRs against AprilNEA/OpenLogi (drafted here, posted by the user).
8. Later: CEC bridge hardware, Tizen spike, PR 3 with the GUI.

## 10. Deferred

- CEC bridge hardware (ESP32 + ESPHome `hdmi_cec` + inline HDMI breakout), interface in
  §7.3.
- Tizen local backend beyond the spike.
- OpenLogi PR 3: `SwitchHost` action, Easy-Switch GUI panel, the 3D keyboard-and-mouse
  view.
- OS notifications on failure, Ark power control, mDNS peer discovery, using the Ark's USB
  KVM for the Stream Deck.

## 11. Assumptions to confirm during rollout

- The Ark is registered in the user's SmartThings account and reports `HDMI1`, `HDMI2`,
  `HDMI3` in `supportedInputSourcesMap` (stage 3, `config check`).
- Upstream's divert session works over Bluetooth-direct (stage 2).
- OpenLogi's Windows build runs on both Windows machines (stage 5).
- The Elgato Stream Deck app stays closed on the Mac (stage 6).
