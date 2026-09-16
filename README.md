# deskswitch

Press 1, 2, or 3 on the MX Keys S and the whole desk follows: mouse, keyboard, and the Samsung Odyssey Ark 2nd Gen all land on that machine. A Stream Deck button or a shell command does the same. Everything about the desk is configuration, nothing is hard-coded.

See [`docs/superpowers/specs/2026-09-02-ark-desk-switch-design.md`](docs/superpowers/specs/2026-09-02-ark-desk-switch-design.md) for the complete design.

## Installation

### macOS

Install the daemon only after the config and both secrets exist — bootstrapping it first
means `launchctl` starts (and, without the `Crashed`-only `KeepAlive` below, would keep
respawning) a daemon that has nothing to work with yet.

1. Quit the Elgato Stream Deck app. It holds the device exclusively, so `deskswitch daemon`
   cannot open it while Elgato's app is running.
2. Build and install the binary:
   ```bash
   cargo build --release --features streamdeck
   sudo cp target/release/deskswitch /usr/local/bin/deskswitch
   ```
3. Write `~/.config/deskswitch/config.toml` (see [Configuration](#configuration)), then
   validate it:
   ```bash
   deskswitch config check
   ```
4. Log in to SmartThings and store the LAN secret (see [SmartThings Setup](#smartthings-setup)):
   ```bash
   deskswitch smartthings login --client-id <ID> --client-secret <secret>
   deskswitch lan set-secret
   ```
5. Only once `config check` passes and both secrets are stored, install the daemon:
   ```bash
   cp deploy/com.deskswitch.daemon.plist ~/Library/LaunchAgents/
   # launchd does not expand "~"; point the log paths at your actual home directory first.
   sed -i '' "s#~/Library/Logs#$HOME/Library/Logs#g" ~/Library/LaunchAgents/com.deskswitch.daemon.plist
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.deskswitch.daemon.plist
   ```
   The plist's `KeepAlive` only restarts the daemon when it crashes — a config error makes
   it exit cleanly instead of being respawned every few seconds.

### Windows

1. Build: `cargo build --release` (no `streamdeck` feature).
2. Copy `target\release\deskswitch.exe` to `C:\Program Files\deskswitch\deskswitch.exe`.
3. Config: `%APPDATA%\deskswitch\config.toml`; run `deskswitch config check`.
4. LAN secret: `deskswitch lan set-secret`.
5. Register daemon at logon:
   ```
   schtasks /Create /TN deskswitch /SC ONLOGON /RL LIMITED /TR "\"C:\Program Files\deskswitch\deskswitch.exe\" daemon"
   ```

## Configuration

Create `~/.config/deskswitch/config.toml` (macOS/Linux) or `%APPDATA%\deskswitch\config.toml` (Windows):

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
backends = ["smartthings"]        # tried in order until one succeeds

[monitor.smartthings]
device_id = "<filled by `deskswitch smartthings devices`>"
client_id = "<from the SmartThings CLI app registration>"

[lan]
listen = "0.0.0.0:7391"

[streamdeck]
enabled = true                    # only true on the machine it is plugged into
buttons = ["mac", "avapc", "oldwaterboy"]
font = "/System/Library/Fonts/Supplemental/Arial.ttf"   # optional; per-OS defaults are tried when unset

[openlogi]
binary = "/usr/local/bin/openlogi"   # absolute in production

[secrets]
store = "keyring"                 # "file" writes secrets.json next to the config
```

## OpenLogi Integration

Add to `~/.config/openlogi/config.toml` on each machine:

```toml
[devices."<mx-keys-s key>".host_switch_actions]
slot_1 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 1" }
slot_2 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 2" }
slot_3 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 3" }
arrive = { RunShellCommand = "/usr/local/bin/deskswitch arrive" }
```

On Windows, use `C:\Program Files\deskswitch\deskswitch.exe switch --slot N`.

Each hook receives these environment variables:

```
OPENLOGI_HOST_SLOT      # target slot, 1–3 (absent for arrive)
OPENLOGI_DEVICE_KEY     # the keyboard's config key
OPENLOGI_TRIGGER        # key, command, or arrive
```

## SmartThings Setup

One-time setup:

1. Register an OAuth-In app: `smartthings apps:create`
   - Name: anything
   - Type: OAuth-In / API only
   - Scopes: `r:devices:*` `x:devices:*`
   - Redirect URI: `http://localhost:8765/callback`
   - CLI prints client ID and secret

2. Login and store the token:
   ```bash
   deskswitch smartthings login --client-id <ID> --client-secret <secret>
   ```

3. List your Ark and copy its device ID:
   ```bash
   deskswitch smartthings devices
   ```

4. Add to config:
   ```toml
   [monitor.smartthings]
   device_id = "<copied from step 3>"
   client_id = "<from step 1>"
   ```

5. Verify: `deskswitch config check`

## LAN Relay Setup

Needed only if you use `[lan]` so a switch on one machine relays to whichever machine
actually holds the keyboard. Run this on every machine listed with an `address`:

1. Generate a secret — 32 random bytes, hex-encoded. Don't pass it as a command-line
   argument (it would land in your shell history); the next step prompts for it instead.
   ```bash
   openssl rand -hex 32                                          # macOS/Linux
   ```
   ```powershell
   -join ((1..32) | % { '{0:x2}' -f (Get-Random -Max 256) })     # Windows
   ```
2. Store it: `deskswitch lan set-secret`, then paste the value at the prompt. Use the
   *same* secret on every machine.
3. `listen = "0.0.0.0:7391"` binds every network interface, not just your LAN. On a laptop
   that also joins untrusted Wi-Fi (coffee shops, airports), either bind the LAN-only
   address instead (e.g. `listen = "192.168.1.50:7391"`) or firewall port 7391 away from
   those networks.
4. The relay is plain HTTP: the bearer secret and the switch requests cross the LAN
   unencrypted. Fine on a trusted home network; don't expose 7391 beyond it.

## Commands

| Command | Purpose | Exit codes |
|---------|---------|-----------|
| `switch --slot N` | Switch to slot N (1–3) or machine. | 0 = done, 1 = config error or agent refused, 2 = usage error (no/ambiguous target) or every backend failed, 3 = target not resolvable |
| `switch --to <machine>` | Switch to named machine. | (same as above) |
| `switch --input <X>` | Switch directly to input (HDMI1/2/3, DP). | (same as above) |
| `arrive` | Fallback: monitor follows keyboard on offline→online. | 0 = done, rate limited, or already there; 1 = config error; 2 = monitor switch failed |
| `status [--json]` | Current input, keyboard holder, last switch. | 0 = OK, 1 = config error |
| `daemon` | LAN listener, Stream Deck, periodic refresh. | Runs indefinitely. |
| `config check` | Validate config and dry-run all resolutions. | 0 = OK, 1 = config error |
| `smartthings login` | One-time OAuth setup. | 0 = OK, 1 = error |
| `smartthings devices` | List available devices. | 0 = OK, 1 = error |
| `lan set-secret` | Store the shared relay secret (prompts for it; see [LAN Relay Setup](#lan-relay-setup)). | 0 = OK, 1 = error |

**Note:** `switch` with no target flag is a usage error (exit 2).

**Note:** `--input <X>` bypasses the machines table only for *which input to show*; an input owned by a configured machine (e.g. `HDMI2` when `avapc` is on it) still behaves exactly like `--to avapc` — the keyboard follows too. An input no machine owns (e.g. `DP`) only moves the monitor and is never relayed to a peer.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

No CI yet (this repo has no remote); these three commands are the gate — run them before
every commit. `--all-features` is what builds and tests `src/streamdeck.rs` at all (a plain
`cargo test` never touches it), but seeing it actually drive the hardware — button LEDs
lighting up, presses registering — needs a Stream Deck Plus plugged in.
