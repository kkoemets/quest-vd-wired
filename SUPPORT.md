# Support

Quest VD Wired is a community project. Help is provided on a best-effort basis,
with no guaranteed response time.

## Setup and usage help

Start with the [README troubleshooting guide](README.md#quick-fixes), then ask
in [Q&A Discussions](https://github.com/kkoemets/quest-vd-wired/discussions/categories/q-a)
if the problem remains.

Include:

- the Quest VD Wired version;
- Windows 10 or 11 and its version;
- the Quest model and system version;
- the Virtual Desktop app and Streamer versions;
- the cable type and whether the PC port is USB 3;
- what you expected, what happened, and the steps to reproduce it;
- whether USB debugging, the VPN prompt, Wi-Fi-off connectivity, and cable
  reconnect worked.

If the tray app is available, export a redacted support file from PowerShell in
the extracted app directory:

```powershell
.\quest-vd-wired.exe diagnostics export quest-vd-wired-support.jsonl
```

Review any attachment before sharing it. The built-in export is designed to
redact sensitive fields, but you remain in control of what you publish.

## Bugs and feature requests

Use the [issue chooser](https://github.com/kkoemets/quest-vd-wired/issues/new/choose)
for a reproducible defect or a focused feature request. General troubleshooting
belongs in Discussions so the issue tracker stays actionable.

Do not report suspected vulnerabilities publicly. Follow
[SECURITY.md](SECURITY.md) for private reporting.
