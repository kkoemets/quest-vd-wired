#!/usr/bin/env python3
"""Verify that a Windows release executable contains no local build paths."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path
from typing import Iterable, Sequence


class VerificationError(RuntimeError):
    pass


SENSITIVE_PATH_PATTERNS = (
    (
        "POSIX user home",
        re.compile(r"(?i)/(?:users|home)/[^/\x00\r\n]+/"),
    ),
    (
        "POSIX temporary directory",
        re.compile(r"(?i)/(?:private/)?var/(?:folders|tmp)/|/tmp/"),
    ),
    (
        "Windows user home",
        re.compile(r"(?i)(?:[a-z]:)?[\\/]+users[\\/]+[^\\/\x00\r\n]+[\\/]"),
    ),
    (
        "Windows temporary directory",
        re.compile(
            r"(?i)(?:[a-z]:)?[\\/][^\x00\r\n]{0,240}"
            r"[\\/]appdata[\\/]local[\\/]temp[\\/]"
        ),
    ),
)
FORBIDDEN_RUNTIME_IMPORT = re.compile(
    r"(?i)^(?:api-ms-win-crt-[^.]+|concrt\d*|vcomp\d*|vcruntime\d*|msvcp\d*|"
    r"msvcr\d*|ucrtbase|libgcc[^.]*|libstdc\+\+)\.dll$"
)
IMPORT_NAME = re.compile(r"^\s*Name:\s*([^\s]+\.dll)\s*$", re.IGNORECASE | re.MULTILINE)


def decoded_views(data: bytes) -> Iterable[str]:
    yield data.decode("latin-1")
    for offset in (0, 1):
        length = len(data) - offset
        if length >= 2:
            yield data[offset : offset + length - length % 2].decode(
                "utf-16-le", errors="ignore"
            )


def path_variants(path: str) -> set[str]:
    normalized = path.rstrip("/\\")
    if len(normalized) < 4:
        return set()
    return {
        normalized,
        normalized.replace("\\", "/"),
        normalized.replace("/", "\\"),
    }


def verify_no_local_paths(executable: Path, local_roots: Sequence[str] = ()) -> None:
    data = executable.read_bytes()
    if len(data) < 2 or data[:2] != b"MZ":
        raise VerificationError("Windows release executable does not have an MZ header")
    views = tuple(decoded_views(data))
    folded_views = tuple(view.casefold() for view in views)
    for label, pattern in SENSITIVE_PATH_PATTERNS:
        if any(pattern.search(view) for view in views):
            raise VerificationError(f"Windows release executable contains a {label}")
    for root in local_roots:
        variants = {variant.casefold() for variant in path_variants(root)}
        if variants and any(
            variant in view for variant in variants for view in folded_views
        ):
            raise VerificationError(
                "Windows release executable contains an explicitly supplied local build root"
            )


def verify_version_metadata(
    executable: Path,
    *,
    product_name: str,
    original_filename: str,
    product_version: str,
) -> None:
    data = executable.read_bytes()
    expected = {
        "product name": ("ProductName", product_name),
        "original filename": ("OriginalFilename", original_filename),
        "product version": ("ProductVersion", product_version),
    }
    for label, (key, value) in expected.items():
        key_bytes = f"{key}\0".encode("utf-16-le")
        value_bytes = f"{value}\0".encode("utf-16-le")
        search_from = 0
        matched = False
        while True:
            key_offset = data.find(key_bytes, search_from)
            if key_offset < 0:
                break
            value_offset = key_offset + len(key_bytes)
            if any(
                data.startswith(b"\0" * padding + value_bytes, value_offset)
                for padding in range(4)
            ):
                matched = True
                break
            search_from = key_offset + 2
        if not matched:
            raise VerificationError(f"Windows release executable omits the {label}")


def verify_static_runtime_imports(executable: Path, llvm_readobj: Path) -> list[str]:
    process = subprocess.run(
        [str(llvm_readobj), "--coff-imports", str(executable)],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if process.returncode != 0:
        detail = process.stderr.strip() or process.stdout.strip() or "llvm-readobj failed"
        raise VerificationError(detail)
    imports = sorted(set(IMPORT_NAME.findall(process.stdout)), key=str.casefold)
    if not imports:
        raise VerificationError("llvm-readobj reported no Windows DLL imports")
    forbidden = [name for name in imports if FORBIDDEN_RUNTIME_IMPORT.fullmatch(name)]
    if forbidden:
        raise VerificationError(
            "Windows release executable dynamically imports a C/C++ runtime"
        )
    return imports


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("--local-root", action="append", default=[])
    parser.add_argument("--llvm-readobj", type=Path)
    parser.add_argument("--product-name")
    parser.add_argument("--original-filename")
    parser.add_argument("--product-version")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        verify_no_local_paths(args.executable, args.local_root)
        print("local_build_paths=none")
        metadata_values = (
            args.product_name,
            args.original_filename,
            args.product_version,
        )
        if any(metadata_values):
            if not all(metadata_values):
                raise VerificationError(
                    "product name, original filename, and product version must be checked together"
                )
            verify_version_metadata(
                args.executable,
                product_name=args.product_name,
                original_filename=args.original_filename,
                product_version=args.product_version,
            )
            print("windows_version_metadata=verified")
        if args.llvm_readobj is not None:
            imports = verify_static_runtime_imports(args.executable, args.llvm_readobj)
            print("external_vc_runtime_imports=none")
            print(f"windows_imported_dlls={len(imports)}")
        else:
            print("static_import_check=not_run")
    except (OSError, VerificationError) as error:
        print(f"Windows release verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
