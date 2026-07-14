# Quest VD Wired: Virtual Desktop over USB-C on Meta Quest 3

Quest VD Wired is a free, open-source Windows app that gives Virtual Desktop a
wired connection through a USB 3 or link cable instead of Quest Wi-Fi. It is
based on the open-source gnirehtet project and made specifically for Quest 3.

It works with Windows 10 and Windows 11. It does not need administrator access,
a special network driver, SideQuest, or a rooted headset.

## Download for Windows

[**Download Quest VD Wired v4.1.0 for Windows 10/11**](https://github.com/kkoemets/quest-vd-wired/releases/download/v4.1.0/quest-vd-wired-v4.1.0-windows-x64.zip)

Extract the ZIP, connect your Quest 3, and double-click `quest-vd-wired.exe`.

If an older version is already open, choose **Exit** from its tray icon before
starting v4.1.0.

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

## Get connected

1. Download and extract the ZIP.
2. Connect the Quest 3 and accept the USB debugging prompt.
3. Double-click `quest-vd-wired.exe`.
4. Accept the VPN prompt inside the headset if it appears.
5. Open Virtual Desktop on the Quest.

The wired link switches on automatically. Its icon near the Windows clock is
green while it is on and gray while it is off. Right-click the icon to turn the
link on or off, run **Diagnose and fix**, or exit safely.

The app keeps trying quietly if the headset is not ready. Put on and unlock the
headset if Windows is waiting for USB debugging permission.

![Quest VD Wired tray menu](https://github.com/user-attachments/assets/2ce519de-3997-47e5-ac38-84eb974fd804)

## What success looks like

- The headset asks for VPN permission on the first start.
- A VPN key appears while the wired link is active.
- Virtual Desktop can reach the PC while Quest Wi-Fi is off.
- The wired link reconnects after the cable is reattached and USB debugging is
  accepted again.

If this wired link helps you, [star the project](https://github.com/kkoemets/quest-vd-wired) and share the repository link so other Quest users can find it.

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
Quest VD Wired will not start or restart Virtual Desktop for you.

### Performance suddenly becomes worse

First restart Virtual Desktop on the Quest. If that fixes it, the slowdown may
be inside the Virtual Desktop session rather than the cable link. If the wired
link itself is not working, choose **Diagnose and fix** from the tray icon.

### The cable was unplugged

Reconnect it, put on the headset, and approve USB debugging again if asked. The
wired link keeps trying to reconnect. Uncheck **Wired link** from the tray icon
if you want to return the Quest to its normal network setup.

### Still need help?

[Ask in GitHub Discussions](https://github.com/kkoemets/quest-vd-wired/discussions/categories/q-a).

## Frequently asked questions

### Can Virtual Desktop use a USB cable on Quest 3?

Yes, with Quest VD Wired. It creates a wired network connection through the
cable so Virtual Desktop does not have to rely on Quest Wi-Fi.

### Do I still need Virtual Desktop and Virtual Desktop Streamer?

Yes. Virtual Desktop must be installed on the Quest, and Virtual Desktop
Streamer must be running on the Windows PC. Quest VD Wired provides the wired
connection between them.

### Does this use Meta Quest Link?

No. This is a separate community-made connection for Virtual Desktop. It does
not install or replace Meta Quest Link.

### Can it work with Quest Wi-Fi turned off?

Yes. The PC keeps its normal internet connection and shares it with the Quest
through the USB cable.

### Is Quest VD Wired official?

No. It is an independent, open-source community project. It is not made by or
affiliated with Meta or Virtual Desktop.

## Older versions

### v3.1.0 Legacy

This is the older Java version. New users should choose the current release.

[Download v3.1.0 Legacy](https://github.com/kkoemets/quest-vd-wired/releases/download/v3.1.0/gnirehtet-java-v3.1.0.zip)

### v3.0.1 Legacy fallback

Use this only as an additional fallback if you need the older release.

[Download v3.0.1 Legacy fallback](https://github.com/kkoemets/quest-vd-wired/releases/download/v3.0.1/gnirehtet-java-v3.0.1.zip)

## Privacy

The app does not upload diagnostics or record the contents of your network
traffic. Support information is saved locally only when you ask for it.

Quest VD Wired is based on [Genymobile's gnirehtet](https://github.com/Genymobile/gnirehtet).
Virtual Desktop is required separately. This unofficial community project is
not affiliated with Meta or Virtual Desktop.

Licensed under the Apache License 2.0.
