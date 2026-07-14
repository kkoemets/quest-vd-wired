from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import sanitize_rust_sbom  # noqa: E402


class RustSbomSanitizerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.manifest = self.root / "host-rust/Cargo.toml"
        member = self.root / "host-rust/crates/gnirehtet-vd/Cargo.toml"
        member.parent.mkdir(parents=True)
        self.manifest.write_text(
            "[workspace]\n"
            "members = [\"crates/gnirehtet-vd\"]\n"
            "\n"
            "[workspace.package]\n"
            "version = \"4.0.5\"\n",
            encoding="utf-8",
        )
        member.write_text(
            "[package]\n"
            "name = \"gnirehtet-vd\"\n"
            "version.workspace = true\n",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    @staticmethod
    def fixture() -> dict[str, object]:
        root_ref = "path+file:///Users/builder/work/quest-vd-wired/host-rust/crates/gnirehtet-vd#4.0.5"
        first_dependency = "registry+https://github.com/rust-lang/crates.io-index#alpha@1.0.0"
        second_dependency = "registry+https://github.com/rust-lang/crates.io-index#beta@2.0.0"
        return {
            "bomFormat": "CycloneDX",
            "specVersion": "1.5",
            "version": 1,
            "serialNumber": "urn:uuid:11111111-1111-4111-8111-111111111111",
            "metadata": {
                "timestamp": "2026-07-13T20:00:00Z",
                "component": {
                    "type": "application",
                    "bom-ref": root_ref,
                    "name": "gnirehtet-vd",
                    "version": "4.0.5",
                    "description": "Wired-link host",
                    "purl": "pkg:cargo/gnirehtet-vd@4.0.5?download_url=file://.",
                    "components": [
                        {
                            "type": "library",
                            "bom-ref": f"{root_ref} bin-target-0",
                            "name": "gnirehtet_vd",
                            "version": "4.0.5",
                            "purl": "pkg:cargo/gnirehtet-vd@4.0.5?download_url=file://.#src/lib.rs",
                        },
                        {
                            "type": "application",
                            "bom-ref": f"{root_ref} bin-target-1",
                            "name": "gnirehtet-vd",
                            "version": "4.0.5",
                            "purl": "pkg:cargo/gnirehtet-vd@4.0.5?download_url=file://.#src/main.rs",
                        },
                    ],
                },
            },
            "components": [
                {
                    "type": "library",
                    "bom-ref": second_dependency,
                    "name": "beta",
                    "version": "2.0.0",
                    "purl": "pkg:cargo/beta@2.0.0",
                },
                {
                    "type": "library",
                    "bom-ref": first_dependency,
                    "name": "alpha",
                    "version": "1.0.0",
                    "purl": "pkg:cargo/alpha@1.0.0",
                },
            ],
            "dependencies": [
                {"ref": second_dependency, "dependsOn": []},
                {"ref": root_ref, "dependsOn": [second_dependency, first_dependency]},
                {"ref": first_dependency, "dependsOn": []},
            ],
        }

    def test_output_is_deterministic_private_path_free_and_graph_preserving(self) -> None:
        first_input = self.fixture()
        second_input = copy.deepcopy(first_input)
        second_input["serialNumber"] = "urn:uuid:22222222-2222-4222-8222-222222222222"
        second_input["metadata"]["timestamp"] = "2026-07-14T01:02:03Z"  # type: ignore[index]
        second_input["components"].reverse()  # type: ignore[union-attr]
        second_input["dependencies"].reverse()  # type: ignore[union-attr]
        second_input["metadata"]["component"]["components"].reverse()  # type: ignore[index,union-attr]

        first = sanitize_rust_sbom.sanitize_document(first_input, self.manifest, self.root)
        second = sanitize_rust_sbom.sanitize_document(second_input, self.manifest, self.root)
        first_bytes = sanitize_rust_sbom.render_document(first)
        second_bytes = sanitize_rust_sbom.render_document(second)

        self.assertEqual(first_bytes, second_bytes)
        output = json.loads(first_bytes)
        self.assertNotIn("serialNumber", output)
        self.assertNotIn("timestamp", output["metadata"])
        root = output["metadata"]["component"]
        stable_root = "pkg:cargo/gnirehtet-vd@4.0.5"
        self.assertEqual("gnirehtet-vd", root["name"])
        self.assertEqual("4.0.5", root["version"])
        self.assertEqual(stable_root, root["bom-ref"])
        self.assertEqual(stable_root, root["purl"])
        self.assertEqual(
            [
                "pkg:cargo/gnirehtet-vd@4.0.5?target=application%3Agnirehtet-vd",
                "pkg:cargo/gnirehtet-vd@4.0.5?target=library%3Agnirehtet_vd",
            ],
            [component["bom-ref"] for component in root["components"]],
        )

        serialized = first_bytes.decode("utf-8")
        for forbidden in ("/Users/", "/private/var/", "path+file:", "file://", "C:\\\\"):
            self.assertNotIn(forbidden.casefold(), serialized.casefold())

        references = {stable_root, *(component["bom-ref"] for component in output["components"])}
        graph = {entry["ref"]: set(entry["dependsOn"]) for entry in output["dependencies"]}
        self.assertEqual(references, set(graph))
        self.assertTrue(all(dependency in references for edges in graph.values() for dependency in edges))
        self.assertEqual(
            {
                "registry+https://github.com/rust-lang/crates.io-index#alpha@1.0.0",
                "registry+https://github.com/rust-lang/crates.io-index#beta@2.0.0",
            },
            graph[stable_root],
        )

    def test_unexpected_private_references_fail_without_replacing_output(self) -> None:
        cases = (
            "/Users/example/private/file",
            "/private/var/folders/private/file",
            "/home/builder/private/file",
            "/tmp/private/file",
            "path+file:///tmp/private/file",
            "file:///tmp/private/file",
            r"C:\private\file",
            "D:/private/file",
            "/opt/release/repository/private/file",
        )
        for index, leaked_value in enumerate(cases):
            with self.subTest(leaked_value=leaked_value):
                source = self.fixture()
                source["components"][0]["description"] = leaked_value  # type: ignore[index]
                input_path = self.root / f"input-{index}.json"
                output_path = self.root / f"output-{index}.json"
                input_path.write_text(json.dumps(source), encoding="utf-8")
                output_path.write_bytes(b"existing-output")
                repository_root = (
                    Path("/opt/release/repository")
                    if leaked_value.startswith("/opt/release/repository")
                    else self.root
                )

                with self.assertRaises(sanitize_rust_sbom.SanitizationError):
                    sanitize_rust_sbom.sanitize_file(
                        input_path, output_path, self.manifest, repository_root
                    )
                self.assertEqual(b"existing-output", output_path.read_bytes())

    def test_dangling_dependency_is_rejected(self) -> None:
        source = self.fixture()
        source["dependencies"][0]["dependsOn"] = ["missing-component"]  # type: ignore[index]
        with self.assertRaisesRegex(
            sanitize_rust_sbom.SanitizationError, "unknown references"
        ):
            sanitize_rust_sbom.sanitize_document(source, self.manifest, self.root)

    def test_wrong_host_identity_is_rejected(self) -> None:
        source = self.fixture()
        source["metadata"]["component"]["version"] = "4.0.1"  # type: ignore[index]
        with self.assertRaisesRegex(
            sanitize_rust_sbom.SanitizationError, "host package identity"
        ):
            sanitize_rust_sbom.sanitize_document(source, self.manifest, self.root)

    def test_duplicate_nested_target_reference_is_rejected(self) -> None:
        source = self.fixture()
        targets = source["metadata"]["component"]["components"]  # type: ignore[index]
        targets[1]["bom-ref"] = targets[0]["bom-ref"]  # type: ignore[index]
        with self.assertRaisesRegex(
            sanitize_rust_sbom.SanitizationError, "duplicate root target reference"
        ):
            sanitize_rust_sbom.sanitize_document(source, self.manifest, self.root)

    def test_manual_windows_release_sanitizes_and_removes_one_exact_raw_sbom(self) -> None:
        release = (SCRIPTS / "build_v4_windows_rc.ps1").read_text(encoding="utf-8")
        expected_raw = (
            '$rawSbom = Join-Path $repoRoot '
            '"host-rust\\crates\\gnirehtet-vd\\gnirehtet-vd.cdx.json"'
        )
        self.assertIn(expected_raw, release)
        self.assertIn('"scripts\\sanitize_rust_sbom.py"', release)
        self.assertGreaterEqual(
            release.count("Remove-Item -LiteralPath $rawSbom -Force"),
            2,
        )
        self.assertNotIn('Get-ChildItem -LiteralPath (Join-Path $repoRoot "host-rust")', release)
        sanitizer_call = release.index('"scripts\\sanitize_rust_sbom.py"')
        cleanup = release.index("Remove-Item -LiteralPath $rawSbom -Force", sanitizer_call)
        self.assertGreater(cleanup, sanitizer_call)


if __name__ == "__main__":
    unittest.main()
