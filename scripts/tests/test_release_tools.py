from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import unittest
import zipfile
import struct
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
REPOSITORY = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

import generate_rust_notices  # noqa: E402
import generate_v4_native_sbom  # noqa: E402


class NativeSbomTest(unittest.TestCase):
    def test_bom_is_deterministic_and_has_closed_graph(self) -> None:
        first = generate_v4_native_sbom.build_bom("4.0.0-beta.1", 1_700_000_000, "a" * 64, "b" * 64)
        second = generate_v4_native_sbom.build_bom("4.0.0-beta.1", 1_700_000_000, "a" * 64, "b" * 64)
        self.assertEqual(first, second)
        self.assertEqual("CycloneDX", first["bomFormat"])
        self.assertEqual("1.6", first["specVersion"])
        self.assertEqual(5, len(first["components"]))
        root = first["metadata"]["component"]
        self.assertEqual("com.genymobile", root["group"])
        self.assertEqual("a" * 64, root["hashes"][0]["content"])
        dependency_refs = {entry["ref"] for entry in first["dependencies"]}
        self.assertIn(root["bom-ref"], dependency_refs)


class RustNoticeTest(unittest.TestCase):
    def test_notice_contains_resolved_license_text(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            package_dir = Path(directory) / "dependency"
            package_dir.mkdir()
            manifest = package_dir / "Cargo.toml"
            manifest.write_text("[package]\nname='example'\nversion='1.0.0'\n", encoding="utf-8")
            (package_dir / "LICENSE-MIT").write_text("sample <license>", encoding="utf-8")
            package_id = "registry+https://example.invalid#index/example@1.0.0"
            metadata = {
                "packages": [
                    {
                        "id": package_id,
                        "name": "example",
                        "version": "1.0.0",
                        "source": "registry+https://example.invalid/index",
                        "license": "MIT",
                        "license_file": None,
                        "manifest_path": str(manifest),
                    }
                ],
                "resolve": {"nodes": [{"id": package_id}]},
            }
            output = generate_rust_notices.render_notices(metadata)
            self.assertIn("## example 1.0.0", output)
            self.assertIn("sample &lt;license&gt;", output)

    def test_notice_records_metadata_when_crate_omits_license_text(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text("", encoding="utf-8")
            package_id = "registry+https://example.invalid#index/example@1.0.0"
            metadata = {
                "packages": [
                    {
                        "id": package_id,
                        "name": "example",
                        "version": "1.0.0",
                        "source": "registry+https://example.invalid/index",
                        "license": "MIT",
                        "license_file": None,
                        "manifest_path": str(manifest),
                    }
                ],
                "resolve": {"nodes": [{"id": package_id}]},
            }
            output = generate_rust_notices.render_notices(metadata)
            self.assertIn("does not contain a standalone license file", output)


class CommandLineToolsTest(unittest.TestCase):
    def test_android_release_builder_fails_closed_without_signing_secrets(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            environment = os.environ.copy()
            for name in (
                "ANDROID_RELEASE_KEYSTORE_BASE64",
                "ANDROID_RELEASE_CERT_SHA256",
                "ORG_GRADLE_PROJECT_RELEASE_STORE_PASSWORD",
                "ORG_GRADLE_PROJECT_RELEASE_KEY_ALIAS",
                "ORG_GRADLE_PROJECT_RELEASE_KEY_PASSWORD",
            ):
                environment.pop(name, None)
            process = subprocess.run(
                ["bash", str(SCRIPTS / "build_v4_android_rc.sh"), directory],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                env=environment,
            )
            self.assertNotEqual(0, process.returncode)
            self.assertIn("required release input is missing", process.stderr)

    def test_checksum_writer_is_sorted_and_self_excluding(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "z.txt").write_text("z", encoding="utf-8")
            (root / "a.txt").write_text("a", encoding="utf-8")
            output = root / "SHA256SUMS"
            subprocess.run(
                [sys.executable, str(SCRIPTS / "write_sha256.py"), str(root), "--output", str(output)],
                check=True,
            )
            names = [line.split("  ", 1)[1] for line in output.read_text(encoding="utf-8").splitlines()]
            self.assertEqual(["a.txt", "z.txt"], names)

    def test_embedded_apk_requires_one_exact_copy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            apk = root / "app.apk"
            executable = root / "host.exe"
            apk.write_bytes(b"PK\x03\x04signed-apk")
            executable.write_bytes(b"prefix" + apk.read_bytes() + b"suffix")
            subprocess.run(
                [sys.executable, str(SCRIPTS / "verify_embedded_apk.py"), str(executable), str(apk)],
                check=True,
            )

    def test_apk_payload_comparison_excludes_signing_block_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first.apk"
            second = root / "second.apk"
            with zipfile.ZipFile(first, "w") as archive:
                archive.writestr("classes.dex", b"payload")
            original = first.read_bytes()
            eocd = original.rfind(b"PK\x05\x06")
            central_offset = struct.unpack_from("<I", original, eocd + 16)[0]
            signing_bytes = b"different-signing-block"
            modified = bytearray(original[:central_offset] + signing_bytes + original[central_offset:])
            struct.pack_into("<I", modified, eocd + len(signing_bytes) + 16, central_offset + len(signing_bytes))
            second.write_bytes(modified)
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "compare_apk_payload.py"),
                    str(first),
                    str(second),
                ],
                check=True,
            )

    def test_apk_verifier_checks_exact_release_identity_and_notices(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk = root / "sdk"
            build_tools = sdk / "build-tools/36.0.0"
            command_tools = sdk / "cmdline-tools/latest/bin"
            build_tools.mkdir(parents=True)
            command_tools.mkdir(parents=True)
            digest = "0" * 64
            apksigner = build_tools / "apksigner"
            apksigner.write_text(
                f"#!/bin/sh\necho 'Signer #1 certificate SHA-256 digest: {digest}'\n",
                encoding="utf-8",
            )
            apkanalyzer = command_tools / "apkanalyzer"
            apkanalyzer.write_text(
                "#!/bin/sh\n"
                "case \"$2\" in\n"
                "application-id) echo com.genymobile.gnirehtet ;;\n"
                "version-code) echo 41 ;;\n"
                "version-name) echo 4.0.0-beta.2 ;;\n"
                "min-sdk) echo 29 ;;\n"
                "target-sdk) echo 36 ;;\n"
                "debuggable) echo false ;;\n"
                "*) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            apksigner.chmod(0o755)
            apkanalyzer.chmod(0o755)
            apk = root / "release.apk"
            notices = (SCRIPTS.parent / "android-v4/app/src/main/assets/THIRD_PARTY_NOTICES.md").read_bytes()
            with zipfile.ZipFile(apk, "w") as archive:
                archive.writestr("lib/arm64-v8a/libhev-socks5-tunnel.so", b"native")
                archive.writestr("assets/THIRD_PARTY_NOTICES.md", notices)
            environment = os.environ.copy()
            environment["ANDROID_HOME"] = str(sdk)
            environment["ANDROID_SDK_ROOT"] = str(sdk)
            subprocess.run(
                ["bash", str(SCRIPTS / "verify_v4_apk.sh"), str(apk), digest],
                check=True,
                env=environment,
            )

    def test_v3_apk_verifier_checks_standard_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk = root / "sdk"
            build_tools = sdk / "build-tools/36.0.0"
            command_tools = sdk / "cmdline-tools/latest/bin"
            build_tools.mkdir(parents=True)
            command_tools.mkdir(parents=True)
            digest = "1" * 64
            apksigner = build_tools / "apksigner"
            apksigner.write_text(
                f"#!/bin/sh\necho 'Signer #1 certificate SHA-256 digest: {digest}'\n",
                encoding="utf-8",
            )
            apkanalyzer = command_tools / "apkanalyzer"
            apkanalyzer.write_text(
                "#!/bin/sh\n"
                "case \"$2\" in\n"
                "application-id) echo com.genymobile.gnirehtet ;;\n"
                "version-code) echo 11 ;;\n"
                "version-name) echo 3.1.0 ;;\n"
                "min-sdk) echo 21 ;;\n"
                "target-sdk) echo 29 ;;\n"
                "debuggable) echo false ;;\n"
                "*) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            apksigner.chmod(0o755)
            apkanalyzer.chmod(0o755)
            apk = root / "standard.apk"
            apk.write_bytes(b"signed-v3-apk")
            environment = os.environ.copy()
            environment["ANDROID_HOME"] = str(sdk)
            environment["ANDROID_SDK_ROOT"] = str(sdk)
            subprocess.run(
                ["bash", str(SCRIPTS / "verify_v3_apk.sh"), str(apk), digest],
                check=True,
                env=environment,
            )


class ReleasePolicyTest(unittest.TestCase):
    def test_gradle_distributions_are_checksum_pinned(self) -> None:
        for properties in (
            REPOSITORY / "gradle/wrapper/gradle-wrapper.properties",
            REPOSITORY / "android-v4/gradle/wrapper/gradle-wrapper.properties",
        ):
            text = properties.read_text(encoding="utf-8")
            match = re.search(r"^distributionSha256Sum=([0-9a-f]{64})$", text, re.MULTILINE)
            self.assertIsNotNone(match, f"unverified Gradle distribution: {properties}")

    def test_github_actions_remain_disabled(self) -> None:
        workflows = sorted((REPOSITORY / ".github/workflows").glob("*.yml"))
        self.assertEqual(workflows, [])

    def test_comparator_is_locked_audited_and_not_released(self) -> None:
        comparator = (
            REPOSITORY / "benchmarks/comparators/tun2proxy/Cargo.toml"
        ).read_text(encoding="utf-8")
        lock = (REPOSITORY / "benchmarks/comparators/tun2proxy/Cargo.lock").read_text(
            encoding="utf-8"
        )
        notices = (
            REPOSITORY / "benchmarks/comparators/tun2proxy/THIRD_PARTY_NOTICES.md"
        ).read_text(encoding="utf-8")
        release = (REPOSITORY / "scripts/build_v4_windows_rc.ps1").read_text(
            encoding="utf-8"
        )
        revision = "eed123fbbec06295bf83f9be36d5a0f64ed9a8cb"
        self.assertIn(revision, comparator)
        self.assertIn(revision, lock)
        self.assertIn("GPL-3.0-or-later", notices)
        self.assertIn("WTFPL", notices)
        product_policy = (REPOSITORY / "host-rust/deny.toml").read_text(encoding="utf-8")
        self.assertNotIn("GPL-3.0-or-later", product_policy)
        self.assertNotIn("WTFPL", product_policy)
        self.assertNotIn("benchmarks/comparators", release)
        self.assertIn('"--locked"', release)
        self.assertIn("target-feature=+crt-static", release)

    def test_standard_and_beta_rollout_is_publishable_and_user_facing(self) -> None:
        readme = (REPOSITORY / "README.md").read_text(encoding="utf-8")
        android_v4 = (REPOSITORY / "android-v4/app/build.gradle.kts").read_text(
            encoding="utf-8"
        )
        rust_v4 = (REPOSITORY / "host-rust/Cargo.toml").read_text(encoding="utf-8")
        ignore = (REPOSITORY / ".gitignore").read_text(encoding="utf-8")

        self.assertIn("v3.1 Standard", readme)
        self.assertIn("v4.0 Beta", readme)
        self.assertIn("gnirehtet-java-v3.1.0.zip", readme)
        self.assertIn("gnirehtet-v4.0.0-beta.1-windows-x64.zip", readme)
        self.assertIn("gnirehtet-java-v3.0.0.zip", readme)
        self.assertNotIn("docs/", readme)
        self.assertIn("/docs/", ignore)
        self.assertTrue((REPOSITORY / "release").is_file())
        self.assertTrue((REPOSITORY / "scripts/build_v4_android_rc.sh").is_file())
        self.assertTrue((REPOSITORY / "scripts/build_v4_windows_rc.ps1").is_file())
        self.assertIn('versionName = "4.0.0-beta.2"', android_v4)
        self.assertIn('version = "4.0.0-beta.2"', rust_v4)


if __name__ == "__main__":
    unittest.main()
