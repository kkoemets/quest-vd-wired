Gnirehtet For Quest 3 Link Cable And Virtual Desktop

Quick start

1. Make sure Meta Developer Mode is enabled for your Quest 3.
2. Connect the Quest 3 to the PC by USB / Link cable.
3. Put on the headset and accept the USB debugging prompt.
4. On Windows, run gnirehtet-run.cmd.
   It checks Java, adb, gnirehtet.jar, gnirehtet.apk, and Quest authorization
   before starting the tunnel.
5. If the launcher reports missing adb / platform-tools, press R in the
   launcher or run gnirehtet-repair.cmd.
6. If you previously installed another Gnirehtet APK and install fails, run:
   adb uninstall com.genymobile.gnirehtet

What to expect

- The launcher prints visible dependency and Quest connection status.
- The first launch asks the headset to allow the VPN connection.
- With Quest Wi-Fi turned off, the headset should still have internet access.
- Keep the launcher window open while using Virtual Desktop.

Included Windows helpers

- gnirehtet-run.cmd checks dependencies and starts Gnirehtet.
- gnirehtet-repair.cmd refreshes Android platform-tools in this folder.
- gnirehtet-launcher.cmd status shows the same dependency / Quest status
  without starting the tunnel.
- gnirehtet-get-adb.cmd downloads Google's official Android platform-tools
  into this folder. It does not ship adb in the release zip.

Notes

- If the cable is unplugged and plugged back in, approve the USB debugging
  prompt again if the headset asks for it.
- Java still needs to be installed separately if the launcher reports it
  missing.

More help

- Full project guide: README.md in the repository
- Developer/build notes: DEVELOP.md in the repository
