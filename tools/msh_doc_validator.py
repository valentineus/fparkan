#!/usr/bin/env python3
"""
Validate assumptions from docs/specs/msh.md on real game archives.

The tool checks three groups:
1) MSH model payloads (nested NRes in *.msh entries),
2) Texm texture payloads,
3) FXID effect payloads.
"""

from __future__ import annotations

import argparse
import json
import math
import struct
from collections import Counter
from pathlib import Path
from typing import Any

import archive_roundtrip_validator as arv

MAGIC_NRES = b"NRes"
MAGIC_PAGE = b"Page"

TYPE_FXID = 0x44495846
TYPE_TEXM = 0x6D786554

FX_CMD_SIZE = {1: 224, 2: 148, 3: 200, 4: 204, 5: 112, 6: 4, 7: 208, 8: 248, 9: 208, 10: 208}
TEXM_KNOWN_FORMATS = {0, 565, 556, 4444, 888, 8888}


def _add_issue(
    issues: list[dict[str, Any]],
    severity: str,
    category: str,
    archive: Path,
    entry_name: str | None,
    message: str,
) -> None:
    issues.append(
        {
            "severity": severity,
            "category": category,
            "archive": str(archive),
            "entry": entry_name,
            "message": message,
        }
    )


def _entry_payload(blob: bytes, entry: dict[str, Any]) -> bytes:
    start = int(entry["data_offset"])
    end = start + int(entry["size"])
    return blob[start:end]


def _entry_by_type(entries: list[dict[str, Any]]) -> dict[int, list[dict[str, Any]]]:
    by_type: dict[int, list[dict[str, Any]]] = {}
    for item in entries:
        by_type.setdefault(int(item["type_id"]), []).append(item)
    return by_type


def _expect_single_resource(
    by_type: dict[int, list[dict[str, Any]]],
    type_id: int,
    label: str,
    issues: list[dict[str, Any]],
    archive: Path,
    model_name: str,
    required: bool,
) -> dict[str, Any] | None:
    rows = by_type.get(type_id, [])
    if not rows:
        if required:
            _add_issue(
                issues,
                "error",
                "model-resource",
                archive,
                model_name,
                f"missing required resource type={type_id} ({label})",
            )
        return None
    if len(rows) > 1:
        _add_issue(
            issues,
            "warning",
            "model-resource",
            archive,
            model_name,
            f"multiple resources type={type_id} ({label}); using first entry",
        )
    return rows[0]


def _check_fixed_stride(
    *,
    entry: dict[str, Any],
    stride: int,
    label: str,
    issues: list[dict[str, Any]],
    archive: Path,
    model_name: str,
    enforce_attr3: bool = True,
    enforce_attr2_zero: bool = True,
) -> int:
    size = int(entry["size"])
    attr1 = int(entry["attr1"])
    attr2 = int(entry["attr2"])
    attr3 = int(entry["attr3"])

    count = -1
    if size % stride != 0:
        _add_issue(
            issues,
            "error",
            "model-stride",
            archive,
            model_name,
            f"{label}: size={size} is not divisible by stride={stride}",
        )
    else:
        count = size // stride
        if attr1 != count:
            _add_issue(
                issues,
                "error",
                "model-attr",
                archive,
                model_name,
                f"{label}: attr1={attr1} != size/stride={count}",
            )
    if enforce_attr3 and attr3 != stride:
        _add_issue(
            issues,
            "error",
            "model-attr",
            archive,
            model_name,
            f"{label}: attr3={attr3} != {stride}",
        )
    if enforce_attr2_zero and attr2 != 0:
        _add_issue(
            issues,
            "warning",
            "model-attr",
            archive,
            model_name,
            f"{label}: attr2={attr2} (expected 0 in known assets)",
        )
    return count


