#!/usr/bin/env python3
"""Normalize non-functional PE debug metadata for reproducible releases."""

from __future__ import annotations

import argparse
import struct
from pathlib import Path


def read_u16(data: bytearray, offset: int) -> int:
    return struct.unpack_from("<H", data, offset)[0]


def read_u32(data: bytearray, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def require_range(data: bytearray, offset: int, length: int, label: str) -> None:
    if offset < 0 or length < 0 or offset + length > len(data):
        raise ValueError(f"{label} extends beyond the PE file")


def rva_to_file_offset(
    data: bytearray,
    section_table: int,
    section_count: int,
    rva: int,
) -> int:
    for index in range(section_count):
        section = section_table + index * 40
        require_range(data, section, 40, "section header")
        virtual_size = read_u32(data, section + 8)
        virtual_address = read_u32(data, section + 12)
        raw_size = read_u32(data, section + 16)
        raw_offset = read_u32(data, section + 20)
        extent = max(virtual_size, raw_size)
        if virtual_address <= rva < virtual_address + extent:
            offset = raw_offset + (rva - virtual_address)
            require_range(data, offset, 1, "RVA mapping")
            return offset
    raise ValueError(f"RVA 0x{rva:x} is not mapped by a PE section")


def normalize(path: Path) -> tuple[int, int]:
    data = bytearray(path.read_bytes())
    require_range(data, 0, 64, "DOS header")
    if data[:2] != b"MZ":
        raise ValueError("file does not have an MZ header")
    pe = read_u32(data, 0x3C)
    require_range(data, pe, 24, "PE header")
    if data[pe : pe + 4] != b"PE\0\0":
        raise ValueError("file does not have a PE signature")

    section_count = read_u16(data, pe + 6)
    optional_size = read_u16(data, pe + 20)
    optional = pe + 24
    require_range(data, optional, optional_size, "optional header")
    magic = read_u16(data, optional)
    if magic == 0x20B:
        directory_count_offset = optional + 108
        directories = optional + 112
    elif magic == 0x10B:
        directory_count_offset = optional + 92
        directories = optional + 96
    else:
        raise ValueError(f"unsupported PE optional-header magic 0x{magic:x}")

    data[pe + 8 : pe + 12] = b"\0" * 4
    directory_count = read_u32(data, directory_count_offset)
    if directory_count <= 6:
        path.write_bytes(data)
        return (0, 0)

    debug_directory = directories + 6 * 8
    require_range(data, debug_directory, 8, "debug data directory")
    debug_rva = read_u32(data, debug_directory)
    debug_size = read_u32(data, debug_directory + 4)
    if debug_rva == 0 or debug_size == 0:
        path.write_bytes(data)
        return (0, 0)
    if debug_size % 28 != 0:
        raise ValueError("PE debug directory size is not entry-aligned")

    section_table = optional + optional_size
    debug_offset = rva_to_file_offset(data, section_table, section_count, debug_rva)
    require_range(data, debug_offset, debug_size, "debug directory")
    entries = debug_size // 28
    codeview = 0
    for index in range(entries):
        entry = debug_offset + index * 28
        data[entry + 4 : entry + 8] = b"\0" * 4
        if read_u32(data, entry + 12) != 2:
            continue
        payload_size = read_u32(data, entry + 16)
        payload_offset = read_u32(data, entry + 24)
        require_range(data, payload_offset, payload_size, "CodeView record")
        if payload_size >= 24 and data[payload_offset : payload_offset + 4] == b"RSDS":
            data[payload_offset + 4 : payload_offset + 20] = b"\0" * 16
            codeview += 1

    path.write_bytes(data)
    return (entries, codeview)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("executable", type=Path)
    args = parser.parse_args()
    entries, codeview = normalize(args.executable)
    print(f"normalized_debug_entries={entries}")
    print(f"normalized_codeview_records={codeview}")


if __name__ == "__main__":
    main()
