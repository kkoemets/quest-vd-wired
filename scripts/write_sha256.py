#!/usr/bin/env python3
"""Write deterministic GNU-style SHA-256 entries for a release directory."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.directory.resolve()
    output = args.output.resolve()
    files = sorted(
        path for path in root.rglob("*") if path.is_file() and path.resolve() != output
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        "".join(f"{digest(path)}  {path.relative_to(root).as_posix()}\n" for path in files),
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
