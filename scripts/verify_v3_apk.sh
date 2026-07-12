#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: verify_v3_apk.sh APK EXPECTED_CERT_SHA256" >&2
    exit 2
fi

apk="$1"
expected_cert="$(printf '%s' "$2" | tr -d ':[:space:]' | tr '[:upper:]' '[:lower:]')"
sdk_root="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}"
build_tools="${ANDROID_BUILD_TOOLS_VERSION:-36.0.0}"

if [[ ! -f "$apk" ]]; then
    echo "signed v3.1 APK is missing" >&2
    exit 1
fi
if [[ ! "$expected_cert" =~ ^[0-9a-f]{64}$ ]]; then
    echo "expected signing certificate SHA-256 is missing or malformed" >&2
    exit 1
fi
if [[ -z "$sdk_root" ]]; then
    echo "ANDROID_SDK_ROOT or ANDROID_HOME is required" >&2
    exit 1
fi

apksigner="$sdk_root/build-tools/$build_tools/apksigner"
apkanalyzer="$sdk_root/cmdline-tools/latest/bin/apkanalyzer"
if [[ ! -x "$apksigner" || ! -x "$apkanalyzer" ]]; then
    echo "Android verification tools are incomplete" >&2
    exit 1
fi

verification="$(mktemp)"
trap 'rm -f "$verification"' EXIT
"$apksigner" verify --verbose --print-certs "$apk" >"$verification"
signer_digests="$(
    sed -n 's/^Signer #[0-9][0-9]* certificate SHA-256 digest: //p' "$verification" \
        | tr -d ':[:space:]' \
        | tr '[:upper:]' '[:lower:]'
)"
if [[ "$signer_digests" != "$expected_cert" ]]; then
    echo "APK signing certificate does not match the expected project identity" >&2
    exit 1
fi

package_name="$("$apkanalyzer" manifest application-id "$apk")"
version_code="$("$apkanalyzer" manifest version-code "$apk")"
version_name="$("$apkanalyzer" manifest version-name "$apk")"
min_sdk="$("$apkanalyzer" manifest min-sdk "$apk")"
target_sdk="$("$apkanalyzer" manifest target-sdk "$apk")"
debuggable="$("$apkanalyzer" manifest debuggable "$apk" | tr '[:upper:]' '[:lower:]')"

if [[ "$package_name" != "com.genymobile.gnirehtet" ]]; then
    echo "APK application identity is incorrect" >&2
    exit 1
fi
if [[ "$version_code" != "11" || "$version_name" != "3.1.0" ]]; then
    echo "APK version is not the exact v3.1 Standard identity" >&2
    exit 1
fi
if [[ "$min_sdk" != "21" || "$target_sdk" != "29" ]]; then
    echo "APK Android version support is incorrect" >&2
    exit 1
fi
if [[ "$debuggable" != "false" ]]; then
    echo "v3.1 Standard APK is debuggable" >&2
    exit 1
fi
