#!/usr/bin/env python3
"""Create a deterministic ZIP from a staged Windows v4 release directory."""

from __future__ import annotations

import argparse
import datetime as dt
import os
import stat
import tempfile
import unicodedata
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence


ZIP_MIN_EPOCH = 315_532_800  # 1980-01-01T00:00:00Z
ZIP_MAX_EPOCH_EXCLUSIVE = 4_354_819_200  # 2108-01-01T00:00:00Z
FILE_MODE = stat.S_IFREG | 0o644
DIRECTORY_MODE = stat.S_IFDIR | 0o755
WINDOWS_FORBIDDEN_CHARACTERS = frozenset('<>:"/\\|?*')
WINDOWS_RESERVED_NAMES = frozenset(
    {"CON", "PRN", "AUX", "NUL"}
    | {f"COM{index}" for index in range(1, 10)}
    | {f"LPT{index}" for index in range(1, 10)}
)


class PackagingError(ValueError):
    """Raised when the staging tree cannot be packaged safely."""


@dataclass(frozen=True)
class Entry:
    source: Path
    archive_name: str
    is_directory: bool


def _validate_component(component: str) -> None:
    if component in {"", ".", ".."}:
        raise PackagingError(f"unsafe path component: {component!r}")
    if component.endswith((" ", ".")):
        raise PackagingError(f"Windows-unsafe path component: {component!r}")
    if any(character in WINDOWS_FORBIDDEN_CHARACTERS for character in component):
        raise PackagingError(f"Windows-unsafe path component: {component!r}")
    if any(ord(character) < 32 for character in component):
        raise PackagingError(f"control character in path component: {component!r}")
    stem = component.split(".", 1)[0].upper()
    if stem in WINDOWS_RESERVED_NAMES:
        raise PackagingError(f"reserved Windows path component: {component!r}")


def _collision_key(archive_name: str) -> str:
    return unicodedata.normalize("NFC", archive_name.rstrip("/")).casefold()


def _collect_entries(root: Path) -> list[Entry]:
    entries: list[Entry] = []
    collision_keys: dict[str, str] = {}

    def visit(directory: Path, relative_parts: tuple[str, ...]) -> None:
        try:
            with os.scandir(directory) as iterator:
                children = sorted(iterator, key=lambda item: item.name)
        except OSError as error:
            raise PackagingError(f"cannot read staging directory {directory}: {error}") from error

        for child in children:
            _validate_component(child.name)
            parts = (*relative_parts, child.name)
            archive_path = "/".join(parts)
            try:
                if child.is_symlink():
                    raise PackagingError(f"symbolic links are not allowed: {archive_path}")
                if child.is_dir(follow_symlinks=False):
                    archive_name = f"{archive_path}/"
                    entry = Entry(Path(child.path), archive_name, True)
                elif child.is_file(follow_symlinks=False):
                    archive_name = archive_path
                    entry = Entry(Path(child.path), archive_name, False)
                else:
                    raise PackagingError(f"special files are not allowed: {archive_path}")
            except OSError as error:
                raise PackagingError(f"cannot inspect staging entry {archive_path}: {error}") from error

            collision_key = _collision_key(archive_name)
            previous = collision_keys.get(collision_key)
            if previous is not None:
                raise PackagingError(
                    f"entries collide on Windows: {previous!r} and {archive_name!r}"
                )
            collision_keys[collision_key] = archive_name
            entries.append(entry)
            if entry.is_directory:
                visit(entry.source, parts)

    visit(root, ())
    entries.sort(key=lambda entry: entry.archive_name)
    if not any(not entry.is_directory for entry in entries):
        raise PackagingError("staging directory contains no release files")
    return entries


def _resolve_epoch(explicit_epoch: int | None, environment: Mapping[str, str]) -> int:
    if explicit_epoch is not None:
        return explicit_epoch
    value = environment.get("SOURCE_DATE_EPOCH")
    if value is None:
        raise PackagingError("SOURCE_DATE_EPOCH or --source-date-epoch is required")
    try:
        return int(value, 10)
    except ValueError as error:
        raise PackagingError("SOURCE_DATE_EPOCH must be an integer") from error


def _zip_timestamp(epoch: int) -> tuple[int, int, int, int, int, int]:
    normalized = max(epoch, ZIP_MIN_EPOCH)
    if normalized >= ZIP_MAX_EPOCH_EXCLUSIVE:
        raise PackagingError("release timestamp exceeds the ZIP date range")
    timestamp = dt.datetime.fromtimestamp(normalized, tz=dt.timezone.utc)
    second = timestamp.second - timestamp.second % 2
    return (
        timestamp.year,
        timestamp.month,
        timestamp.day,
        timestamp.hour,
        timestamp.minute,
        second,
    )


def _zip_info(entry: Entry, timestamp: tuple[int, int, int, int, int, int]) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(entry.archive_name, timestamp)
    info.create_system = 3
    info.create_version = 20
    info.extract_version = 20
    info.flag_bits = 0
    info.internal_attr = 0
    info.external_attr = (
        ((DIRECTORY_MODE if entry.is_directory else FILE_MODE) << 16)
        | (0x10 if entry.is_directory else 0)
    )
    info.compress_type = zipfile.ZIP_STORED if entry.is_directory else zipfile.ZIP_DEFLATED
    info.extra = b""
    info.comment = b""
    return info


def package(
    input_directory: Path,
    output_zip: Path,
    *,
    source_date_epoch: int | None = None,
    environment: Mapping[str, str] = os.environ,
) -> None:
    if input_directory.is_symlink():
        raise PackagingError("staging directory must not be a symbolic link")
    try:
        root = input_directory.resolve(strict=True)
    except OSError as error:
        raise PackagingError(f"staging directory is unavailable: {input_directory}") from error
    if not root.is_dir():
        raise PackagingError(f"staging path is not a directory: {input_directory}")

    if output_zip.is_symlink():
        raise PackagingError("output ZIP must not be a symbolic link")
    output = output_zip.resolve(strict=False)
    if output == root or root in output.parents:
        raise PackagingError("output ZIP must be outside the staging directory")
    if output.exists() and not output.is_file():
        raise PackagingError(f"output path is not a file: {output_zip}")

    timestamp = _zip_timestamp(_resolve_epoch(source_date_epoch, environment))
    entries = _collect_entries(root)
    output.parent.mkdir(parents=True, exist_ok=True)

    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w+b", prefix=f".{output.name}.", suffix=".tmp", dir=output.parent, delete=False
        ) as temporary:
            temporary_path = Path(temporary.name)
        with zipfile.ZipFile(
            temporary_path,
            mode="w",
            compression=zipfile.ZIP_DEFLATED,
            compresslevel=9,
            strict_timestamps=True,
        ) as archive:
            archive.comment = b""
            for entry in entries:
                content = b"" if entry.is_directory else entry.source.read_bytes()
                archive.writestr(_zip_info(entry, timestamp), content, compresslevel=9)
        os.replace(temporary_path, output)
        temporary_path = None
    except (OSError, zipfile.BadZipFile) as error:
        raise PackagingError(f"could not create release ZIP: {error}") from error
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input_directory", type=Path)
    parser.add_argument("output_zip", type=Path)
    parser.add_argument("--source-date-epoch", type=int)
    args = parser.parse_args(argv)
    try:
        package(
            args.input_directory,
            args.output_zip,
            source_date_epoch=args.source_date_epoch,
        )
    except PackagingError as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
