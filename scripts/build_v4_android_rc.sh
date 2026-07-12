#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
output="${1:-$repo_root/dist/v4-android-rc}"
hev_revision="c6e4c72246fb0f20bda299f0efc7814bb3098d57"

require_env() {
    if [[ -z "${!1:-}" ]]; then
        echo "required release input is missing: $1" >&2
        exit 1
    fi
}

for name in \
    ANDROID_RELEASE_KEYSTORE_BASE64 \
    ANDROID_RELEASE_CERT_SHA256 \
    ORG_GRADLE_PROJECT_RELEASE_STORE_PASSWORD \
    ORG_GRADLE_PROJECT_RELEASE_KEY_ALIAS \
    ORG_GRADLE_PROJECT_RELEASE_KEY_PASSWORD; do
    require_env "$name"
done
for command in base64 git java keytool python3 unzip; do
    command -v "$command" >/dev/null || {
        echo "required command is unavailable: $command" >&2
        exit 1
    }
done

source_date_epoch="${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" show -s --format=%ct HEAD)}"
if [[ ! "$source_date_epoch" =~ ^[0-9]+$ ]]; then
    echo "SOURCE_DATE_EPOCH must be an integer" >&2
    exit 1
fi
export SOURCE_DATE_EPOCH="$source_date_epoch"
export CARGO_INCREMENTAL=0

temporary="$(mktemp -d)"
cleanup() {
    rm -rf "$temporary"
    unset ORG_GRADLE_PROJECT_RELEASE_STORE_FILE
}
trap cleanup EXIT
umask 077
keystore="$temporary/gnirehtet-release.jks"
base64_help="$(base64 --help 2>&1 || true)"
if [[ "$base64_help" == *"--decode"* ]]; then
    decode_flag='--decode'
else
    decode_flag='-D'
fi
if ! printf '%s' "$ANDROID_RELEASE_KEYSTORE_BASE64" | base64 "$decode_flag" >"$keystore"; then
    echo "release keystore is not valid base64" >&2
    exit 1
fi
if [[ ! -s "$keystore" ]]; then
    echo "release keystore decoded to an empty file" >&2
    exit 1
fi
export ORG_GRADLE_PROJECT_RELEASE_STORE_FILE="$keystore"

keytool -list \
    -keystore "$keystore" \
    -storepass:env ORG_GRADLE_PROJECT_RELEASE_STORE_PASSWORD \
    -alias "$ORG_GRADLE_PROJECT_RELEASE_KEY_ALIAS" >/dev/null

mkdir -p "$output"
rm -f "$output"/*

(
    cd "$repo_root/android-v4"
    bash scripts/fetch-hev.sh "$hev_revision"
    test "$(git -C .deps/hev-socks5-tunnel rev-parse HEAD)" = "$hev_revision"
    ./gradlew --no-daemon clean testDebugUnitTest lintRelease assembleRelease
)

first_apk="$repo_root/android-v4/app/build/outputs/apk/release/app-release.apk"
if [[ ! -f "$first_apk" ]]; then
    echo "signed release APK was not produced; signing inputs were rejected" >&2
    exit 1
fi
cp "$first_apk" "$output/gnirehtet-v4.apk"
"$repo_root/scripts/verify_v4_apk.sh" \
    "$output/gnirehtet-v4.apk" \
    "$ANDROID_RELEASE_CERT_SHA256"

python3 "$repo_root/scripts/generate_v4_native_sbom.py" \
    --repo-root "$repo_root" \
    --apk "$output/gnirehtet-v4.apk" \
    --source-date-epoch "$source_date_epoch" \
    --output "$output/gnirehtet-v4-android-native.cdx.json"
cp "$repo_root/android-v4/app/src/main/assets/THIRD_PARTY_NOTICES.md" \
    "$output/ANDROID_NATIVE_NOTICES.md"
cp "$repo_root/LICENSE" "$output/PROJECT_LICENSE.txt"

(
    cd "$repo_root/android-v4"
    ./gradlew --no-daemon clean assembleRelease
)
second_apk="$repo_root/android-v4/app/build/outputs/apk/release/app-release.apk"
"$repo_root/scripts/verify_v4_apk.sh" "$second_apk" "$ANDROID_RELEASE_CERT_SHA256"

first_hash="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$output/gnirehtet-v4.apk")"
second_hash="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$second_apk")"
payload_report="$temporary/apk-payload-reproducibility.txt"
python3 "$repo_root/scripts/compare_apk_payload.py" \
    "$output/gnirehtet-v4.apk" \
    "$second_apk" \
    --report "$payload_report"
{
    printf 'artifact=gnirehtet-v4.apk\n'
    printf 'first_sha256=%s\n' "$first_hash"
    printf 'second_sha256=%s\n' "$second_hash"
    printf 'byte_reproducible=%s\n' "$([[ "$first_hash" = "$second_hash" ]] && printf true || printf false)"
    cat "$payload_report"
} >"$output/ANDROID_REPRODUCIBILITY.txt"

python3 "$repo_root/scripts/write_sha256.py" \
    "$output" \
    --output "$output/SHA256SUMS"
