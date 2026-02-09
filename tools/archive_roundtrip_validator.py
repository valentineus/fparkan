#!/usr/bin/env python3
"""
Roundtrip tools for NRes and RsLi archives.

The script can:
1) scan archives by header signature (ignores file extensions),
2) unpack / pack NRes archives,
3) unpack / pack RsLi archives,
4) validate docs assumptions by full roundtrip and byte-to-byte comparison.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import struct
import tempfile
import zlib
from pathlib import Path
from typing import Any

MAGIC_NRES = b"NRes"
MAGIC_RSLI = b"NL\x00\x01"


class ArchiveFormatError(RuntimeError):
    pass


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def safe_component(value: str, fallback: str = "item", max_len: int = 80) -> str:
    clean = re.sub(r"[^A-Za-z0-9._-]+", "_", value).strip("._-")
    if not clean:
        clean = fallback
    return clean[:max_len]


def first_diff(a: bytes, b: bytes) -> tuple[int | None, str | None]:
    if a == b:
        return None, None
    limit = min(len(a), len(b))
    for idx in range(limit):
        if a[idx] != b[idx]:
            return idx, f"{a[idx]:02x}!={b[idx]:02x}"
    return limit, f"len {len(a)}!={len(b)}"


def load_json(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def dump_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, ensure_ascii=False)
        handle.write("\n")


def xor_stream(data: bytes, key16: int) -> bytes:
    lo = key16 & 0xFF
    hi = (key16 >> 8) & 0xFF
    out = bytearray(len(data))
    for i, value in enumerate(data):
        lo = (hi ^ ((lo << 1) & 0xFF)) & 0xFF
        out[i] = value ^ lo
        hi = (lo ^ ((hi >> 1) & 0xFF)) & 0xFF
    return bytes(out)


def lzss_decompress_simple(data: bytes, expected_size: int) -> bytes:
    ring = bytearray([0x20] * 0x1000)
    ring_pos = 0xFEE
    out = bytearray()
    in_pos = 0
    control = 0
    bits_left = 0

    while len(out) < expected_size and in_pos < len(data):
        if bits_left == 0:
            control = data[in_pos]
            in_pos += 1
            bits_left = 8

        if control & 1:
            if in_pos >= len(data):
                break
            byte = data[in_pos]
            in_pos += 1
            out.append(byte)
            ring[ring_pos] = byte
            ring_pos = (ring_pos + 1) & 0x0FFF
        else:
            if in_pos + 1 >= len(data):
                break
            low = data[in_pos]
            high = data[in_pos + 1]
            in_pos += 2
            # Real files indicate nibble layout opposite to common LZSS variant:
            # high nibble extends offset, low nibble stores (length - 3).
            offset = low | ((high & 0xF0) << 4)
            length = (high & 0x0F) + 3
            for step in range(length):
                byte = ring[(offset + step) & 0x0FFF]
                out.append(byte)
                ring[ring_pos] = byte
                ring_pos = (ring_pos + 1) & 0x0FFF
                if len(out) >= expected_size:
                    break

        control >>= 1
        bits_left -= 1

    if len(out) != expected_size:
        raise ArchiveFormatError(
            f"LZSS size mismatch: expected {expected_size}, got {len(out)}"
        )
    return bytes(out)


def decode_rsli_payload(
    packed: bytes, method: int, sort_to_original: int, unpacked_size: int
) -> bytes:
    key16 = sort_to_original & 0xFFFF

    if method == 0x000:
        out = packed
    elif method == 0x020:
        if len(packed) < unpacked_size:
            raise ArchiveFormatError(
                f"method 0x20 packed too short: {len(packed)} < {unpacked_size}"
            )
        out = xor_stream(packed[:unpacked_size], key16)
    elif method == 0x040:
        out = lzss_decompress_simple(packed, unpacked_size)
    elif method == 0x060:
        out = lzss_decompress_simple(xor_stream(packed, key16), unpacked_size)
    elif method == 0x100:
        try:
            out = zlib.decompress(packed, -15)
        except zlib.error:
            out = zlib.decompress(packed)
    else:
        raise ArchiveFormatError(f"unsupported RsLi method: 0x{method:03X}")

    if len(out) != unpacked_size:
        raise ArchiveFormatError(
            f"unpacked_size mismatch: expected {unpacked_size}, got {len(out)}"
        )
    return out


def detect_archive_type(path: Path) -> str | None:
    try:
        with path.open("rb") as handle:
            magic = handle.read(4)
    except OSError:
        return None

    if magic == MAGIC_NRES:
        return "nres"
    if magic == MAGIC_RSLI:
        return "rsli"
    return None


def scan_archives(root: Path) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        archive_type = detect_archive_type(path)
        if not archive_type:
            continue
        found.append(
            {
                "path": str(path),
                "relative_path": str(path.relative_to(root)),
                "type": archive_type,
                "size": path.stat().st_size,
            }
        )
    return found


def parse_nres(data: bytes, source: str = "<memory>") -> dict[str, Any]:
    if len(data) < 16:
        raise ArchiveFormatError(f"{source}: NRes too short ({len(data)} bytes)")

    magic, version, entry_count, total_size = struct.unpack_from("<4sIII", data, 0)
    if magic != MAGIC_NRES:
        raise ArchiveFormatError(f"{source}: invalid NRes magic")

    issues: list[str] = []
    if total_size != len(data):
        issues.append(
            f"header.total_size={total_size} != actual_size={len(data)} (spec 1.2)"
        )
    if version != 0x100:
        issues.append(f"version=0x{version:08X} != 0x00000100 (spec 1.2)")

    directory_offset = total_size - entry_count * 64
    if directory_offset < 16 or directory_offset > len(data):
        raise ArchiveFormatError(
            f"{source}: invalid directory offset {directory_offset} for entry_count={entry_count}"
        )
    if directory_offset + entry_count * 64 != len(data):
        issues.append(
            "directory_offset + entry_count*64 != file_size (spec 1.3)"
        )

    entries: list[dict[str, Any]] = []
    for index in range(entry_count):
        offset = directory_offset + index * 64
        if offset + 64 > len(data):
            raise ArchiveFormatError(f"{source}: truncated directory entry {index}")

        (
            type_id,
            attr1,
            attr2,
            size,
            attr3,
            name_raw,
            data_offset,
            sort_index,
        ) = struct.unpack_from("<IIIII36sII", data, offset)
        name_bytes = name_raw.split(b"\x00", 1)[0]
        name = name_bytes.decode("latin1", errors="replace")
        entries.append(
            {
                "index": index,
                "type_id": type_id,
                "attr1": attr1,
                "attr2": attr2,
                "size": size,
                "attr3": attr3,
                "name": name,
                "name_bytes_hex": name_bytes.hex(),
                "name_raw_hex": name_raw.hex(),
                "data_offset": data_offset,
                "sort_index": sort_index,
            }
        )

    # Spec checks.
    expected_sort = sorted(
        range(entry_count),
        key=lambda idx: bytes.fromhex(entries[idx]["name_bytes_hex"]).lower(),
    )
    current_sort = [item["sort_index"] for item in entries]
    if current_sort != expected_sort:
        issues.append(
            "sort_index table does not match case-insensitive name order (spec 1.4)"
        )

    data_regions = sorted(
        (
            item["index"],
            item["data_offset"],
            item["size"],
        )
        for item in entries
    )
    for idx, data_offset, size in data_regions:
        if data_offset % 8 != 0:
            issues.append(f"entry {idx}: data_offset={data_offset} not aligned to 8 (spec 1.5)")
        if data_offset < 16 or data_offset + size > directory_offset:
            issues.append(
                f"entry {idx}: data range [{data_offset}, {data_offset + size}) out of data area (spec 1.3)"
            )
    for i in range(len(data_regions) - 1):
        _, start, size = data_regions[i]
        _, next_start, _ = data_regions[i + 1]
        if start + size > next_start:
            issues.append(
                f"entry overlap at data_offset={start}, next={next_start}"
            )
        padding = data[start + size : next_start]
        if any(padding):
            issues.append(
                f"non-zero padding after data block at offset={start + size} (spec 1.5)"
            )

    return {
        "format": "NRes",
        "header": {
            "magic": "NRes",
            "version": version,
            "entry_count": entry_count,
            "total_size": total_size,
            "directory_offset": directory_offset,
        },
        "entries": entries,
        "issues": issues,
    }


def build_nres_name_field(entry: dict[str, Any]) -> bytes:
    if "name_bytes_hex" in entry:
        raw = bytes.fromhex(entry["name_bytes_hex"])
    else:
        raw = entry.get("name", "").encode("latin1", errors="replace")
    raw = raw[:35]
    return raw + b"\x00" * (36 - len(raw))


def unpack_nres_file(archive_path: Path, out_dir: Path, source_root: Path | None = None) -> dict[str, Any]:
    data = archive_path.read_bytes()
    parsed = parse_nres(data, source=str(archive_path))

    out_dir.mkdir(parents=True, exist_ok=True)
    entries_dir = out_dir / "entries"
    entries_dir.mkdir(parents=True, exist_ok=True)

    manifest: dict[str, Any] = {
        "format": "NRes",
        "source_path": str(archive_path),
        "source_relative_path": str(archive_path.relative_to(source_root)) if source_root else str(archive_path),
        "header": parsed["header"],
        "entries": [],
        "issues": parsed["issues"],
        "source_sha256": sha256_hex(data),
    }

    for entry in parsed["entries"]:
        begin = entry["data_offset"]
        end = begin + entry["size"]
        if begin < 0 or end > len(data):
            raise ArchiveFormatError(
                f"{archive_path}: entry {entry['index']} data range outside file"
            )
        payload = data[begin:end]
        base = safe_component(entry["name"], fallback=f"entry_{entry['index']:05d}")
        file_name = (
            f"{entry['index']:05d}__{base}"
            f"__t{entry['type_id']:08X}_a1{entry['attr1']:08X}_a2{entry['attr2']:08X}.bin"
        )
        (entries_dir / file_name).write_bytes(payload)

        manifest_entry = dict(entry)
        manifest_entry["data_file"] = f"entries/{file_name}"
        manifest_entry["sha256"] = sha256_hex(payload)
        manifest["entries"].append(manifest_entry)

    dump_json(out_dir / "manifest.json", manifest)
    return manifest


def pack_nres_manifest(manifest_path: Path, out_file: Path) -> bytes:
    manifest = load_json(manifest_path)
    if manifest.get("format") != "NRes":
        raise ArchiveFormatError(f"{manifest_path}: not an NRes manifest")

    entries = manifest["entries"]
    count = len(entries)
    version = int(manifest.get("header", {}).get("version", 0x100))

    out = bytearray(b"\x00" * 16)
    data_offsets: list[int] = []
    data_sizes: list[int] = []

    for entry in entries:
        payload_path = manifest_path.parent / entry["data_file"]
        payload = payload_path.read_bytes()
        offset = len(out)
        out.extend(payload)
        padding = (-len(out)) % 8
        if padding:
            out.extend(b"\x00" * padding)
        data_offsets.append(offset)
        data_sizes.append(len(payload))

    directory_offset = len(out)
    expected_sort = sorted(
        range(count),
        key=lambda idx: bytes.fromhex(entries[idx].get("name_bytes_hex", "")).lower(),
    )

    for index, entry in enumerate(entries):
        name_field = build_nres_name_field(entry)
        out.extend(
            struct.pack(
                "<IIIII36sII",
                int(entry["type_id"]),
                int(entry["attr1"]),
                int(entry["attr2"]),
                data_sizes[index],
                int(entry["attr3"]),
                name_field,
                data_offsets[index],
                expected_sort[index],
            )
        )

    total_size = len(out)
    struct.pack_into("<4sIII", out, 0, MAGIC_NRES, version, count, total_size)

    out_file.parent.mkdir(parents=True, exist_ok=True)
    out_file.write_bytes(out)
    return bytes(out)


def parse_rsli(data: bytes, source: str = "<memory>") -> dict[str, Any]:
    if len(data) < 32:
        raise ArchiveFormatError(f"{source}: RsLi too short ({len(data)} bytes)")
    if data[:4] != MAGIC_RSLI:
        raise ArchiveFormatError(f"{source}: invalid RsLi magic")

    issues: list[str] = []
    reserved_zero = data[2]
    version = data[3]
    entry_count = struct.unpack_from("<h", data, 4)[0]
    presorted_flag = struct.unpack_from("<H", data, 14)[0]
    seed = struct.unpack_from("<I", data, 20)[0]

    if reserved_zero != 0:
        issues.append(f"header[2]={reserved_zero} != 0 (spec 2.2)")
    if version != 1:
        issues.append(f"version={version} != 1 (spec 2.2)")
    if entry_count < 0:
        raise ArchiveFormatError(f"{source}: negative entry_count={entry_count}")

    table_offset = 32
    table_size = entry_count * 32
    if table_offset + table_size > len(data):
        raise ArchiveFormatError(
            f"{source}: encrypted table out of file bounds ({table_offset}+{table_size}>{len(data)})"
        )

    table_encrypted = data[table_offset : table_offset + table_size]
    table_plain = xor_stream(table_encrypted, seed & 0xFFFF)

    trailer: dict[str, Any] = {"present": False}
    overlay_offset = 0
    if len(data) >= 6 and data[-6:-4] == b"AO":
        overlay_offset = struct.unpack_from("<I", data, len(data) - 4)[0]
        trailer = {
            "present": True,
            "signature": "AO",
            "overlay_offset": overlay_offset,
            "raw_hex": data[-6:].hex(),
        }

    entries: list[dict[str, Any]] = []
    sort_values: list[int] = []
    for index in range(entry_count):
        row = table_plain[index * 32 : (index + 1) * 32]
        name_raw = row[0:12]
        reserved4 = row[12:16]
        flags_signed, sort_to_original = struct.unpack_from("<hh", row, 16)
        unpacked_size, data_offset, packed_size = struct.unpack_from("<III", row, 20)
        method = flags_signed & 0x1E0
        name = name_raw.split(b"\x00", 1)[0].decode("latin1", errors="replace")
        effective_offset = data_offset + overlay_offset
        entries.append(
            {
                "index": index,
                "name": name,
                "name_raw_hex": name_raw.hex(),
                "reserved_raw_hex": reserved4.hex(),
                "flags_signed": flags_signed,
                "flags_u16": flags_signed & 0xFFFF,
                "method": method,
                "sort_to_original": sort_to_original,
                "unpacked_size": unpacked_size,
                "data_offset": data_offset,
                "effective_data_offset": effective_offset,
                "packed_size": packed_size,
            }
        )
        sort_values.append(sort_to_original)

        if effective_offset < 0:
            issues.append(f"entry {index}: negative effective_data_offset={effective_offset}")
        elif effective_offset + packed_size > len(data):
            end = effective_offset + packed_size
            if method == 0x100 and end == len(data) + 1:
                issues.append(
                    f"entry {index}: deflate packed_size reaches EOF+1 ({end}); "
                    "observed in game data, likely decoder lookahead byte"
                )
            else:
                issues.append(
                    f"entry {index}: packed range [{effective_offset}, {end}) out of file"
                )

    if presorted_flag == 0xABBA:
        if sorted(sort_values) != list(range(entry_count)):
            issues.append(
                "presorted flag is 0xABBA but sort_to_original is not a permutation [0..N-1] (spec 2.2/2.4)"
            )

    return {
        "format": "RsLi",
        "header_raw_hex": data[:32].hex(),
        "header": {
            "magic": "NL\\x00\\x01",
            "entry_count": entry_count,
            "seed": seed,
            "presorted_flag": presorted_flag,
        },
        "entries": entries,
        "issues": issues,
        "trailer": trailer,
    }


def unpack_rsli_file(archive_path: Path, out_dir: Path, source_root: Path | None = None) -> dict[str, Any]:
    data = archive_path.read_bytes()
    parsed = parse_rsli(data, source=str(archive_path))

    out_dir.mkdir(parents=True, exist_ok=True)
    entries_dir = out_dir / "entries"
    entries_dir.mkdir(parents=True, exist_ok=True)

    manifest: dict[str, Any] = {
        "format": "RsLi",
        "source_path": str(archive_path),
        "source_relative_path": str(archive_path.relative_to(source_root)) if source_root else str(archive_path),
        "source_size": len(data),
        "header_raw_hex": parsed["header_raw_hex"],
        "header": parsed["header"],
        "entries": [],
        "issues": list(parsed["issues"]),
        "trailer": parsed["trailer"],
        "source_sha256": sha256_hex(data),
    }

    for entry in parsed["entries"]:
        begin = int(entry["effective_data_offset"])
        end = begin + int(entry["packed_size"])
        packed = data[begin:end]
        base = safe_component(entry["name"], fallback=f"entry_{entry['index']:05d}")
        packed_name = f"{entry['index']:05d}__{base}__packed.bin"
        (entries_dir / packed_name).write_bytes(packed)

        manifest_entry = dict(entry)
        manifest_entry["packed_file"] = f"entries/{packed_name}"
        manifest_entry["packed_file_size"] = len(packed)
        manifest_entry["packed_sha256"] = sha256_hex(packed)

        try:
            unpacked = decode_rsli_payload(
                packed=packed,
                method=int(entry["method"]),
                sort_to_original=int(entry["sort_to_original"]),
                unpacked_size=int(entry["unpacked_size"]),
            )
            unpacked_name = f"{entry['index']:05d}__{base}__unpacked.bin"
            (entries_dir / unpacked_name).write_bytes(unpacked)
            manifest_entry["unpacked_file"] = f"entries/{unpacked_name}"
            manifest_entry["unpacked_sha256"] = sha256_hex(unpacked)
        except ArchiveFormatError as exc:
            manifest_entry["unpack_error"] = str(exc)
            manifest["issues"].append(
                f"entry {entry['index']}: cannot decode method 0x{entry['method']:03X}: {exc}"
            )

        manifest["entries"].append(manifest_entry)

    dump_json(out_dir / "manifest.json", manifest)
    return manifest


def _pack_i16(value: int) -> int:
    if not (-32768 <= int(value) <= 32767):
        raise ArchiveFormatError(f"int16 overflow: {value}")
    return int(value)


def pack_rsli_manifest(manifest_path: Path, out_file: Path) -> bytes:
    manifest = load_json(manifest_path)
    if manifest.get("format") != "RsLi":
        raise ArchiveFormatError(f"{manifest_path}: not an RsLi manifest")

    entries = manifest["entries"]
    count = len(entries)

    header_raw = bytes.fromhex(manifest["header_raw_hex"])
    if len(header_raw) != 32:
        raise ArchiveFormatError(f"{manifest_path}: header_raw_hex must be 32 bytes")
    header = bytearray(header_raw)
    header[:4] = MAGIC_RSLI
    struct.pack_into("<h", header, 4, count)
    seed = int(manifest["header"]["seed"])
    struct.pack_into("<I", header, 20, seed)

    rows = bytearray()
    packed_chunks: list[tuple[dict[str, Any], bytes]] = []

    for entry in entries:
        packed_path = manifest_path.parent / entry["packed_file"]
        packed = packed_path.read_bytes()
        declared_size = int(entry["packed_size"])
        if len(packed) > declared_size:
            raise ArchiveFormatError(
                f"{packed_path}: packed size {len(packed)} > manifest packed_size {declared_size}"
            )

        data_offset = int(entry["data_offset"])
        packed_chunks.append((entry, packed))

        row = bytearray(32)
        name_raw = bytes.fromhex(entry["name_raw_hex"])
        reserved_raw = bytes.fromhex(entry["reserved_raw_hex"])
        if len(name_raw) != 12 or len(reserved_raw) != 4:
            raise ArchiveFormatError(
                f"entry {entry['index']}: invalid name/reserved raw length"
            )
        row[0:12] = name_raw
        row[12:16] = reserved_raw
        struct.pack_into(
            "<hhIII",
            row,
            16,
            _pack_i16(int(entry["flags_signed"])),
            _pack_i16(int(entry["sort_to_original"])),
            int(entry["unpacked_size"]),
            data_offset,
            declared_size,
        )
        rows.extend(row)

    encrypted_table = xor_stream(bytes(rows), seed & 0xFFFF)
    trailer = manifest.get("trailer", {})
    trailer_raw = b""
    if trailer.get("present"):
        raw_hex = trailer.get("raw_hex", "")
        trailer_raw = bytes.fromhex(raw_hex)
        if len(trailer_raw) != 6:
            raise ArchiveFormatError("trailer raw length must be 6 bytes")

    source_size = manifest.get("source_size")
    table_end = 32 + count * 32
    if source_size is not None:
        pre_trailer_size = int(source_size) - len(trailer_raw)
        if pre_trailer_size < table_end:
            raise ArchiveFormatError(
                f"invalid source_size={source_size}: smaller than header+table"
            )
    else:
        pre_trailer_size = table_end
        for entry, packed in packed_chunks:
            pre_trailer_size = max(
                pre_trailer_size, int(entry["data_offset"]) + len(packed)
            )

    out = bytearray(pre_trailer_size)
    out[0:32] = header
    out[32:table_end] = encrypted_table
    occupied = bytearray(pre_trailer_size)
    occupied[0:table_end] = b"\x01" * table_end

    for entry, packed in packed_chunks:
        base_offset = int(entry["data_offset"])
        for index, byte in enumerate(packed):
            pos = base_offset + index
            if pos >= pre_trailer_size:
                raise ArchiveFormatError(
                    f"entry {entry['index']}: data write at {pos} beyond output size {pre_trailer_size}"
                )
            if occupied[pos] and out[pos] != byte:
                raise ArchiveFormatError(
                    f"entry {entry['index']}: overlapping packed data conflict at offset {pos}"
                )
            out[pos] = byte
            occupied[pos] = 1

    out.extend(trailer_raw)
    if source_size is not None and len(out) != int(source_size):
        raise ArchiveFormatError(
            f"packed size {len(out)} != source_size {source_size} from manifest"
        )

    out_file.parent.mkdir(parents=True, exist_ok=True)
    out_file.write_bytes(out)
    return bytes(out)


def cmd_scan(args: argparse.Namespace) -> int:
    root = Path(args.input).resolve()
    archives = scan_archives(root)
    if args.json:
        print(json.dumps(archives, ensure_ascii=False, indent=2))
    else:
        print(f"Found {len(archives)} archive(s) in {root}")
        for item in archives:
            print(f"{item['type']:4}  {item['size']:10d}  {item['relative_path']}")
    return 0


def cmd_nres_unpack(args: argparse.Namespace) -> int:
    archive_path = Path(args.archive).resolve()
    out_dir = Path(args.output).resolve()
    manifest = unpack_nres_file(archive_path, out_dir)
    print(f"NRes unpacked: {archive_path}")
    print(f"Manifest: {out_dir / 'manifest.json'}")
    print(f"Entries : {len(manifest['entries'])}")
    if manifest["issues"]:
        print("Issues:")
        for issue in manifest["issues"]:
            print(f"- {issue}")
    return 0


def cmd_nres_pack(args: argparse.Namespace) -> int:
    manifest_path = Path(args.manifest).resolve()
    out_file = Path(args.output).resolve()
    packed = pack_nres_manifest(manifest_path, out_file)
    print(f"NRes packed: {out_file} ({len(packed)} bytes, sha256={sha256_hex(packed)})")
    return 0


def cmd_rsli_unpack(args: argparse.Namespace) -> int:
    archive_path = Path(args.archive).resolve()
    out_dir = Path(args.output).resolve()
    manifest = unpack_rsli_file(archive_path, out_dir)
    print(f"RsLi unpacked: {archive_path}")
    print(f"Manifest: {out_dir / 'manifest.json'}")
    print(f"Entries : {len(manifest['entries'])}")
    if manifest["issues"]:
        print("Issues:")
        for issue in manifest["issues"]:
            print(f"- {issue}")
    return 0


def cmd_rsli_pack(args: argparse.Namespace) -> int:
    manifest_path = Path(args.manifest).resolve()
    out_file = Path(args.output).resolve()
    packed = pack_rsli_manifest(manifest_path, out_file)
    print(f"RsLi packed: {out_file} ({len(packed)} bytes, sha256={sha256_hex(packed)})")
    return 0


def cmd_validate(args: argparse.Namespace) -> int:
    input_root = Path(args.input).resolve()
    archives = scan_archives(input_root)

    temp_created = False
    if args.workdir:
        workdir = Path(args.workdir).resolve()
        workdir.mkdir(parents=True, exist_ok=True)
    else:
        workdir = Path(tempfile.mkdtemp(prefix="nres-rsli-validate-"))
        temp_created = True

    report: dict[str, Any] = {
        "input_root": str(input_root),
        "workdir": str(workdir),
        "archives_total": len(archives),
        "results": [],
        "summary": {},
    }

    failures = 0
    try:
        for idx, item in enumerate(archives):
            rel = item["relative_path"]
            archive_path = input_root / rel
            marker = f"{idx:04d}_{safe_component(rel, fallback='archive')}"
            unpack_dir = workdir / "unpacked" / marker
            repacked_file = workdir / "repacked" / f"{marker}.bin"
            try:
                if item["type"] == "nres":
                    manifest = unpack_nres_file(archive_path, unpack_dir, source_root=input_root)
                    repacked = pack_nres_manifest(unpack_dir / "manifest.json", repacked_file)
                elif item["type"] == "rsli":
                    manifest = unpack_rsli_file(archive_path, unpack_dir, source_root=input_root)
                    repacked = pack_rsli_manifest(unpack_dir / "manifest.json", repacked_file)
                else:
                    continue

                original = archive_path.read_bytes()
                match = original == repacked
                diff_offset, diff_desc = first_diff(original, repacked)
                issues = list(manifest.get("issues", []))
                result = {
                    "relative_path": rel,
                    "type": item["type"],
                    "size_original": len(original),
                    "size_repacked": len(repacked),
                    "sha256_original": sha256_hex(original),
                    "sha256_repacked": sha256_hex(repacked),
                    "match": match,
                    "first_diff_offset": diff_offset,
                    "first_diff": diff_desc,
                    "issues": issues,
                    "entries": len(manifest.get("entries", [])),
                    "error": None,
                }
            except Exception as exc:  # pylint: disable=broad-except
                result = {
                    "relative_path": rel,
                    "type": item["type"],
                    "size_original": item["size"],
                    "size_repacked": None,
                    "sha256_original": None,
                    "sha256_repacked": None,
                    "match": False,
                    "first_diff_offset": None,
                    "first_diff": None,
                    "issues": [f"processing error: {exc}"],
                    "entries": None,
                    "error": str(exc),
                }

            report["results"].append(result)

            if not result["match"]:
                failures += 1
            if result["issues"] and args.fail_on_issues:
                failures += 1

        matches = sum(1 for row in report["results"] if row["match"])
        mismatches = len(report["results"]) - matches
        nres_count = sum(1 for row in report["results"] if row["type"] == "nres")
        rsli_count = sum(1 for row in report["results"] if row["type"] == "rsli")
        issues_total = sum(len(row["issues"]) for row in report["results"])
        report["summary"] = {
            "nres_count": nres_count,
            "rsli_count": rsli_count,
            "matches": matches,
            "mismatches": mismatches,
            "issues_total": issues_total,
        }

        if args.report:
            dump_json(Path(args.report).resolve(), report)

        print(f"Input root     : {input_root}")
        print(f"Work dir       : {workdir}")
        print(f"NRes archives  : {nres_count}")
        print(f"RsLi archives  : {rsli_count}")
        print(f"Roundtrip match: {matches}/{len(report['results'])}")
        print(f"Doc issues     : {issues_total}")

        if mismatches:
            print("\nMismatches:")
            for row in report["results"]:
                if row["match"]:
                    continue
                print(
                    f"- {row['relative_path']} [{row['type']}] "
                    f"diff@{row['first_diff_offset']}: {row['first_diff']}"
                )

        if issues_total:
            print("\nIssues:")
            for row in report["results"]:
                if not row["issues"]:
                    continue
                print(f"- {row['relative_path']} [{row['type']}]")
                for issue in row["issues"]:
                    print(f"  * {issue}")

    finally:
        if temp_created or args.cleanup:
            shutil.rmtree(workdir, ignore_errors=True)

    if failures > 0:
        return 1
    if report["summary"].get("mismatches", 0) > 0 and args.fail_on_diff:
        return 1
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="NRes/RsLi tools: scan, unpack, repack, and roundtrip validation."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    scan = sub.add_parser("scan", help="Scan files by header signatures.")
    scan.add_argument("--input", required=True, help="Root directory to scan.")
    scan.add_argument("--json", action="store_true", help="Print JSON output.")
    scan.set_defaults(func=cmd_scan)

    nres_unpack = sub.add_parser("nres-unpack", help="Unpack a single NRes archive.")
    nres_unpack.add_argument("--archive", required=True, help="Path to NRes file.")
    nres_unpack.add_argument("--output", required=True, help="Output directory.")
    nres_unpack.set_defaults(func=cmd_nres_unpack)

    nres_pack = sub.add_parser("nres-pack", help="Pack NRes archive from manifest.")
    nres_pack.add_argument("--manifest", required=True, help="Path to manifest.json.")
    nres_pack.add_argument("--output", required=True, help="Output file path.")
    nres_pack.set_defaults(func=cmd_nres_pack)

    rsli_unpack = sub.add_parser("rsli-unpack", help="Unpack a single RsLi archive.")
    rsli_unpack.add_argument("--archive", required=True, help="Path to RsLi file.")
    rsli_unpack.add_argument("--output", required=True, help="Output directory.")
    rsli_unpack.set_defaults(func=cmd_rsli_unpack)

    rsli_pack = sub.add_parser("rsli-pack", help="Pack RsLi archive from manifest.")
    rsli_pack.add_argument("--manifest", required=True, help="Path to manifest.json.")
    rsli_pack.add_argument("--output", required=True, help="Output file path.")
    rsli_pack.set_defaults(func=cmd_rsli_pack)

    validate = sub.add_parser(
        "validate",
        help="Scan all archives and run unpack->repack->byte-compare validation.",
    )
    validate.add_argument("--input", required=True, help="Root with game data files.")
    validate.add_argument(
        "--workdir",
        help="Working directory for temporary unpack/repack files. "
        "If omitted, a temporary directory is used and removed automatically.",
    )
    validate.add_argument("--report", help="Optional JSON report output path.")
    validate.add_argument(
        "--fail-on-diff",
        action="store_true",
        help="Return non-zero exit code if any byte mismatch exists.",
    )
    validate.add_argument(
        "--fail-on-issues",
        action="store_true",
        help="Return non-zero exit code if any spec issue was detected.",
    )
    validate.add_argument(
        "--cleanup",
        action="store_true",
        help="Remove --workdir after completion.",
    )
    validate.set_defaults(func=cmd_validate)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
