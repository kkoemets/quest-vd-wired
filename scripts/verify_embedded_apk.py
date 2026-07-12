#!/usr/bin/env python3
"""Fail unless an executable contains the exact signed APK byte sequence."""

from __future__ import annotations

import argparse
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("executable", type=Path)
    parser.add_argument("apk", type=Path)
    args = parser.parse_args()
    executable = args.executable.read_bytes()
    apk = args.apk.read_bytes()
    if len(apk) < 4 or apk[:2] != b"PK":
        parser.error("APK input is not a ZIP archive")
    occurrences = executable.count(apk)
    if occurrences != 1:
        parser.error(f"executable contains the signed APK {occurrences} times; expected exactly once")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
