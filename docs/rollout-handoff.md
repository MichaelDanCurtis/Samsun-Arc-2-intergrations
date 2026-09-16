# Rollout handoff — what only Michael can do

Everything below needs the desk hardware, your SmartThings account, or your GitHub account, so it was left for you. Stages follow the design spec §9; the software side (OpenLogi PRs 1 and 2, `deskswitch`) is complete and reviewed. Record what happens, including what does not work, in `docs/rollout-log.md`.

## Where things are

| Piece | Location | State |
|---|---|---|
| OpenLogi PR 1 (host switch actions + `arrive`) | worktree `/Volumes/Main External/Development/Logi-Open-hs`, branch `feat/host-switch-actions`, head `a4eea94e` | complete, not pushed |
| OpenLogi PR 2 (`openlogi host list/switch`, protocol v30) | same worktree, branch `feat/host-cli` (stacked on PR 1), head `e9acf4e0`, currently checked out | complete, not pushed |
| PR bodies | `docs/pr-drafts/openlogi-pr1.md`, `docs/pr-drafts/openlogi-pr2.md` | each has one "TO BE FILLED BY MICHAEL" hardware paragraph |
| `deskswitch` | this repo, branch `feat/deskswitch-v1` | complete, reviewed, install steps in `README.md` and `deploy/` |

Your fork's local `master` was fast-forwarded to upstream v0.8.3 (`d5e2b880`). Nothing has been pushed anywhere; that is your call (see "Decisions" at the end).

## One thing to know about the dev build

The macOS dev bundle (`cargo run -p openlogi-desktop --release` in the worktree) runs under the `openlogi-dev` profile: its config is `~/.config/openlogi-dev/config.toml` and it has its own agent socket, separate from any installed OpenLogi. Two consequences:

- Put the `host_switch_actions` table in `~/.config/openlogi-dev/config.toml`, not `~/.config/openlogi/config.toml`.
- A CLI installed with `cargo install --path crates/openlogi` defaults to the production profile. For it (and for `deskswitch`, which shells out to it) to reach the dev agent, set `OPENLOGI_PROFILE=dev` in the environment. From a terminal: `export OPENLOGI_PROFILE=dev`. For the launchd daemon, add to the plist:

```xml
<key>EnvironmentVariables</key>
<dict><key>OPENLOGI_PROFILE</key><string>dev</string></dict>
```

Hooks fired by the agent do not need this: in hook mode `deskswitch` never calls the agent.

On Windows there is no bundle detection, so a fork build runs as production with no extra setting.

## Stage 2 — prove upstream's divert on Bluetooth-direct hardware (no new code)

In the worktree, on `feat/host-cli` (it contains everything):

```bash
cargo run -p openlogi-desktop --release
```

Confirm `openlogi list` (or the GUI) shows both the MX Keys S and the MX Master 3S. In `~/.config/openlogi-dev/config.toml`, link the mouse to the keyboard (the keys below are the ones in your current production config; the dev profile's inventory will show the same two):

```toml
[devices."direct:046d:b37c:serial:2520cfb1pv68"]
host_switch_targets = ["direct:046d:b034:serial:2522apkeupe8"]
```

Press Easy-Switch 2 on the keyboard: the mouse should follow.

If the mouse does not follow, stop here. That is an upstream bug on Bluetooth-direct and fixing it becomes the first PR, ahead of everything below.

## Stage 3 — SmartThings from the Mac

1. Install the SmartThings CLI: `brew install smartthingscommunity/smartthings/smartthings` (or `npm i -g @smartthings/cli`).
2. Follow the README's "SmartThings Setup": create the OAuth-In app (scopes `r:devices:*` `x:devices:*`, redirect `http://localhost:8765/callback`), keep the client ID and secret it prints. If the CLI refuses a plain-http localhost redirect, note the exact message in the log; the login flow will need an https redirect and we adapt.
3. Install `deskswitch` on the Mac in the README's order: build, copy the binary, write the config (the sample in the README has the mapping mac→HDMI1 slot 1, avapc→HDMI2 slot 3, oldwaterboy→HDMI3 slot 2), `deskswitch config check`, `deskswitch smartthings login --client-id … --client-secret …`, `deskswitch smartthings devices` (copy the Ark's device id into `[monitor.smartthings] device_id`), `config check` again. Expect it to report the Ark's inputs as HDMI1, HDMI2, HDMI3. If the names differ, use the reported names in `[[machines]].input`.
4. From a terminal: `deskswitch switch --input HDMI2`. The Ark should land on AVAPC within about three seconds. `deskswitch status` should show `input: HDMI2`. Switch back with `--input HDMI1`.

