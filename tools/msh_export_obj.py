#!/usr/bin/env python3
"""
Export NGI MSH geometry to Wavefront OBJ.

The exporter is intended for inspection/debugging and uses the same
batch/slot selection logic as msh_preview_renderer.py.
"""

from __future__ import annotations

import argparse
import math
import struct
from pathlib import Path
from typing import Any

import archive_roundtrip_validator as arv

MAGIC_NRES = b"NRes"


def _entry_payload(blob: bytes, entry: dict[str, Any]) -> bytes:
    start = int(entry["data_offset"])
    end = start + int(entry["size"])
    return blob[start:end]


def _parse_nres(blob: bytes, source: str) -> dict[str, Any]:
    if blob[:4] != MAGIC_NRES:
        raise RuntimeError(f"{source}: not an NRes payload")
    return arv.parse_nres(blob, source=source)


def _by_type(entries: list[dict[str, Any]]) -> dict[int, list[dict[str, Any]]]:
    out: dict[int, list[dict[str, Any]]] = {}
    for row in entries:
        out.setdefault(int(row["type_id"]), []).append(row)
    return out


def _get_single(by_type: dict[int, list[dict[str, Any]]], type_id: int, label: str) -> dict[str, Any]:
    rows = by_type.get(type_id, [])
    if not rows:
        raise RuntimeError(f"missing resource type {type_id} ({label})")
    return rows[0]


def _pick_model_payload(archive_path: Path, model_name: str | None) -> tuple[bytes, str]:
    root_blob = archive_path.read_bytes()
    parsed = _parse_nres(root_blob, str(archive_path))

    msh_entries = [row for row in parsed["entries"] if str(row["name"]).lower().endswith(".msh")]
    if msh_entries:
        chosen: dict[str, Any] | None = None
        if model_name:
            model_l = model_name.lower()
            for row in msh_entries:
                name_l = str(row["name"]).lower()
                if name_l == model_l:
                    chosen = row
                    break
            if chosen is None:
                for row in msh_entries:
                    if str(row["name"]).lower().startswith(model_l):
                        chosen = row
                        break
        else:
            chosen = msh_entries[0]

        if chosen is None:
            names = ", ".join(str(row["name"]) for row in msh_entries[:12])
            raise RuntimeError(
                f"model '{model_name}' not found in {archive_path}. Available: {names}"
            )
        return _entry_payload(root_blob, chosen), str(chosen["name"])

    by_type = _by_type(parsed["entries"])
    if all(k in by_type for k in (1, 2, 3, 6, 13)):
        return root_blob, archive_path.name

    raise RuntimeError(
        f"{archive_path} does not contain .msh entries and does not look like a direct model payload"
    )