def _validate_res10(
    data: bytes,
    node_count: int,
    issues: list[dict[str, Any]],
    archive: Path,
    model_name: str,
) -> None:
    off = 0
    for idx in range(node_count):
        if off + 4 > len(data):
            _add_issue(
                issues,
                "error",
                "res10",
                archive,
                model_name,
                f"record {idx}: missing u32 length (offset={off}, size={len(data)})",
            )
            return
        ln = struct.unpack_from("<I", data, off)[0]
        off += 4
        need = ln + 1 if ln else 0
        if off + need > len(data):
            _add_issue(
                issues,
                "error",
                "res10",
                archive,
                model_name,
                f"record {idx}: out of bounds (len={ln}, need={need}, offset={off}, size={len(data)})",
            )
            return
        if ln and data[off + ln] != 0:
            _add_issue(
                issues,
                "warning",
                "res10",
                archive,
                model_name,
                f"record {idx}: missing trailing NUL at payload end",
            )
        off += need

    if off != len(data):
        _add_issue(
            issues,
            "error",
            "res10",
            archive,
            model_name,
            f"tail bytes after node records: consumed={off}, size={len(data)}",
        )


def _validate_model_payload(
    model_blob: bytes,
    archive: Path,
    model_name: str,
    issues: list[dict[str, Any]],
    counters: Counter[str],
) -> None:
    counters["models_total"] += 1

    if model_blob[:4] != MAGIC_NRES:
        _add_issue(
            issues,
            "error",
            "model-container",
            archive,
            model_name,
            "payload is not NRes (missing magic)",
        )
        return

    try:
        parsed = arv.parse_nres(model_blob, source=f"{archive}:{model_name}")
    except Exception as exc:  # pylint: disable=broad-except
        _add_issue(
            issues,
            "error",
            "model-container",
            archive,
            model_name,
            f"cannot parse nested NRes: {exc}",
        )
        return

    for item in parsed.get("issues", []):
        _add_issue(issues, "warning", "model-container", archive, model_name, str(item))

    entries = parsed["entries"]
    by_type = _entry_by_type(entries)

    res1 = _expect_single_resource(by_type, 1, "Res1", issues, archive, model_name, True)
    res2 = _expect_single_resource(by_type, 2, "Res2", issues, archive, model_name, True)
    res3 = _expect_single_resource(by_type, 3, "Res3", issues, archive, model_name, True)
    res4 = _expect_single_resource(by_type, 4, "Res4", issues, archive, model_name, False)
    res5 = _expect_single_resource(by_type, 5, "Res5", issues, archive, model_name, False)
    res6 = _expect_single_resource(by_type, 6, "Res6", issues, archive, model_name, True)
    res7 = _expect_single_resource(by_type, 7, "Res7", issues, archive, model_name, False)
    res8 = _expect_single_resource(by_type, 8, "Res8", issues, archive, model_name, False)
    res10 = _expect_single_resource(by_type, 10, "Res10", issues, archive, model_name, False)
    res13 = _expect_single_resource(by_type, 13, "Res13", issues, archive, model_name, True)
    res15 = _expect_single_resource(by_type, 15, "Res15", issues, archive, model_name, False)
    res16 = _expect_single_resource(by_type, 16, "Res16", issues, archive, model_name, False)
    res18 = _expect_single_resource(by_type, 18, "Res18", issues, archive, model_name, False)
    res19 = _expect_single_resource(by_type, 19, "Res19", issues, archive, model_name, False)

    if not (res1 and res2 and res3 and res6 and res13):
        return

    # Res1
    res1_stride = int(res1["attr3"])
    if res1_stride not in (38, 24):
        _add_issue(
            issues,
            "warning",
            "res1",
            archive,
            model_name,
            f"unexpected Res1 stride attr3={res1_stride} (known: 38 or 24)",
        )
    if res1_stride <= 0:
        _add_issue(issues, "error", "res1", archive, model_name, f"invalid Res1 stride={res1_stride}")
        return
    if int(res1["size"]) % res1_stride != 0:
        _add_issue(
            issues,
            "error",
            "res1",
            archive,
            model_name,
            f"Res1 size={res1['size']} not divisible by stride={res1_stride}",
        )
        return
    node_count = int(res1["size"]) // res1_stride
    if int(res1["attr1"]) != node_count:
        _add_issue(
            issues,
            "error",
            "res1",
            archive,
            model_name,
            f"Res1 attr1={res1['attr1']} != node_count={node_count}",
        )

    # Res2
    res2_size = int(res2["size"])
    res2_attr1 = int(res2["attr1"])
    res2_attr2 = int(res2["attr2"])
    res2_attr3 = int(res2["attr3"])
    if res2_size < 0x8C:
        _add_issue(issues, "error", "res2", archive, model_name, f"Res2 too small: size={res2_size}")
        return
    slot_bytes = res2_size - 0x8C
    slot_count = -1
    if slot_bytes % 68 != 0:
        _add_issue(
            issues,
            "error",
            "res2",
            archive,
            model_name,
            f"Res2 slot area not divisible by 68: slot_bytes={slot_bytes}",
        )
    else:
        slot_count = slot_bytes // 68
        if res2_attr1 != slot_count:
            _add_issue(
                issues,
                "error",
                "res2",
                archive,
                model_name,
                f"Res2 attr1={res2_attr1} != slot_count={slot_count}",
            )
    if res2_attr2 != 0:
        _add_issue(
            issues,
            "warning",
            "res2",
            archive,
            model_name,
            f"Res2 attr2={res2_attr2} (expected 0 in known assets)",
        )
    if res2_attr3 != 68:
        _add_issue(
            issues,
            "error",
            "res2",
            archive,
            model_name,
            f"Res2 attr3={res2_attr3} != 68",
        )

    # Fixed-stride resources
    vertex_count = _check_fixed_stride(
        entry=res3,
        stride=12,
        label="Res3",
        issues=issues,
        archive=archive,
        model_name=model_name,
    )
    _ = _check_fixed_stride(
        entry=res4,
        stride=4,
        label="Res4",
        issues=issues,
        archive=archive,
        model_name=model_name,
    ) if res4 else None
    _ = _check_fixed_stride(
        entry=res5,
        stride=4,
        label="Res5",
        issues=issues,
        archive=archive,
        model_name=model_name,
    ) if res5 else None
    index_count = _check_fixed_stride(
        entry=res6,
        stride=2,
        label="Res6",
        issues=issues,
        archive=archive,
        model_name=model_name,
    )
    tri_desc_count = _check_fixed_stride(
        entry=res7,
        stride=16,
        label="Res7",
        issues=issues,
        archive=archive,
        model_name=model_name,
    ) if res7 else -1
    anim_key_count = _check_fixed_stride(
        entry=res8,
        stride=24,
        label="Res8",
        issues=issues,
        archive=archive,
        model_name=model_name,
        enforce_attr3=False,  # format stores attr3=4 in data set
    ) if res8 else -1
    if res8 and int(res8["attr3"]) != 4:
        _add_issue(
            issues,
            "error",
            "res8",
            archive,
            model_name,
            f"Res8 attr3={res8['attr3']} != 4",
        )
    if res13:
        batch_count = _check_fixed_stride(
            entry=res13,
            stride=20,
            label="Res13",
            issues=issues,
            archive=archive,
            model_name=model_name,
        )
    else:
        batch_count = -1
    if res15:
        _check_fixed_stride(
            entry=res15,
            stride=8,
            label="Res15",
            issues=issues,
            archive=archive,
            model_name=model_name,
        )
    if res16:
        _check_fixed_stride(
            entry=res16,
            stride=8,
            label="Res16",
            issues=issues,
            archive=archive,
            model_name=model_name,
        )
    if res18:
        _check_fixed_stride(
            entry=res18,
            stride=4,
            label="Res18",
            issues=issues,
            archive=archive,
            model_name=model_name,
        )

    if res19:
        anim_map_count = _check_fixed_stride(
            entry=res19,
            stride=2,
            label="Res19",
            issues=issues,
            archive=archive,
            model_name=model_name,
            enforce_attr3=False,
            enforce_attr2_zero=False,
        )
        if int(res19["attr3"]) != 2:
            _add_issue(
                issues,
                "error",
                "res19",
                archive,
                model_name,
                f"Res19 attr3={res19['attr3']} != 2",
            )
    else:
        anim_map_count = -1

    # Res10
    if res10:
        if int(res10["attr1"]) != int(res1["attr1"]):
            _add_issue(
                issues,
                "error",
                "res10",
                archive,
                model_name,
                f"Res10 attr1={res10['attr1']} != Res1.attr1={res1['attr1']}",
            )
        if int(res10["attr3"]) != 0:
            _add_issue(
                issues,
                "warning",
                "res10",
                archive,
                model_name,
                f"Res10 attr3={res10['attr3']} (known assets use 0)",
            )
        _validate_res10(_entry_payload(model_blob, res10), node_count, issues, archive, model_name)

    # Cross-table checks.
    if vertex_count > 0 and (res4 and int(res4["size"]) // 4 != vertex_count):
        _add_issue(issues, "error", "model-cross", archive, model_name, "Res4 count != Res3 count")
    if vertex_count > 0 and (res5 and int(res5["size"]) // 4 != vertex_count):
        _add_issue(issues, "error", "model-cross", archive, model_name, "Res5 count != Res3 count")

    indices: list[int] = []
    if index_count > 0:
        res6_data = _entry_payload(model_blob, res6)
        indices = list(struct.unpack_from(f"<{index_count}H", res6_data, 0))

    if batch_count > 0:
        res13_data = _entry_payload(model_blob, res13)
        for batch_idx in range(batch_count):
            b_off = batch_idx * 20
            (
                _batch_flags,
                _mat_idx,
                _unk4,
                _unk6,
                idx_count,
                idx_start,
                _unk14,
                base_vertex,
            ) = struct.unpack_from("<HHHHHIHI", res13_data, b_off)
            end = idx_start + idx_count
            if index_count > 0 and end > index_count:
                _add_issue(
                    issues,
                    "error",
                    "res13",
                    archive,
                    model_name,
                    f"batch {batch_idx}: index range [{idx_start}, {end}) outside Res6 count={index_count}",
                )
                continue
            if idx_count % 3 != 0:
                _add_issue(
                    issues,
                    "warning",
                    "res13",
                    archive,
                    model_name,
                    f"batch {batch_idx}: indexCount={idx_count} is not divisible by 3",
                )
            if vertex_count > 0 and index_count > 0 and idx_count > 0:
                raw_slice = indices[idx_start:end]
                max_raw = max(raw_slice)
                if base_vertex + max_raw >= vertex_count:
                    _add_issue(
                        issues,
                        "error",
                        "res13",
                        archive,
                        model_name,
                        f"batch {batch_idx}: baseVertex+maxIndex={base_vertex + max_raw} >= vertex_count={vertex_count}",
                    )

    if slot_count > 0:
        res2_data = _entry_payload(model_blob, res2)
        for slot_idx in range(slot_count):
            s_off = 0x8C + slot_idx * 68
            tri_start, tri_count, batch_start, slot_batch_count = struct.unpack_from("<4H", res2_data, s_off)
            if tri_desc_count > 0 and tri_start + tri_count > tri_desc_count:
                _add_issue(
                    issues,
                    "error",
                    "res2-slot",
                    archive,
                    model_name,
                    f"slot {slot_idx}: tri range [{tri_start}, {tri_start + tri_count}) outside Res7 count={tri_desc_count}",
                )
            if batch_count > 0 and batch_start + slot_batch_count > batch_count:
                _add_issue(
                    issues,
                    "error",
                    "res2-slot",
                    archive,
                    model_name,
                    f"slot {slot_idx}: batch range [{batch_start}, {batch_start + slot_batch_count}) outside Res13 count={batch_count}",
                )
            # Slot bounds are 10 float values.
            for f_idx in range(10):
                value = struct.unpack_from("<f", res2_data, s_off + 8 + f_idx * 4)[0]
                if not math.isfinite(value):
                    _add_issue(
                        issues,
                        "error",
                        "res2-slot",
                        archive,
                        model_name,
                        f"slot {slot_idx}: non-finite bound float at field {f_idx}",
                    )
                    break

    if tri_desc_count > 0:
        res7_data = _entry_payload(model_blob, res7)
        for tri_idx in range(tri_desc_count):
            t_off = tri_idx * 16
            _flags, l0, l1, l2 = struct.unpack_from("<4H", res7_data, t_off)
            for link in (l0, l1, l2):
                if link != 0xFFFF and link >= tri_desc_count:
                    _add_issue(
                        issues,
                        "error",
                        "res7",
                        archive,
                        model_name,
                        f"tri {tri_idx}: link {link} outside tri_desc_count={tri_desc_count}",
                    )
            _ = struct.unpack_from("<H", res7_data, t_off + 14)[0]

    # Node-level constraints for slot matrix / animation mapping.
    if res1_stride == 38:
        res1_data = _entry_payload(model_blob, res1)
        map_words: list[int] = []
        if anim_map_count > 0 and res19:
            res19_data = _entry_payload(model_blob, res19)
            map_words = list(struct.unpack_from(f"<{anim_map_count}H", res19_data, 0))
        frame_count = int(res19["attr2"]) if res19 else 0

        for node_idx in range(node_count):
            n_off = node_idx * 38
            hdr2 = struct.unpack_from("<H", res1_data, n_off + 4)[0]
            hdr3 = struct.unpack_from("<H", res1_data, n_off + 6)[0]
            # Slot matrix: 15 uint16 at +8.
            for w_idx in range(15):
                slot_idx = struct.unpack_from("<H", res1_data, n_off + 8 + w_idx * 2)[0]
                if slot_idx != 0xFFFF and slot_count > 0 and slot_idx >= slot_count:
                    _add_issue(
                        issues,
                        "error",
                        "res1-slot",
                        archive,
                        model_name,
                        f"node {node_idx}: slotIndex[{w_idx}]={slot_idx} outside slot_count={slot_count}",
                    )

            if anim_key_count > 0 and hdr3 != 0xFFFF and hdr3 >= anim_key_count:
                _add_issue(
                    issues,
                    "error",
                    "res1-anim",
                    archive,
                    model_name,
                    f"node {node_idx}: fallbackKeyIndex={hdr3} outside Res8 count={anim_key_count}",
                )
            if map_words and hdr2 != 0xFFFF and frame_count > 0:
                end = hdr2 + frame_count
                if end > len(map_words):
                    _add_issue(
                        issues,
                        "error",
                        "res19-map",
                        archive,
                        model_name,
                        f"node {node_idx}: map range [{hdr2}, {end}) outside Res19 count={len(map_words)}",
                    )

    counters["models_ok"] += 1


def _validate_texm_payload(
    payload: bytes,
    archive: Path,
    entry_name: str,
    issues: list[dict[str, Any]],
    counters: Counter[str],
) -> None:
    counters["texm_total"] += 1

    if len(payload) < 32:
        _add_issue(
            issues,
            "error",
            "texm",
            archive,
            entry_name,
            f"payload too small: {len(payload)}",
        )
        return

    magic, width, height, mip_count, flags4, flags5, unk6, fmt = struct.unpack_from("<8I", payload, 0)
    if magic != TYPE_TEXM:
        _add_issue(issues, "error", "texm", archive, entry_name, f"magic=0x{magic:08X} != Texm")
        return
    if width == 0 or height == 0:
        _add_issue(issues, "error", "texm", archive, entry_name, f"invalid size {width}x{height}")
        return
    if mip_count == 0:
        _add_issue(issues, "error", "texm", archive, entry_name, "mipCount=0")
        return
    if fmt not in TEXM_KNOWN_FORMATS:
        _add_issue(
            issues,
            "error",
            "texm",
            archive,
            entry_name,
            f"unknown format code {fmt}",
        )
        return
    if flags4 not in (0, 32):
        _add_issue(
            issues,
            "warning",
            "texm",
            archive,
            entry_name,
            f"flags4={flags4} (known values: 0 or 32)",
        )
    if flags5 not in (0, 0x04000000, 0x00800000):
        _add_issue(
            issues,
            "warning",
            "texm",
            archive,
            entry_name,
            f"flags5=0x{flags5:08X} (known values: 0, 0x00800000, 0x04000000)",
        )

    bpp = 1 if fmt == 0 else (2 if fmt in (565, 556, 4444) else 4)
    pix_sum = 0
    w = width
    h = height
    for _ in range(mip_count):
        pix_sum += w * h
        w = max(1, w >> 1)
        h = max(1, h >> 1)
    size_core = 32 + (1024 if fmt == 0 else 0) + bpp * pix_sum
    if size_core > len(payload):
        _add_issue(
            issues,
            "error",
            "texm",
            archive,
            entry_name,
            f"sizeCore={size_core} exceeds payload size={len(payload)}",
        )
        return

    tail = len(payload) - size_core
    if tail > 0:
        off = size_core
        if tail < 8:
            _add_issue(
                issues,
                "error",
                "texm",
                archive,
                entry_name,
                f"tail too short for Page chunk: tail={tail}",
            )
            return
        if payload[off : off + 4] != MAGIC_PAGE:
            _add_issue(
                issues,
                "error",
                "texm",
                archive,
                entry_name,
                f"tail is present but no Page magic at offset {off}",
            )
            return
        rect_count = struct.unpack_from("<I", payload, off + 4)[0]
        need = 8 + rect_count * 8
        if need > tail:
            _add_issue(
                issues,
                "error",
                "texm",
                archive,
                entry_name,
                f"Page chunk truncated: need={need}, tail={tail}",
            )
            return
        if need != tail:
            _add_issue(
                issues,
                "error",
                "texm",
                archive,
                entry_name,
                f"extra bytes after Page chunk: tail={tail}, pageSize={need}",
            )
            return

    _ = unk6  # carried as raw field in spec, semantics intentionally unknown.
    counters["texm_ok"] += 1


def _validate_fxid_payload(
    payload: bytes,
    archive: Path,
    entry_name: str,
    issues: list[dict[str, Any]],
    counters: Counter[str],
) -> None:
    counters["fxid_total"] += 1

    if len(payload) < 60:
        _add_issue(
            issues,
            "error",
            "fxid",
            archive,
            entry_name,
            f"payload too small: {len(payload)}",
        )
        return

    cmd_count = struct.unpack_from("<I", payload, 0)[0]
    ptr = 0x3C
    for idx in range(cmd_count):
        if ptr + 4 > len(payload):
            _add_issue(
                issues,
                "error",
                "fxid",
                archive,
                entry_name,
                f"command {idx}: missing header at offset={ptr}",
            )
            return
        word = struct.unpack_from("<I", payload, ptr)[0]
        opcode = word & 0xFF
        if opcode not in FX_CMD_SIZE:
            _add_issue(
                issues,
                "error",
                "fxid",
                archive,
                entry_name,
                f"command {idx}: unknown opcode={opcode} at offset={ptr}",
            )
            return
        size = FX_CMD_SIZE[opcode]
        if ptr + size > len(payload):
            _add_issue(
                issues,
                "error",
                "fxid",
                archive,
                entry_name,
                f"command {idx}: truncated, need end={ptr + size}, payload={len(payload)}",
            )
            return
        ptr += size

    if ptr != len(payload):
        _add_issue(
            issues,
            "error",
            "fxid",
            archive,
            entry_name,
            f"tail bytes after command stream: parsed_end={ptr}, payload={len(payload)}",
        )
        return

    counters["fxid_ok"] += 1


def _scan_nres_files(root: Path) -> list[Path]:
    rows = arv.scan_archives(root)
    out: list[Path] = []
    for item in rows:
        if item["type"] != "nres":
            continue
        out.append(root / item["relative_path"])
    return out


def run_validation(input_root: Path) -> dict[str, Any]:
    archives = _scan_nres_files(input_root)
    issues: list[dict[str, Any]] = []
    counters: Counter[str] = Counter()

    for archive_path in archives:
        counters["archives_total"] += 1
        data = archive_path.read_bytes()
        try:
            parsed = arv.parse_nres(data, source=str(archive_path))
        except Exception as exc:  # pylint: disable=broad-except
            _add_issue(issues, "error", "archive", archive_path, None, f"cannot parse NRes: {exc}")
            continue

        for item in parsed.get("issues", []):
            _add_issue(issues, "warning", "archive", archive_path, None, str(item))

        for entry in parsed["entries"]:
            name = str(entry["name"])
            payload = _entry_payload(data, entry)
            type_id = int(entry["type_id"])

            if name.lower().endswith(".msh"):
                _validate_model_payload(payload, archive_path, name, issues, counters)

            if type_id == TYPE_TEXM:
                _validate_texm_payload(payload, archive_path, name, issues, counters)

            if type_id == TYPE_FXID:
                _validate_fxid_payload(payload, archive_path, name, issues, counters)

    errors = sum(1 for row in issues if row["severity"] == "error")
    warnings = sum(1 for row in issues if row["severity"] == "warning")

    return {
        "input_root": str(input_root),
        "summary": {
            "archives_total": counters["archives_total"],
            "models_total": counters["models_total"],
            "models_ok": counters["models_ok"],
            "texm_total": counters["texm_total"],
            "texm_ok": counters["texm_ok"],
            "fxid_total": counters["fxid_total"],
            "fxid_ok": counters["fxid_ok"],
            "errors": errors,
            "warnings": warnings,
            "issues_total": len(issues),
        },
        "issues": issues,
    }


def cmd_scan(args: argparse.Namespace) -> int:
    root = Path(args.input).resolve()
    report = run_validation(root)
    summary = report["summary"]
    print(f"Input root    : {root}")
    print(f"NRes archives : {summary['archives_total']}")
    print(f"MSH models    : {summary['models_total']}")
    print(f"Texm textures : {summary['texm_total']}")
    print(f"FXID effects  : {summary['fxid_total']}")
    return 0


def cmd_validate(args: argparse.Namespace) -> int:
    root = Path(args.input).resolve()
    report = run_validation(root)
    summary = report["summary"]

    if args.report:
        arv.dump_json(Path(args.report).resolve(), report)

    print(f"Input root    : {root}")
    print(f"NRes archives : {summary['archives_total']}")
    print(f"MSH models    : {summary['models_ok']}/{summary['models_total']} valid")
    print(f"Texm textures : {summary['texm_ok']}/{summary['texm_total']} valid")
    print(f"FXID effects  : {summary['fxid_ok']}/{summary['fxid_total']} valid")
    print(f"Issues        : {summary['issues_total']} (errors={summary['errors']}, warnings={summary['warnings']})")

    if report["issues"]:
        limit = max(1, int(args.print_limit))
        print("\nSample issues:")
        for item in report["issues"][:limit]:
            where = item["archive"]
            if item["entry"]:
                where = f"{where}::{item['entry']}"
            print(f"- [{item['severity']}] [{item['category']}] {where}: {item['message']}")
        if len(report["issues"]) > limit:
            print(f"... and {len(report['issues']) - limit} more issue(s)")

    if summary["errors"] > 0:
        return 1
    if args.fail_on_warnings and summary["warnings"] > 0:
        return 1
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Validate docs/specs/msh.md assumptions on real archives."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    scan = sub.add_parser("scan", help="Quick scan and counts (models/textures/effects).")
    scan.add_argument("--input", required=True, help="Root directory with game/test archives.")
    scan.set_defaults(func=cmd_scan)

    validate = sub.add_parser("validate", help="Run full spec validation.")
    validate.add_argument("--input", required=True, help="Root directory with game/test archives.")
    validate.add_argument("--report", help="Optional JSON report output path.")
    validate.add_argument(
        "--print-limit",
        type=int,
        default=50,
        help="How many issues to print to stdout (default: 50).",
    )
    validate.add_argument(
        "--fail-on-warnings",
        action="store_true",
        help="Return non-zero if warnings are present.",
    )
    validate.set_defaults(func=cmd_validate)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
