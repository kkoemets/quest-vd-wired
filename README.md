# Wired Virtual Desktop for Meta Quest 3

Use Virtual Desktop on your Quest 3 through a USB cable instead of relying on
Quest Wi-Fi. The PC keeps its normal internet connection and shares it with the
headset through the cable.

This project is for one Quest 3 and a Windows 10 or Windows 11 PC. It does not
need administrator access, a special network driver, SideQuest, or a rooted
headset.

## Choose your download

### v3.1 Standard — recommended

This is the normal version for everyday use. It is the safer choice while the
new version is tested by more people.

[Download v3.1 Standard](https://github.com/kkoemets/gnirehtet-quest-3-virtual-desktop-link-cable/releases/download/v3.1.0/gnirehtet-java-v3.1.0.zip)

### v4.0 Beta — for testing

This is the new version with a simpler tray app and a redesigned connection
engine. It may still have bugs, so keep v3.1 Standard nearby.

[Download v4.0 Beta](https://github.com/kkoemets/gnirehtet-quest-3-virtual-desktop-link-cable/releases/download/v4.0.0-beta.1/gnirehtet-v4.0.0-beta.1-windows-x64.zip)

### v3.0.0 Legacy — older fallback

This is the previous release, kept available for users who need to roll back.
New users should choose v3.1 Standard instead.

[Download v3.0.0 Legacy](https://github.com/kkoemets/gnirehtet-quest-3-virtual-desktop-link-cable/releases/download/v3.0.0/gnirehtet-java-v3.0.0.zip)

If a download has not been published yet, check the
[Releases page](https://github.com/kkoemets/gnirehtet-quest-3-virtual-desktop-link-cable/releases).

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

## Use v3.1 Standard

1. Download and extract the v3.1 Standard zip.
2. Connect the Quest 3 to the PC.
3. Put on the headset and accept the USB debugging prompt.
4. Double-click `gnirehtet-run.cmd`.
5. If the launcher says a required Android tool is missing, press **R** to
   repair it, then start again.
6. Accept the VPN prompt inside the headset.
7. Open Virtual Desktop on the Quest.

Keep the launcher window open while using the wired link. If it reports that
Java is missing, install a current Java 11 or newer runtime and try again.

## Use v4.0 Beta

1. Download and extract the v4.0 Beta zip.
2. Connect the Quest 3 and accept the USB debugging prompt.
3. Double-click `gnirehtet-vd.exe`.
4. Find the Gnirehtet VD icon near the Windows clock.
5. Right-click the icon and select **Start wired link**.
6. Accept the VPN prompt inside the headset.
7. Open Virtual Desktop on the Quest.

If the Beta reports a missing Android tool, choose **Repair** from the same
tray menu and then try **Start wired link** again. To disconnect, choose
**Stop wired link** from the tray or from the Gnirehtet notification on the
Quest.

## What success looks like

- The headset asks for VPN permission on the first start.
- A VPN key appears while the wired link is active.
- Virtual Desktop can reach the PC while Quest Wi-Fi is off.
- The wired link reconnects after the cable is reattached and USB debugging is
  accepted again.

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
be inside the Virtual Desktop session rather than the cable link. Beta testers
can use the tray **Status** option and include that information in a bug report.

### The cable was unplugged

Reconnect it, put on the headset, and approve USB debugging again if asked. The
wired link keeps trying to reconnect. Use **Stop wired link** if you want to
return the Quest to its normal network setup.

### An older install will not update

Official v3.1 Standard and v4.0 Beta releases use the same project signing
identity and should update in place. Very old builds from another source may
need to be removed once before installing this project.

## Standard or Beta?

Use **v3.1 Standard** if you mainly want to play. Use **v4.0 Beta** if you are
comfortable reporting problems and switching back to Standard when needed.
Both versions stay available from the Releases page during the Beta period.

## Privacy

The app does not upload diagnostics or record the contents of your network
traffic. Support information is saved locally only when you ask for it.

This project is licensed under Apache License 2.0.