def _extract_geometry(
    model_blob: bytes,
    *,
    lod: int,
    group: int,
    max_faces: int,
    all_batches: bool,
) -> tuple[list[tuple[float, float, float]], list[tuple[int, int, int]], dict[str, int]]:
    parsed = _parse_nres(model_blob, "<model>")
    by_type = _by_type(parsed["entries"])

    res1 = _get_single(by_type, 1, "Res1")
    res2 = _get_single(by_type, 2, "Res2")
    res3 = _get_single(by_type, 3, "Res3")
    res6 = _get_single(by_type, 6, "Res6")
    res13 = _get_single(by_type, 13, "Res13")

    pos_blob = _entry_payload(model_blob, res3)
    if len(pos_blob) % 12 != 0:
        raise RuntimeError(f"Res3 size is not divisible by 12: {len(pos_blob)}")
    vertex_count = len(pos_blob) // 12
    positions = [struct.unpack_from("<3f", pos_blob, i * 12) for i in range(vertex_count)]

    idx_blob = _entry_payload(model_blob, res6)
    if len(idx_blob) % 2 != 0:
        raise RuntimeError(f"Res6 size is not divisible by 2: {len(idx_blob)}")
    index_count = len(idx_blob) // 2
    indices = list(struct.unpack_from(f"<{index_count}H", idx_blob, 0))

    batch_blob = _entry_payload(model_blob, res13)
    if len(batch_blob) % 20 != 0:
        raise RuntimeError(f"Res13 size is not divisible by 20: {len(batch_blob)}")
    batch_count = len(batch_blob) // 20
    batches: list[tuple[int, int, int, int]] = []
    for i in range(batch_count):
        off = i * 20
        idx_count = struct.unpack_from("<H", batch_blob, off + 8)[0]
        idx_start = struct.unpack_from("<I", batch_blob, off + 10)[0]
        base_vertex = struct.unpack_from("<I", batch_blob, off + 16)[0]
        batches.append((idx_count, idx_start, base_vertex, i))

    res2_blob = _entry_payload(model_blob, res2)
    if len(res2_blob) < 0x8C:
        raise RuntimeError("Res2 is too small (< 0x8C)")
    slot_blob = res2_blob[0x8C:]
    if len(slot_blob) % 68 != 0:
        raise RuntimeError(f"Res2 slot area is not divisible by 68: {len(slot_blob)}")
    slot_count = len(slot_blob) // 68
    slots: list[tuple[int, int, int, int]] = []
    for i in range(slot_count):
        off = i * 68
        tri_start, tri_count, batch_start, slot_batch_count = struct.unpack_from("<4H", slot_blob, off)
        slots.append((tri_start, tri_count, batch_start, slot_batch_count))

    res1_blob = _entry_payload(model_blob, res1)
    node_stride = int(res1["attr3"])
    node_count = int(res1["attr1"])
    node_slot_indices: list[int] = []
    if not all_batches and node_stride >= 38 and len(res1_blob) >= node_count * node_stride:
        if lod < 0 or lod > 2:
            raise RuntimeError(f"lod must be 0..2 (got {lod})")
        if group < 0 or group > 4:
            raise RuntimeError(f"group must be 0..4 (got {group})")
        matrix_index = lod * 5 + group
        for n in range(node_count):
            off = n * node_stride + 8 + matrix_index * 2
            slot_idx = struct.unpack_from("<H", res1_blob, off)[0]
            if slot_idx == 0xFFFF:
                continue
            if slot_idx >= slot_count:
                continue
            node_slot_indices.append(slot_idx)

    faces: list[tuple[int, int, int]] = []
    used_batches = 0
    used_slots = 0

    def append_batch(batch_idx: int) -> None:
        nonlocal used_batches
        if batch_idx < 0 or batch_idx >= len(batches):
            return
        idx_count, idx_start, base_vertex, _ = batches[batch_idx]
        if idx_count < 3:
            return
        end = idx_start + idx_count
        if end > len(indices):
            return
        used_batches += 1
        tri_count = idx_count // 3
        for t in range(tri_count):
            i0 = indices[idx_start + t * 3 + 0] + base_vertex
            i1 = indices[idx_start + t * 3 + 1] + base_vertex
            i2 = indices[idx_start + t * 3 + 2] + base_vertex
            if i0 >= vertex_count or i1 >= vertex_count or i2 >= vertex_count:
                continue
            faces.append((i0, i1, i2))
            if len(faces) >= max_faces:
                return

    if node_slot_indices:
        for slot_idx in node_slot_indices:
            if len(faces) >= max_faces:
                break
            _tri_start, _tri_count, batch_start, slot_batch_count = slots[slot_idx]
            used_slots += 1
            for bi in range(batch_start, batch_start + slot_batch_count):
                append_batch(bi)
                if len(faces) >= max_faces:
                    break
    else:
        for bi in range(batch_count):
            append_batch(bi)
            if len(faces) >= max_faces:
                break

    if not faces:
        raise RuntimeError("no faces selected for export")

    meta = {
        "vertex_count": vertex_count,
        "index_count": index_count,
        "batch_count": batch_count,
        "slot_count": slot_count,
        "node_count": node_count,
        "used_slots": used_slots,
        "used_batches": used_batches,
        "face_count": len(faces),
    }
    return positions, faces, meta


