# Desk probe — 2026-09-02

Raw findings from probing the desk from the MacBook Pro. Source of truth for the design spec.

## Monitor — Samsung Odyssey Ark 2nd Gen
- Model `LS55CG970NNXGO` (G97NC, 2023), advertises as "55" Odyssey Ark G9". OS is **Tizen** (not Android).
- LAN: `192.168.1.153` (Wi-Fi, MAC 94:E6:BA:01:0C:4F), mDNS `_samsungmsf._tcp`, `_airplay._tcp`, `_smartthings._tcp` (D052A8192B8F000D), `_smartthings-hedge._tcp` (built-in SmartThings hub).
- Open ports: 8001 (REST + WS remote API), 8002 (WSS, token auth), 8080, 9197, 9119, 15500.
- `GET http://192.168.1.153:8001/api/v2/` → `TokenAuthSupport=true`, `PowerState=on`, `remote_available`.
- Connected to the Mac via the iVANKY FusionDock Max 2 HDMI at **3840x2160@60Hz** (macOS flags it `Television: Yes`). CoreGraphics id 4, vendor 0x4c2d model 0xe01e.
- **DDC/CI: BetterDisplay 4.3.6 CLI reports `Failed.` for every DDC query** on the Ark → treat DDC as unavailable.
- One Connect box (per published specs): 3× HDMI 2.1, 1× DP 1.4, USB hub with 2× Type-A downstream, 2× Type-B + 2× Type-C **upstream for the built-in USB KVM** (needs Samsung Easy Setting Box per PC).
- Input switching options: SmartThings cloud (`samsungvd.mediaInputSource` / `setInputSource`; PATs now expire in 24h → OAuth authorization_code flow needed), HDMI-CEC via a Pulse-Eight USB adapter (local), local Tizen WS `KEY_SOURCE` + navigation (fragile; no direct `KEY_HDMIn` on Tizen).
- Nothing has been paired to the Tizen WS API yet (pairing pops an Allow prompt on the screen).

## Keyboard / mouse — Logitech, Bluetooth LE direct (no receiver)
- MX Keys S for Mac: pid `0xb37c`, BLE, serial 2520CFB1PV68. Exposes HID++ vendor collection `0xFF43/0x0202`. Plain Python hidapi cannot open it (macOS Input Monitoring TCC); `openlogi list` can.
- MX Master 3S: pid `0xb034`, BLE, serial 2522APKEUPE8. HID++ probe over BLE from the Mac:
  - ChangeHost 0x1814 → feature index 10 (v1); HostsInfo 0x1815 → index 11 (v2); numHosts=3, currentHost=0.
  - slot 1: BLE  "Michael’s MacBook Pro" (this Mac) → One Connect **HDMI1**
  - slot 2: bus 5 "OLDWATERBOY" (Windows; not seen on the LAN during probe) → **HDMI3**
  - slot 3: BLE  "AVAPC-X825243P7" → `192.168.1.191` (Windows box) → **HDMI2**
  - Mapping confirmed by the user 2026-09-02. Requirement: the slot↔input↔machine mapping must be configuration, never hard-coded.
- Older MX Keys Mac (0xb361) + MX Master 3 Mac (0xb023) are paired but not connected (second set).

## Stream Deck
- Stream Deck **Plus** (Elgato 0x0fd9:0x0084, serial EL31L1A09758) on USB via the dock. Elgato app 7.4.0 installed, LaunchAgent present, not running.

## Software on the Mac
- macOS 27.0 (26A5406e), M4 Max, 128 GB. BetterDisplay 4.3.6 (running), Raycast, OpenLogi 0.6.18 app bundle.
- brew: python 3.12–3.14, node, go, rust, libusb, tailscale, ollama … (no hidapi/m1ddc/ddcctl/blueutil).

## OpenLogi (user's fork, MichaelDanCurtis/OpenLogi)
- Source: `/Volumes/Main External/Development/Logi-Open` (branch `feature/function-key-remapper`, v0.6.22, edition 2024, devenv/Nix toolchain — run cargo via `direnv exec .`).
- Already has: `hidpp::feature::change_host` (`get_host_info`, `set_current_host`, cookies), `hosts_info`, `openlogi_hid::watch_hotplug`, agent-core inventory watcher, tarpc IPC (append-only), CLI (`list`, `assets`, `diag`), GUI, macOS/Linux/Windows.
- Conventions in `AGENTS.md`; specs/plans live in `docs/superpowers/`.

## Other LAN hosts seen
unifi gw .1, daphne .59, joey_br .77, las-production .138 (ssh), iphone .142, avapc-x825243p7 .191, desktop-t0ep741 .202, hopperduo-br .216, moog-one (ssh via mDNS), espressif .176, litter-robot4 .242.
