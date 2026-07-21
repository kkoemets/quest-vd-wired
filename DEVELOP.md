# Development

The repository deliberately carries two product generations:

- `app/` and `relay-java/` are the supported v3.1 safety/performance baseline.
- `android-v4/` and `host-rust/` are the Quest 3/Virtual Desktop v4 Beta.

Do not delete the Java baseline until v4 has reviewable passing evidence from
the target Quest 3 and Windows hardware, including long runs and lifecycle
cycling.

## v3.1 build

The legacy Gradle build requires JDK 11, Android SDK platform 28, and build
tools 28.0.3. Point `local.properties` at the Android SDK:

```properties
sdk.dir=/path/to/Android/sdk
```

Build and test with:

```console
JAVA_HOME=/path/to/jdk-11 ./gradlew checkAll :relay-java:jar :app:assembleDebug
```

The release APK preserves application ID `com.genymobile.gnirehtet`. Release
signing uses Gradle properties and must use the existing fork key for in-place
upgrades:

```properties
RELEASE_STORE_FILE=/absolute/path/to/gnirehtet-release.jks
RELEASE_STORE_PASSWORD=...
RELEASE_KEY_ALIAS=...
RELEASE_KEY_PASSWORD=...
```

Signing files belong in `.local-signing/` or another private location and must
never be committed. `dist/`, root generated APKs, and local signing material are
ignored.

The root `release` script builds the v3.1 fallback bundle. Its Windows repair
script must remain fail-closed on a pinned official platform-tools checksum.

## Android v4 build

Android v4 uses Gradle 9.4.1, Android Gradle Plugin 9.2, JDK 17 language level,
compile/target SDK 36, NDK 28.2.13676358, min SDK 29, and arm64-v8a only.

```console
cd android-v4
ANDROID_HOME=/path/to/android-sdk JAVA_HOME=/path/to/jdk-17 \
  ./gradlew testDebugUnitTest lintDebug assembleDebug
```

The build fetches HEV Socks5Tunnel at
`c6e4c72246fb0f20bda299f0efc7814bb3098d57`, checks out its exact gitlinks,
and rejects a locally modified dependency tree. Notices are packaged from
`android-v4/app/src/main/assets/THIRD_PARTY_NOTICES.md`.

Android v4 keeps application ID `com.genymobile.gnirehtet` and accepts the same
release-signing Gradle properties as v3.1. Namespace changes do not change the
installed package identity.

## Rust v4 host

Use the release/CI toolchain, Rust 1.88:

```console
cargo fmt --manifest-path host-rust/Cargo.toml --all -- --check
cargo clippy --manifest-path host-rust/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path host-rust/Cargo.toml --all-targets
```

The host listeners must remain loopback-only. ADB operations must use one
configured executable, have deadlines, and roll back every partial mapping.
Explicit Stop is successful only after Android acknowledges descriptor closure
and all product mappings are absent. Unexpected host/cable loss must retain the
Quest VPN and move to `degraded`.

The ignored synthetic soak test is opt-in:

```console
cargo test --manifest-path host-rust/Cargo.toml --test synthetic_udp -- \
  --ignored --nocapture
```

## Protocol and dependency rules

`GNR4` is a clean break from v3. Header and message fixtures must match between
Kotlin and Rust. Reject bad magic, unknown versions/types, oversized frames,
and stale sessions before unbounded allocation.

Every release must include:

- Android and Windows build/test results
- protocol/parser property tests and fuzz results
- dependency and license scan output
- CycloneDX or SPDX SBOMs
- SHA-256 checksums
- reproducibility comparison for independently built artifacts
- raw hardware-gate evidence or a clear statement that v4 remains experimental

No remote diagnostics backend exists. The bounded local diagnostics recorder
must never write packet payloads, destinations, account details, or browsing
history to logs.

The trust boundaries, failure policy, local command-channel requirement, and
release supply-chain rules must remain enforced by code, tests, and workflows.

## v4.0 release

The local Android release build requires these environment variables:

- `ANDROID_RELEASE_KEYSTORE_BASE64`
- `ANDROID_RELEASE_STORE_PASSWORD`
- `ANDROID_RELEASE_KEY_ALIAS`
- `ANDROID_RELEASE_KEY_PASSWORD`
- `ANDROID_RELEASE_CERT_SHA256`

The Android build fails closed unless the certificate, application ID, version,
non-debuggable flag, arm64 ABI, native engine, and packaged notices all match.
It builds twice and compares the signed APK payload independently of the APK
signing block. The Windows build embeds that exact signed APK, verifies the byte
sequence in the executable, and packages SBOMs, notices, and SHA-256 manifests.

Local release-tool tests do not need signing material:

```console
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v
```

Never print, copy, or commit the decoded keystore or its passwords. Disposable
keys are suitable only for testing the release pipeline, not for an in-place
upgrade artifact.