def _compute_vertex_normals(
    positions: list[tuple[float, float, float]],
    faces: list[tuple[int, int, int]],
) -> list[tuple[float, float, float]]:
    acc = [[0.0, 0.0, 0.0] for _ in positions]
    for i0, i1, i2 in faces:
        p0 = positions[i0]
        p1 = positions[i1]
        p2 = positions[i2]
        ux = p1[0] - p0[0]
        uy = p1[1] - p0[1]
        uz = p1[2] - p0[2]
        vx = p2[0] - p0[0]
        vy = p2[1] - p0[1]
        vz = p2[2] - p0[2]
        nx = uy * vz - uz * vy
        ny = uz * vx - ux * vz
        nz = ux * vy - uy * vx
        acc[i0][0] += nx
        acc[i0][1] += ny
        acc[i0][2] += nz
        acc[i1][0] += nx
        acc[i1][1] += ny
        acc[i1][2] += nz
        acc[i2][0] += nx
        acc[i2][1] += ny
        acc[i2][2] += nz

    normals: list[tuple[float, float, float]] = []
    for nx, ny, nz in acc:
        ln = math.sqrt(nx * nx + ny * ny + nz * nz)
        if ln <= 1e-12:
            normals.append((0.0, 1.0, 0.0))
        else:
            normals.append((nx / ln, ny / ln, nz / ln))
    return normals


def _write_obj(
    output_path: Path,
    object_name: str,
    positions: list[tuple[float, float, float]],
    faces: list[tuple[int, int, int]],
) -> None:
    output_path.parent.mkdir(parents=True, exist_ok=True)
    normals = _compute_vertex_normals(positions, faces)

    with output_path.open("w", encoding="utf-8", newline="\n") as out:
        out.write("# Exported by msh_export_obj.py\n")
        out.write(f"o {object_name}\n")
        for x, y, z in positions:
            out.write(f"v {x:.9g} {y:.9g} {z:.9g}\n")
        for nx, ny, nz in normals:
            out.write(f"vn {nx:.9g} {ny:.9g} {nz:.9g}\n")
        for i0, i1, i2 in faces:
            a = i0 + 1
            b = i1 + 1
            c = i2 + 1
            out.write(f"f {a}//{a} {b}//{b} {c}//{c}\n")


def cmd_list_models(args: argparse.Namespace) -> int:
    archive_path = Path(args.archive).resolve()
    blob = archive_path.read_bytes()
    parsed = _parse_nres(blob, str(archive_path))
    rows = [row for row in parsed["entries"] if str(row["name"]).lower().endswith(".msh")]
    print(f"Archive: {archive_path}")
    print(f"MSH entries: {len(rows)}")
    for row in rows:
        print(f"- {row['name']}")
    return 0


def cmd_export(args: argparse.Namespace) -> int:
    archive_path = Path(args.archive).resolve()
    output_path = Path(args.output).resolve()

    model_blob, model_label = _pick_model_payload(archive_path, args.model)
    positions, faces, meta = _extract_geometry(
        model_blob,
        lod=int(args.lod),
        group=int(args.group),
        max_faces=int(args.max_faces),
        all_batches=bool(args.all_batches),
    )
    obj_name = Path(model_label).stem or "msh_model"
    _write_obj(output_path, obj_name, positions, faces)

    print(f"Exported model : {model_label}")
    print(f"Output OBJ     : {output_path}")
    print(f"Object name    : {obj_name}")
    print(
        "Geometry       : "
        f"vertices={meta['vertex_count']}, faces={meta['face_count']}, "
        f"batches={meta['used_batches']}/{meta['batch_count']}, slots={meta['used_slots']}/{meta['slot_count']}"
    )
    print(
        "Mode           : "
        f"lod={args.lod}, group={args.group}, all_batches={bool(args.all_batches)}"
    )
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Export NGI MSH geometry to Wavefront OBJ."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    list_models = sub.add_parser("list-models", help="List .msh entries in an NRes archive.")
    list_models.add_argument("--archive", required=True, help="Path to archive (e.g. animals.rlb).")
    list_models.set_defaults(func=cmd_list_models)

    export = sub.add_parser("export", help="Export one model to OBJ.")
    export.add_argument("--archive", required=True, help="Path to NRes archive or direct model payload.")
    export.add_argument(
        "--model",
        help="Model entry name (*.msh) inside archive. If omitted, first .msh is used.",
    )
    export.add_argument("--output", required=True, help="Output .obj path.")
    export.add_argument("--lod", type=int, default=0, help="LOD index 0..2 (default: 0).")
    export.add_argument("--group", type=int, default=0, help="Group index 0..4 (default: 0).")
    export.add_argument("--max-faces", type=int, default=120000, help="Face limit (default: 120000).")
    export.add_argument(
        "--all-batches",
        action="store_true",
        help="Ignore slot matrix selection and export all batches.",
    )
    export.set_defaults(func=cmd_export)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
