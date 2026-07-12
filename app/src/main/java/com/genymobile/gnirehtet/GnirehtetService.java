/*
 * Copyright (C) 2017 Genymobile
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package com.genymobile.gnirehtet;

import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.net.Network;
import android.net.VpnService;
import android.os.Build;
import android.os.Handler;
import android.os.Message;
import android.os.ParcelFileDescriptor;
import android.util.Log;

import java.io.FileDescriptor;
import java.io.IOException;
import java.io.PrintWriter;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.net.InetAddress;

public class GnirehtetService extends VpnService {

    public static final boolean VERBOSE = false;

    private static final String ACTION_START_VPN = "com.genymobile.gnirehtet.START_VPN";
    private static final String ACTION_CLOSE_VPN = "com.genymobile.gnirehtet.CLOSE_VPN";
    private static final String EXTRA_VPN_CONFIGURATION = "vpnConfiguration";

    private static final String TAG = GnirehtetService.class.getSimpleName();

    private static final InetAddress VPN_ADDRESS = Net.toInetAddress(new byte[] {10, 0, 0, 2});
    // magic value: higher (like 0x8000 or 0xffff) or lower (like 1500) values show poorer performances
    private static final int MTU = 0x4000;

    private final Notifier notifier = new Notifier(this);
    private final Handler handler = new RelayTunnelConnectionStateHandler(this);
    private final Object resourceLock = new Object();

    private ParcelFileDescriptor vpnInterface = null;
    private Forwarder forwarder;
    private boolean shuttingDown;
    private boolean shutdownComplete = true;

    public static void start(Context context, VpnConfiguration config) {
        Intent intent = new Intent(context, GnirehtetService.class);
        intent.setAction(ACTION_START_VPN);
        intent.putExtra(GnirehtetService.EXTRA_VPN_CONFIGURATION, config);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent);
        } else {
            context.startService(intent);
        }
    }

    public static void stop(Context context) {
        // Stopping the service invokes onDestroy(), which owns the shutdown transaction. This also
        // avoids starting an otherwise absent foreground service just to tell it to stop.
        context.stopService(new Intent(context, GnirehtetService.class));
    }

    static Intent createStopIntent(Context context) {
        Intent intent = new Intent(context, GnirehtetService.class);
        intent.setAction(ACTION_CLOSE_VPN);
        return intent;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent != null ? intent.getAction() : null;
        Log.d(TAG, "Received request " + action);
        if (ACTION_START_VPN.equals(action)) {
            if (isRunning()) {
                Log.d(TAG, "VPN already running, ignore START request");
            } else {
                VpnConfiguration config = intent.getParcelableExtra(EXTRA_VPN_CONFIGURATION);
                if (config == null) {
                    config = new VpnConfiguration();
                }
                startVpn(config);
            }
        } else if (ACTION_CLOSE_VPN.equals(action)) {
            close(VpnLifecycle.State.STOPPED, "explicit stop requested");
        } else if (!isRunning()) {
            stopSelf(startId);
        }
        return START_NOT_STICKY;
    }

    private boolean isRunning() {
        synchronized (resourceLock) {
            return vpnInterface != null && !shuttingDown;
        }
    }

    private void startVpn(VpnConfiguration config) {
        synchronized (resourceLock) {
            shuttingDown = false;
            shutdownComplete = false;
        }
        VpnLifecycle.transition(VpnLifecycle.State.STARTING, "establishing VPN");
        try {
            notifier.start();
            if (setupVpn(config)) {
                startForwarding();
            } else {
                close(VpnLifecycle.State.ERROR, VpnLifecycle.getDetail());
            }
        } catch (RuntimeException e) {
            Log.e(TAG, "Cannot start VPN", e);
            close(VpnLifecycle.State.ERROR, "VPN startup exception: " + e.getClass().getSimpleName());
        }
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private boolean setupVpn(VpnConfiguration config) {
        Builder builder = new Builder();
        builder.addAddress(VPN_ADDRESS, 32);
        builder.setSession(getString(R.string.app_name));

        CIDR[] routes = config.getRoutes();
        if (routes.length == 0) {
            // no routes defined, redirect the whole network traffic
            builder.addRoute("0.0.0.0", 0);
        } else {
            for (CIDR route : routes) {
                builder.addRoute(route.getAddress(), route.getPrefixLength());
            }
        }

        InetAddress[] dnsServers = config.getDnsServers();
        if (dnsServers.length == 0) {
            // no DNS server defined, use Google DNS
            builder.addDnsServer("8.8.8.8");
        } else {
            for (InetAddress dnsServer : dnsServers) {
                builder.addDnsServer(dnsServer);
            }
        }

        // non-blocking by default, but FileChannel is not selectable, that's stupid!
        // so switch to synchronous I/O to avoid polling
        builder.setBlocking(true);
        builder.setMtu(MTU);

        if (!configureAllowedApplication(builder, config)) {
            return false;
        }
        setUnmetered(builder);

        ParcelFileDescriptor establishedInterface = builder.establish();
        if (establishedInterface == null) {
            Log.w(TAG, "VPN starting failed, please retry");
            // establish() may return null if the application is not prepared or is revoked
            VpnLifecycle.transition(VpnLifecycle.State.ERROR, "VPN permission missing or revoked");
            return false;
        }

        synchronized (resourceLock) {
            vpnInterface = establishedInterface;
        }
        useDefaultUnderlyingNetworks();
        return true;
    }

    private boolean configureAllowedApplication(Builder builder, VpnConfiguration config) {
        if (config.isAllTraffic()) {
            Log.w(TAG, "Diagnostic all-traffic VPN mode enabled");
            return true;
        }
        String packageName = config.getAllowedApplication();
        try {
            builder.addAllowedApplication(packageName);
            Log.i(TAG, "Routing only " + packageName + " through the wired link");
            return true;
        } catch (PackageManager.NameNotFoundException e) {
            Log.e(TAG, "Allowed application is not installed: " + packageName, e);
            VpnLifecycle.transition(VpnLifecycle.State.ERROR, "Virtual Desktop is not installed");
            return false;
        }
    }

    /**
     * {@code Builder.setMetered(false)} was added in API 29, while this maintenance branch still
     * compiles against API 28. Reflection keeps the v3 build compatible and applies the Quest-safe
     * unmetered setting whenever the runtime supports it.
     */
    @SuppressWarnings("checkstyle:MagicNumber")
    private void setUnmetered(Builder builder) {
        if (Build.VERSION.SDK_INT < 29) {
            return;
        }
        try {
            Method setMetered = Builder.class.getMethod("setMetered", Boolean.TYPE);
            setMetered.invoke(builder, false);
        } catch (NoSuchMethodException | IllegalAccessException | InvocationTargetException | SecurityException e) {
            Log.w(TAG, "Cannot mark VPN as unmetered", e);
        }
    }

    @SuppressWarnings("checkstyle:MagicNumber")
    private void useDefaultUnderlyingNetworks() {
        if (Build.VERSION.SDK_INT >= 22) {
            // null is the documented system-managed default. The previous implementation supplied
            // the VPN itself as its own underlying network, which is not a real carrier network.
            boolean accepted = setUnderlyingNetworks((Network[]) null);
            if (!accepted) {
                Log.w(TAG, "System rejected the default underlying-network policy");
            } else {
                Log.d(TAG, "Underlying-network policy: system default");
            }
        } else {
            Log.w(TAG, "Cannot set underlying network, API version " + Build.VERSION.SDK_INT + " < 22");
        }
    }

    private void startForwarding() {
        Forwarder newForwarder;
        synchronized (resourceLock) {
            newForwarder = new Forwarder(this, vpnInterface.getFileDescriptor(), new RelayTunnelListener(handler));
            forwarder = newForwarder;
        }
        newForwarder.forward();
    }

    private void close(VpnLifecycle.State finalState, String detail) {
        Forwarder forwarderToStop;
        ParcelFileDescriptor interfaceToClose;
        synchronized (resourceLock) {
            if (shuttingDown) {
                return;
            }
            if (shutdownComplete) {
                stopSelf();
                return;
            }
            shuttingDown = true;
            VpnLifecycle.transition(VpnLifecycle.State.STOPPING, detail);
            forwarderToStop = forwarder;
            interfaceToClose = vpnInterface;
            forwarder = null;
            vpnInterface = null;
        }

        handler.removeCallbacksAndMessages(null);
        try {
            if (forwarderToStop != null) {
                forwarderToStop.stop();
            }
        } catch (RuntimeException e) {
            Log.w(TAG, "Cannot stop forwarding cleanly", e);
        } finally {
            if (interfaceToClose != null) {
                try {
                    interfaceToClose.close();
                } catch (IOException e) {
                    Log.w(TAG, "Cannot close VPN file descriptor", e);
                }
            }
            try {
                notifier.stop();
            } catch (RuntimeException e) {
                Log.w(TAG, "Cannot stop foreground notification", e);
            }
            synchronized (resourceLock) {
                shuttingDown = false;
                shutdownComplete = true;
            }
            VpnLifecycle.transition(finalState, detail);
            stopSelf();
        }
    }

    @Override
    public void onRevoke() {
        try {
            close(VpnLifecycle.State.STOPPED, "VPN permission revoked");
        } finally {
            super.onRevoke();
        }
    }

    @Override
    public void onDestroy() {
        try {
            close(VpnLifecycle.State.STOPPED, "service destroyed");
        } finally {
            super.onDestroy();
        }
    }

    @Override
    protected void dump(FileDescriptor fd, PrintWriter writer, String[] args) {
        boolean vpnFdOpen;
        synchronized (resourceLock) {
            vpnFdOpen = vpnInterface != null;
        }
        writer.println(VpnLifecycle.formatDumpLine(vpnFdOpen));
    }

    private static final class RelayTunnelConnectionStateHandler extends Handler {

        private final GnirehtetService vpnService;

        private RelayTunnelConnectionStateHandler(GnirehtetService vpnService) {
            this.vpnService = vpnService;
        }

        @Override
        public void handleMessage(Message message) {
            if (!vpnService.isRunning()) {
                // if the VPN is not running anymore, ignore obsolete events
                return;
            }
            switch (message.what) {
                case RelayTunnelListener.MSG_RELAY_TUNNEL_CONNECTED:
                    Log.d(TAG, "Relay tunnel connected");
                    VpnLifecycle.transition(VpnLifecycle.State.RUNNING, "relay connected");
                    vpnService.notifier.setFailure(false);
                    break;
                case RelayTunnelListener.MSG_RELAY_TUNNEL_DISCONNECTED:
                    Log.d(TAG, "Relay tunnel disconnected");
                    VpnLifecycle.transition(VpnLifecycle.State.DEGRADED, "waiting for relay");
                    vpnService.notifier.setFailure(true);
                    break;
                default:
            }
        }
    }
}
