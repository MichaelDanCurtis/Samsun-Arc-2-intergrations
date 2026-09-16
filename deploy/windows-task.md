# deskswitch on Windows

1. Build on the PC (or cross-compile): `cargo build --release` (no `streamdeck` feature).
2. Copy `target\release\deskswitch.exe` to `C:\Program Files\deskswitch\deskswitch.exe`.
3. Config lives at `%APPDATA%\deskswitch\config.toml`; run `deskswitch config check`.
4. Store the LAN secret: `deskswitch lan set-secret`.
5. Register the daemon at logon:

   schtasks /Create /TN deskswitch /SC ONLOGON /RL LIMITED /TR "\"C:\Program Files\deskswitch\deskswitch.exe\" daemon"

6. OpenLogi hooks on this machine use the absolute path:
   `C:\Program Files\deskswitch\deskswitch.exe switch --slot N`.
