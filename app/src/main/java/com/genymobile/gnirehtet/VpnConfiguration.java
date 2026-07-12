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

import android.os.Parcel;
import android.os.Parcelable;

import java.net.InetAddress;
import java.net.UnknownHostException;

public class VpnConfiguration implements Parcelable {

    public static final String DEFAULT_ALLOWED_APPLICATION = "VirtualDesktop.Android";

    private final InetAddress[] dnsServers;
    private final CIDR[] routes;
    private final boolean allTraffic;
    private final String allowedApplication;

    public VpnConfiguration() {
        this(new InetAddress[0], new CIDR[0]);
    }

    public VpnConfiguration(InetAddress[] dnsServers, CIDR[] routes) {
        this(dnsServers, routes, false, DEFAULT_ALLOWED_APPLICATION);
    }

    public VpnConfiguration(InetAddress[] dnsServers, CIDR[] routes, boolean allTraffic, String allowedApplication) {
        this.dnsServers = dnsServers;
        this.routes = routes;
        this.allTraffic = allTraffic;
        this.allowedApplication = normalizeAllowedApplication(allowedApplication);
    }

    private VpnConfiguration(Parcel source) {
        int dnsCount = source.readInt();
        dnsServers = new InetAddress[dnsCount];
        try {
            for (int i = 0; i < dnsCount; ++i) {
                dnsServers[i] = InetAddress.getByAddress(source.createByteArray());
            }
        } catch (UnknownHostException e) {
            throw new AssertionError("Invalid address", e);
        }
        routes = source.createTypedArray(CIDR.CREATOR);
        allTraffic = source.readByte() != 0;
        allowedApplication = normalizeAllowedApplication(source.readString());
    }

    private static String normalizeAllowedApplication(String packageName) {
        if (packageName == null || packageName.trim().isEmpty()) {
            return DEFAULT_ALLOWED_APPLICATION;
        }
        return packageName.trim();
    }

    public InetAddress[] getDnsServers() {
        return dnsServers;
    }

    public CIDR[] getRoutes() {
        return routes;
    }

    public boolean isAllTraffic() {
        return allTraffic;
    }

    public String getAllowedApplication() {
        return allowedApplication;
    }

    @Override
    public void writeToParcel(Parcel dest, int flags) {
        dest.writeInt(dnsServers.length);
        for (InetAddress addr : dnsServers) {
            dest.writeByteArray(addr.getAddress());
        }
        dest.writeTypedArray(routes, 0);
        dest.writeByte((byte) (allTraffic ? 1 : 0));
        dest.writeString(allowedApplication);
    }

    @Override
    public int describeContents() {
        return 0;
    }

    public static final Creator<VpnConfiguration> CREATOR = new Creator<VpnConfiguration>() {
        @Override
        public VpnConfiguration createFromParcel(Parcel source) {
            return new VpnConfiguration(source);
        }

        @Override
        public VpnConfiguration[] newArray(int size) {
            return new VpnConfiguration[size];
        }
    };
}
