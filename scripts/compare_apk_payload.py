#!/usr/bin/env python3
"""Compare reproducible APK ZIP payloads while excluding the APK signing block."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import zipfile
from pathlib import Path


def payload_digest(path: Path) -> tuple[str, list[str]]:
    digest = hashlib.sha256()
    names: list[str] = []
    with zipfile.ZipFile(path) as archive:
        for entry in archive.infolist():
            names.append(entry.filename)
            metadata = {
                "filename": entry.filename,
                "date_time": entry.date_time,
                "compress_type": entry.compress_type,
                "comment": entry.comment.hex(),
                "extra": entry.extra.hex(),
                "create_system": entry.create_system,
                "create_version": entry.create_version,
                "extract_version": entry.extract_version,
                "external_attr": entry.external_attr,
                "flag_bits": entry.flag_bits,
                "internal_attr": entry.internal_attr,
                "volume": entry.volume,
            }
            content = archive.read(entry)
            digest.update(json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode("utf-8"))
            digest.update(b"\0")
            digest.update(hashlib.sha256(content).digest())
    return digest.hexdigest(), names


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    first_digest, first_names = payload_digest(args.first)
    second_digest, second_names = payload_digest(args.second)
    same = first_digest == second_digest and first_names == second_names
    report = (
        f"first_payload_sha256={first_digest}\n"
        f"second_payload_sha256={second_digest}\n"
        f"payload_reproducible={str(same).lower()}\n"
    )
    if args.report:
        args.report.write_text(report, encoding="utf-8")
    else:
        sys.stdout.write(report)
    return 0 if same else 1


if __name__ == "__main__":
    raise SystemExit(main())
