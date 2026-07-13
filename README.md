# Wired Virtual Desktop for Meta Quest 3

Use Virtual Desktop on your Quest 3 through a USB cable instead of relying on
Quest Wi-Fi. The PC keeps its normal internet connection and shares it with the
headset through the cable.

This project is for one Quest 3 and a Windows 10 or Windows 11 PC. It does not
need administrator access, a special network driver, SideQuest, or a rooted
headset.

## Choose your download

### v4.0.2 — current release

This is the latest version and the recommended download. It starts from a
simple icon near the Windows clock and reconnects automatically.

[Download v4.0.2](https://github.com/kkoemets/quest-vd-wired/releases/download/v4.0.2/gnirehtet-v4.0.2-windows-x64.zip)

### v3.1.0 Legacy — older Java version

This is the previous Java version, kept available for users who need to roll
back. New users should choose v4.0.2.

[Download v3.1.0 Legacy](https://github.com/kkoemets/quest-vd-wired/releases/download/v3.1.0/gnirehtet-java-v3.1.0.zip)

### v3.0.0 Legacy — older fallback

This older release remains available as an additional fallback. New users
should choose v4.0.2 instead.

[Download v3.0.0 Legacy](https://github.com/kkoemets/quest-vd-wired/releases/download/v3.0.0/gnirehtet-java-v3.0.0.zip)

If a download has not been published yet, check the
[Releases page](https://github.com/kkoemets/quest-vd-wired/releases).

## Before you start

You need:

- a Meta Quest 3 with Developer Mode enabled;
- USB debugging enabled on the headset;
- a USB 3 data cable;
- Virtual Desktop installed on the Quest;
- Virtual Desktop Streamer running on the PC;
- the PC connected to its normal internet network.

The first time you connect the headset, put it on and accept the USB debugging
prompt. Select **Always allow from this computer** if that choice is shown.

## Use v4.0.2

1. Download and extract the v4.0.2 zip.
2. Connect the Quest 3 and accept the USB debugging prompt.
3. Double-click `gnirehtet-vd.exe`.
4. Accept the VPN prompt inside the headset if it appears.
5. Open Virtual Desktop on the Quest.

The wired link switches on automatically. Its icon near the Windows clock is
green while it is on and gray while it is off. Right-click the icon to turn
the link on or off, run **Diagnose and fix**, or exit safely.

The app keeps trying quietly if the headset is not ready. Put on and unlock the
headset if Windows is waiting for USB debugging permission.

## Use v3.1.0 Legacy

1. Download and extract the v3.1.0 Legacy zip.
2. Connect the Quest 3 to the PC.
3. Put on the headset and accept the USB debugging prompt.
4. Double-click `gnirehtet-run.cmd`.
5. Accept the VPN prompt inside the headset.
6. Open Virtual Desktop on the Quest.

Keep the launcher window open while using the legacy version. It requires a
Java 11 or newer runtime.

## What success looks like

- The headset asks for VPN permission on the first start.
- A VPN key appears while the wired link is active.
- Virtual Desktop can reach the PC while Quest Wi-Fi is off.
- The wired link reconnects after the cable is reattached and USB debugging is
  accepted again.

If this wired link helps you, starring the project on GitHub makes it easier for
other Quest users to find.

![Quest VPN permission request](assets/request.jpg)

![Quest VPN key icon](assets/key.png)

## Quick fixes

### The headset is not found

Put on the Quest and look for the USB debugging prompt. Try another USB port or
data cable if Windows still cannot see it. Disconnect other Android devices
while setting this up.

### Virtual Desktop cannot find the PC

Make sure Virtual Desktop Streamer is running on Windows before opening Virtual
Desktop on the Quest. If you force-closed the Streamer, start it again normally.
The wired-link app will not start or restart Virtual Desktop for you.

### Performance suddenly becomes worse

First restart Virtual Desktop on the Quest. If that fixes it, the slowdown may
be inside the Virtual Desktop session rather than the cable link. If the wired
link itself is not working, choose **Diagnose and fix** from the tray icon.

### The cable was unplugged

Reconnect it, put on the headset, and approve USB debugging again if asked. The
wired link keeps trying to reconnect. Uncheck **Wired link** from the tray icon
if you want to return the Quest to its normal network setup.

### An older install will not update

Official v3.1.0 and v4.0.2 releases use the same project signing identity and
should update in place. Very old builds from another source may need to be
removed once before installing this project.

## Current or legacy?

Use **v4.0.2** for the current experience. Use **v3.1.0 Legacy** only if you
need the older Java version. Both remain available from the Releases page.

## Privacy

The app does not upload diagnostics or record the contents of your network
traffic. Support information is saved locally only when you ask for it.

This project is licensed under Apache License 2.0.
