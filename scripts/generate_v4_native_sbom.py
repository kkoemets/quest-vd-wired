#!/usr/bin/env python3
"""Generate the deterministic Android CycloneDX SBOM for v4.

The dependency graph is intentionally closed over the pinned HEV checkout and
the exact Gradle release runtime. Any dependency, revision, notice, or
project-patch drift is a hard failure.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path


HEV_REVISION = "c6e4c72246fb0f20bda299f0efc7814bb3098d57"
EXPECTED_COMPONENTS = {
    ".": {
        "name": "hev-socks5-tunnel",
        "revision": HEV_REVISION,
        "license": "MIT",
        "url": "https://github.com/heiher/hev-socks5-tunnel",
    },
    "src/core": {
        "name": "hev-socks5-core",
        "revision": "cbff465b916832455c1cb02f1f9e25a41062054d",
        "license": "MIT",
        "url": "https://github.com/heiher/hev-socks5-core",
    },
    "third-part/hev-task-system": {
        "name": "hev-task-system",
        "revision": "b1afa0e21fb4ed5a69560e78e54baf0efdebe171",
        "license": "MIT",
        "url": "https://github.com/heiher/hev-task-system",
    },
    "third-part/lwip": {
        "name": "lwip",
        "revision": "8c69dfbe537835d5f2a5fd8c08c859f667b108ea",
        "license": "BSD-3-Clause",
        "url": "https://github.com/heiher/lwip",
    },
    "third-part/yaml": {
        "name": "yaml",
        "revision": "efa36117a8646d26d12b58e05bac472d7854a70d",
        "license": "MIT",
        "url": "https://github.com/heiher/yaml",
    },
}
EXPECTED_JVM_COMPONENTS = {
    "org.jetbrains.kotlin:kotlin-stdlib:2.2.10": {
        "group": "org.jetbrains.kotlin",
        "name": "kotlin-stdlib",
        "version": "2.2.10",
        "license": "Apache-2.0",
        "url": "https://github.com/JetBrains/kotlin",
        "depends_on": ["org.jetbrains:annotations:13.0"],
    },
    "org.jetbrains:annotations:13.0": {
        "group": "org.jetbrains",
        "name": "annotations",
        "version": "13.0",
        "license": "Apache-2.0",
        "url": "https://github.com/JetBrains/java-annotations",
        "depends_on": [],
    },
}
RUNTIME_CLASSPATH_REPORT = Path(
    "android-v4/app/build/reports/release-runtime-classpath.txt"
)
PROJECT_PATCH_SCOPES = {
    "hev-lifecycle.patch": ".",
    "hev-split-udp-port.patch": ".",
    "hev-timeout-phases.patch": ".",
}
REQUIRED_PROJECT_PATCHES = frozenset(PROJECT_PATCH_SCOPES)


class VerificationError(RuntimeError):
    pass


def git(checkout: Path, *args: str) -> str:
    process = subprocess.run(
        ["git", "-C", str(checkout), *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if process.returncode != 0:
        detail = process.stderr.strip() or process.stdout.strip() or "git command failed"
        raise VerificationError(detail)
    return process.stdout.rstrip("\r\n")


def git_bytes(checkout: Path, *args: str) -> bytes:
    process = subprocess.run(
        ["git", "-C", str(checkout), *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.returncode != 0:
        detail = process.stderr.decode(errors="replace").strip() or "git command failed"
        raise VerificationError(detail)
    return process.stdout


def patched_files(patches: list[Path]) -> set[str]:
    files: set[str] = set()
    for patch in patches:
        for line in patch.read_text(encoding="utf-8").splitlines():
            if line.startswith("+++ b/"):
                files.add(line.removeprefix("+++ b/"))
    return files


def verify_patch_scope(checkout: Path, patches: list[Path], label: str) -> None:
    expected_files = patched_files(patches)
    if not expected_files:
        raise VerificationError(f"project patches for {label} contain no file changes")
    changed = set(
        git(checkout, "diff", "--ignore-submodules=all", "--name-only").splitlines()
    )
    staged = git(checkout, "diff", "--cached", "--name-only")
    untracked = git(checkout, "ls-files", "--others", "--exclude-standard")
    if changed != expected_files or staged or untracked:
        raise VerificationError(f"{label} contains changes outside its checked-in project patches")

    for patch in reversed(patches):
        reverse_check = subprocess.run(
            ["git", "-C", str(checkout), "apply", "--reverse", "--check", str(patch)],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if reverse_check.returncode != 0:
            raise VerificationError(f"project patch content drift detected: {patch.name}")

    with tempfile.TemporaryDirectory() as directory:
        expected = Path(directory)
        subprocess.run(
            ["git", "init", "--quiet", str(expected)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for relative in expected_files:
            destination = expected / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(git_bytes(checkout, "show", f"HEAD:{relative}"))
        for patch in patches:
            applied = subprocess.run(
                ["git", "-C", str(expected), "apply", str(patch)],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            if applied.returncode != 0:
                raise VerificationError(
                    f"cannot reconstruct checked-in project patch {patch.name}: "
                    f"{applied.stderr.strip()}"
                )
        for relative in expected_files:
            if (expected / relative).read_bytes() != (checkout / relative).read_bytes():
                raise VerificationError(f"project patch content drift detected in {label}: {relative}")


def verify_checkout(repo_root: Path) -> dict[str, str]:
    checkout = repo_root / "android-v4/.deps/hev-socks5-tunnel"
    patch_directory = repo_root / "android-v4/patches"
    patches = sorted(patch_directory.glob("hev-*.patch"))
    notices = repo_root / "android-v4/app/src/main/assets/THIRD_PARTY_NOTICES.md"
    if (
        not checkout.is_dir()
        or not notices.is_file()
        or {patch.name for patch in patches} != REQUIRED_PROJECT_PATCHES
    ):
        raise VerificationError("pinned HEV checkout, exact project patch set, or notices file is missing")

    revisions = {".": git(checkout, "rev-parse", "HEAD")}
    if revisions["."] != HEV_REVISION:
        raise VerificationError("HEV root revision drift detected")

    status_lines = git(checkout, "submodule", "status", "--recursive").splitlines()
    for line in status_lines:
        if not line or line[0] != " ":
            raise VerificationError("HEV submodule is uninitialized, conflicted, or revision-dirty")
        fields = line[1:].split()
        if len(fields) < 2:
            raise VerificationError("cannot parse HEV submodule status")
        revisions[fields[1]] = fields[0]

    if set(revisions) != set(EXPECTED_COMPONENTS):
        raise VerificationError("HEV dependency graph changed")
    patched_scopes = set(PROJECT_PATCH_SCOPES.values())
    for path, component in EXPECTED_COMPONENTS.items():
        if revisions[path] != component["revision"]:
            raise VerificationError(f"HEV dependency revision drift at {path}")
        if path != "." and path not in patched_scopes and git(checkout / path, "status", "--porcelain"):
            raise VerificationError(f"HEV dependency contains local changes at {path}")

    patch_by_name = {patch.name: patch for patch in patches}
    for scope in sorted(patched_scopes):
        scoped = [
            patch_by_name[name]
            for name, patch_scope in PROJECT_PATCH_SCOPES.items()
            if patch_scope == scope
        ]
        verify_patch_scope(checkout if scope == "." else checkout / scope, scoped, scope)

    notice_text = notices.read_text(encoding="utf-8")
    for component in EXPECTED_COMPONENTS.values():
        if component["revision"] not in notice_text:
            raise VerificationError(f"notices omit pinned revision for {component['name']}")
    for coordinate in EXPECTED_JVM_COMPONENTS:
        if coordinate not in notice_text:
            raise VerificationError(f"notices omit Gradle runtime dependency {coordinate}")
    if "TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION" not in notice_text:
        raise VerificationError("notices omit the Apache-2.0 license terms")
    return revisions


def verify_runtime_classpath_report(repo_root: Path) -> list[str]:
    report = repo_root / RUNTIME_CLASSPATH_REPORT
    if not report.is_file():
        raise VerificationError(
            "verified Gradle releaseRuntimeClasspath report is missing"
        )
    lines = report.read_text(encoding="utf-8").splitlines()
    expected = sorted(EXPECTED_JVM_COMPONENTS)
    if lines != expected:
        raise VerificationError(
            "Gradle releaseRuntimeClasspath differs from the closed Android SBOM graph"
        )
    return lines


def read_app_version(repo_root: Path) -> str:
    build_file = (repo_root / "android-v4/app/build.gradle.kts").read_text(encoding="utf-8")
    match = re.search(r'versionName\s*=\s*"([^"]+)"', build_file)
    if not match:
        raise VerificationError("cannot read Android v4 versionName")
    return match.group(1)


def iso_timestamp(source_date_epoch: int) -> str:
    return dt.datetime.fromtimestamp(source_date_epoch, tz=dt.timezone.utc).isoformat().replace("+00:00", "Z")


def component_ref(name: str, revision: str) -> str:
    return f"pkg:github/heiher/{name}@{revision}"


def maven_component_ref(component: dict[str, object]) -> str:
    return (
        f"pkg:maven/{component['group']}/{component['name']}"
        f"@{component['version']}"
    )


def build_bom(
    app_version: str,
    source_date_epoch: int,
    apk_sha256: str | None,
    patch_sha256: str,
    patch_names: list[str] | None = None,
) -> dict:
    app_ref = f"pkg:generic/gnirehtet-vd-android@{app_version}"
    hev_ref = component_ref("hev-socks5-tunnel", HEV_REVISION)
    components = []
    for path, definition in EXPECTED_COMPONENTS.items():
        revision = definition["revision"]
        name = definition["name"]
        components.append(
            {
                "type": "library",
                "bom-ref": component_ref(name, revision),
                "group": "heiher",
                "name": name,
                "version": revision,
                "purl": component_ref(name, revision),
                "licenses": [{"license": {"id": definition["license"]}}],
                "externalReferences": [{"type": "vcs", "url": definition["url"]}],
                "properties": [
                    {"name": "gnirehtet:source-path", "value": path},
                    {"name": "gnirehtet:git-revision", "value": revision},
                    {"name": "gnirehtet:compiled-into-apk", "value": "true"},
                ],
            }
        )

    for coordinate in sorted(EXPECTED_JVM_COMPONENTS):
        definition = EXPECTED_JVM_COMPONENTS[coordinate]
        reference = maven_component_ref(definition)
        components.append(
            {
                "type": "library",
                "bom-ref": reference,
                "group": definition["group"],
                "name": definition["name"],
                "version": definition["version"],
                "purl": reference,
                "licenses": [{"license": {"id": definition["license"]}}],
                "externalReferences": [
                    {"type": "vcs", "url": definition["url"]}
                ],
                "properties": [
                    {
                        "name": "gnirehtet:gradle-configuration",
                        "value": "releaseRuntimeClasspath",
                    },
                    {"name": "gnirehtet:gradle-coordinate", "value": coordinate},
                    {"name": "gnirehtet:compiled-into-apk", "value": "true"},
                ],
            }
        )

    app_component = {
        "type": "application",
        "bom-ref": app_ref,
        "group": "com.genymobile",
        "name": "gnirehtet",
        "version": app_version,
        "purl": app_ref,
        "properties": [
            {"name": "gnirehtet:application-id", "value": "com.genymobile.gnirehtet"},
            {"name": "gnirehtet:target-platform", "value": "android-arm64-v8a"},
            {"name": "gnirehtet:hev-project-patches-sha256", "value": patch_sha256},
            {
                "name": "gnirehtet:hev-project-patches",
                "value": ",".join(sorted(patch_names or REQUIRED_PROJECT_PATCHES)),
            },
        ],
    }
    if apk_sha256:
        app_component["hashes"] = [{"alg": "SHA-256", "content": apk_sha256}]

    graph_fingerprint = "|".join(
        [app_version, str(source_date_epoch), patch_sha256]
        + [definition["revision"] for definition in EXPECTED_COMPONENTS.values()]
        + sorted(EXPECTED_JVM_COMPONENTS)
        + ([apk_sha256] if apk_sha256 else [])
    )
    child_refs = [
        component_ref(definition["name"], definition["revision"])
        for path, definition in EXPECTED_COMPONENTS.items()
        if path != "."
    ]
    kotlin_ref = maven_component_ref(
        EXPECTED_JVM_COMPONENTS["org.jetbrains.kotlin:kotlin-stdlib:2.2.10"]
    )
    dependencies = [
        {"ref": app_ref, "dependsOn": [hev_ref, kotlin_ref]},
        {"ref": hev_ref, "dependsOn": child_refs},
    ] + [{"ref": child, "dependsOn": []} for child in child_refs]
    for coordinate in sorted(EXPECTED_JVM_COMPONENTS):
        definition = EXPECTED_JVM_COMPONENTS[coordinate]
        dependencies.append(
            {
                "ref": maven_component_ref(definition),
                "dependsOn": [
                    maven_component_ref(EXPECTED_JVM_COMPONENTS[dependency])
                    for dependency in definition["depends_on"]
                ],
            }
        )

    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, graph_fingerprint)}",
        "version": 1,
        "metadata": {
            "timestamp": iso_timestamp(source_date_epoch),
            "tools": {
                "components": [
                    {
                        "type": "application",
                        "name": "generate_v4_native_sbom.py",
                        "version": "1",
                    }
                ]
            },
            "component": app_component,
        },
        "components": components,
        "dependencies": dependencies,
    }


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def patch_set_sha256(patches: list[Path]) -> str:
    digest = hashlib.sha256()
    for patch in sorted(patches):
        digest.update(patch.name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(patch.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--apk", type=Path)
    parser.add_argument("--source-date-epoch", type=int, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repo_root = args.repo_root.resolve()
    verify_checkout(repo_root)
    verify_runtime_classpath_report(repo_root)
    apk_digest = None
    if args.apk:
        apk = args.apk.resolve()
        if not apk.is_file():
            raise VerificationError("APK for SBOM hashing is missing")
        apk_digest = sha256(apk)
    patches = list((repo_root / "android-v4/patches").glob("hev-*.patch"))
    patch_digest = patch_set_sha256(patches)
    bom = build_bom(
        read_app_version(repo_root),
        args.source_date_epoch,
        apk_digest,
        patch_digest,
        [patch.name for patch in patches],
    )
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(bom, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except VerificationError as error:
        print(f"SBOM verification failed: {error}", file=sys.stderr)
        raise SystemExit(1)
