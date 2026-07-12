#!/usr/bin/env python3
"""Generate deterministic third-party notices from locked Cargo metadata."""

from __future__ import annotations

import argparse
import html
import json
import subprocess
import sys
from pathlib import Path


class NoticeError(RuntimeError):
    pass


def license_files(package: dict) -> list[Path]:
    manifest_dir = Path(package["manifest_path"]).resolve().parent
    candidates: set[Path] = set()
    declared = package.get("license_file")
    if declared:
        path = Path(declared)
        candidates.add(path if path.is_absolute() else manifest_dir / path)
    for path in manifest_dir.iterdir():
        upper = path.name.upper()
        if path.is_file() and upper.startswith(("LICENSE", "COPYING", "NOTICE")):
            candidates.add(path)
    return sorted(path.resolve() for path in candidates if path.is_file())


def dependency_packages(metadata: dict) -> list[dict]:
    resolved_ids = {node["id"] for node in metadata.get("resolve", {}).get("nodes", [])}
    packages = [
        package
        for package in metadata["packages"]
        if package.get("source") is not None and package["id"] in resolved_ids
    ]
    return sorted(packages, key=lambda package: (package["name"].casefold(), package["version"], package["id"]))


def render_notices(metadata: dict) -> str:
    sections = [
        "# Rust third-party notices\n\n",
        "Generated from `cargo metadata --locked`. The release gate separately enforces "
        "the repository license policy with `cargo deny`.\n\n",
    ]
    packages = dependency_packages(metadata)
    if not packages:
        raise NoticeError("Cargo metadata contains no resolved third-party dependencies")
    for package in packages:
        expression = package.get("license")
        if not expression:
            raise NoticeError(f"{package['name']} {package['version']} has no SPDX license expression")
        files = license_files(package)
        sections.extend(
            [
                f"## {package['name']} {package['version']}\n\n",
                f"SPDX expression: `{expression}`  \n",
                f"Source: `{package['source']}`\n\n",
            ]
        )
        if not files:
            sections.append(
                "The published crate does not contain a standalone license file; "
                "the SPDX expression above is taken from its locked Cargo metadata.\n\n"
            )
        for path in files:
            text = path.read_text(encoding="utf-8", errors="replace")
            if len(text.encode("utf-8")) > 2 * 1024 * 1024:
                raise NoticeError(f"license file is unexpectedly large: {path}")
            sections.extend(
                [
                    f"### {path.name}\n\n",
                    "<pre>\n",
                    html.escape(text.rstrip()),
                    "\n</pre>\n\n",
                ]
            )
    return "".join(sections)


def cargo_metadata(manifest: Path) -> dict:
    process = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
            str(manifest),
        ],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if process.returncode != 0:
        raise NoticeError(process.stderr.strip() or "cargo metadata failed")
    return json.loads(process.stdout)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    notices = render_notices(cargo_metadata(args.manifest.resolve()))
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(notices, encoding="utf-8")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except NoticeError as error:
        print(f"notice generation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