The SmartThings token refreshes itself; nothing expires after 24 hours the way a personal access token would.

## Stage 4 — keyboard press moves the desk (Mac only, PR 1)

Add to `~/.config/openlogi-dev/config.toml` under the keyboard's entry (the key is `direct:046d:b37c:serial:2520cfb1pv68` in your current production config; the dev profile will show the same key):

```toml
[devices."direct:046d:b37c:serial:2520cfb1pv68".host_switch_actions]
slot_1 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 1" }
slot_2 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 2" }
slot_3 = { RunShellCommand = "/usr/local/bin/deskswitch switch --slot 3" }
arrive = { RunShellCommand = "/usr/local/bin/deskswitch arrive" }
```

Press 2: mouse and keyboard hop to OLDWATERBOY and the Ark goes to HDMI3 within about three seconds. Press 1 on the keyboard (it is now talking to OLDWATERBOY, which has no agent yet): the keyboard comes back, and the Mac's `arrive` hook brings the Ark back to HDMI1.

While you are here, fill the hardware paragraph in `docs/pr-drafts/openlogi-pr1.md`. The paragraph names the exact checks (a `printenv > /tmp/hs.txt` action for slot 2, the `arrive` action writing a timestamp), so run those two once and write down what you saw.

## Stage 5 — both Windows machines

On AVAPC-X825243P7 and OLDWATERBOY:

1. Build OpenLogi from the fork's `feat/host-cli` branch on the PC and run its agent (Windows was never compiled here; if it fails to build, that is the first thing to record).
2. Install `deskswitch` per `deploy/windows-task.md`, the same config with `this_machine` set per box, `deskswitch lan set-secret` with the same secret on all three machines, `deskswitch config check` on each.
3. Put the same `host_switch_actions` table in each PC's OpenLogi config with the Windows path from `deploy/windows-task.md`.
4. Test all six directions from the keyboard.
5. Sleep OLDWATERBOY, then press 3 from the keyboard while it is on OLDWATERBOY's slot: AVAPC's `arrive` moves the Ark after about three seconds; the mouse stays where it was until its own button is pressed. That is the documented limit of the arrive fallback.

## Stage 6 — Stream Deck and LAN relay (PR 2 on every machine)

1. Quit the Elgato Stream Deck app on the Mac (it holds the device exclusively).
2. Start the daemon (`launchctl bootstrap` per the README, with the `OPENLOGI_PROFILE` line above if you are still on the dev bundle). Confirm three keys render with the machine names.
3. While AVAPC holds the keyboard, press the OLDWATERBOY key: the Ark flips immediately; within two seconds AVAPC's daemon logs `delegated` and the peripherals hop.
4. Unplug AVAPC's network and press again: the Ark still flips, the Mac's log shows the relay failure, the key does not flash red (the monitor succeeded).
5. Delete the SmartThings token from the keychain and press a key: red flash on the pressed key, error in the log. Run `deskswitch smartthings login` again afterwards.

Then fill the hardware paragraph in `docs/pr-drafts/openlogi-pr2.md` (`openlogi host list`, `host switch 2`, the "already on slot 1" case, the unpaired-slot refusal).

## Stage 7 — open the upstream PRs

Push `master`, `feat/host-switch-actions`, and `feat/host-cli` to your fork, then open PR 1 against AprilNEA/OpenLogi `master` with `docs/pr-drafts/openlogi-pr1.md` as the body, and PR 2 stacked on PR 1 (or after PR 1 merges) with `docs/pr-drafts/openlogi-pr2.md`. The bodies list the CI jobs not run locally (Windows clippy, Linux tests, typos) so reviewers know what to expect from CI.

## Decisions that are yours

- Push the three branches to your fork (nothing has been pushed).
- Squash the two `fix(…)` commits on `feat/host-cli` into the commits they repair, or leave the history as is. The PR 2 draft's first paragraph explains either choice.
- The plan-prescribed duplicate helper test in PR 2 was deleted on two reviewers' advice; restore it if you want the plan followed literally.
- Whether `feat/deskswitch-v1` merges into `main` now or after stage 3 proves the SmartThings path.

## Deferred, on record

CEC bridge hardware (ESP32 + ESPHome, spec §7.3), Tizen local backend, OpenLogi PR 3 with the Easy-Switch GUI and the 3D keyboard-and-mouse view, `openlogi host switch --json` (would remove `deskswitch`'s dependence on the CLI's "already on slot" wording).
