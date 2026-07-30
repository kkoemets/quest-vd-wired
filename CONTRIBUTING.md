# Contributing to Quest VD Wired

Thanks for helping make wired Virtual Desktop sessions easier to set up and
more reliable. Bug reports, compatibility findings, documentation fixes, and
focused code changes are all welcome.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Choose the right place

- Ask setup and usage questions in
  [Q&A Discussions](https://github.com/kkoemets/quest-vd-wired/discussions/categories/q-a).
- Start larger proposals in
  [Ideas Discussions](https://github.com/kkoemets/quest-vd-wired/discussions/categories/ideas)
  before investing in an implementation.
- Use the [issue chooser](https://github.com/kkoemets/quest-vd-wired/issues/new/choose)
  for reproducible bugs and focused feature requests.
- Follow [SECURITY.md](SECURITY.md) instead of opening a public issue for a
  suspected vulnerability.

Search existing issues and discussions first. For a substantial change, agree
on the problem and scope with the maintainer before writing the patch.

## Project layout

The repository intentionally carries two product generations:

- `app/` and `relay-java/` contain the proven v3.0.1 Java baseline.
- `android-v4/` and `host-rust/` contain the current Quest 3 implementation.

Do not remove or rewrite the Java baseline without reviewable evidence that the
replacement covers its runtime and recovery behavior. See [DEVELOP.md](DEVELOP.md)
for the architecture, toolchain, signing, protocol, and release constraints.

## Development prerequisites

The current implementation uses:

- Rust 1.88 with Cargo, rustfmt, and Clippy for the Windows host;
- JDK 17, Android SDK 36, NDK 28.2.13676358, Bash, Git, and network access
  for Android v4 and its pinned HEV dependency;
- Python 3 for release-tool tests;
- JDK 11, Android SDK 28, and build tools 28.0.3 only when changing the legacy
  Java baseline.

Signing keys and passwords are never required for ordinary development. Keep
all signing material and generated support files outside version control.

## Run the relevant checks

For Rust host changes:

```console
cargo fmt --manifest-path host-rust/Cargo.toml --all -- --check
cargo clippy --manifest-path host-rust/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path host-rust/Cargo.toml --all-targets
```

For Android v4 changes:

```console
cd android-v4
ANDROID_HOME=/path/to/android-sdk JAVA_HOME=/path/to/jdk-17 \
  ./gradlew testDebugUnitTest lintDebug assembleDebug
```

For release-tool changes:

```console
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v
```

For legacy Java changes:

```properties
# local.properties
sdk.dir=/path/to/Android/sdk
```

```console
JAVA_HOME=/path/to/jdk-11 ./gradlew checkAll :relay-java:jar :app:assembleDebug
```

Run every check that covers the files you changed. Documentation-only changes
should still be reviewed for working links, accurate commands, and readable
Markdown.

Changes to USB, ADB, VPN, routing, reconnect, lifecycle, or tray behavior need
target-hardware evidence from a Quest 3 and Windows 10 or 11. Record what you
tested, including cable reconnect and the Wi-Fi-off success check. If hardware
validation was not possible, state that clearly instead of implying it passed.

## Pull requests

Keep each pull request focused on one problem. In the description:

- explain the user-visible problem and the chosen solution;
- link the related issue or discussion;
- list the exact checks and hardware scenarios you ran;
- call out privacy, security, protocol, packaging, or compatibility impact;
- update user or developer documentation when behavior changes.

New dependencies must have a compatible license and updated notices or SBOM
inputs where applicable. Never commit secrets, signing material, unredacted
support bundles, packet contents, destination addresses, account details, or
browsing history.

## License

By contributing, you agree that your contribution is licensed under the
[Apache License 2.0](LICENSE).
