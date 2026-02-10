#!/usr/bin/env python3
"""
Primitive software renderer for NGI MSH models.

Output format: binary PPM (P6), no external dependencies.
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

    # Fallback: treat file itself as a model NRes payload.
    by_type = _by_type(parsed["entries"])
    if all(k in by_type for k in (1, 2, 3, 6, 13)):
        return root_blob, archive_path.name

    raise RuntimeError(
        f"{archive_path} does not contain .msh entries and does not look like a direct model payload"
    )


def _get_single(by_type: dict[int, list[dict[str, Any]]], type_id: int, label: str) -> dict[str, Any]:
    rows = by_type.get(type_id, [])
    if not rows:
        raise RuntimeError(f"missing resource type {type_id} ({label})")
    return rows[0]


def _extract_geometry(
    model_blob: bytes,
    *,
    lod: int,
    group: int,
    max_faces: int,
) -> tuple[list[tuple[float, float, float]], list[tuple[int, int, int]], dict[str, int]]:
    parsed = _parse_nres(model_blob, "<model>")
    by_type = _by_type(parsed["entries"])

    res1 = _get_single(by_type, 1, "Res1")
    res2 = _get_single(by_type, 2, "Res2")
    res3 = _get_single(by_type, 3, "Res3")
    res6 = _get_single(by_type, 6, "Res6")
    res13 = _get_single(by_type, 13, "Res13")

    # Positions
    pos_blob = _entry_payload(model_blob, res3)
    if len(pos_blob) % 12 != 0:
        raise RuntimeError(f"Res3 size is not divisible by 12: {len(pos_blob)}")
    vertex_count = len(pos_blob) // 12
    positions = [struct.unpack_from("<3f", pos_blob, i * 12) for i in range(vertex_count)]

    # Indices
    idx_blob = _entry_payload(model_blob, res6)
    if len(idx_blob) % 2 != 0:
        raise RuntimeError(f"Res6 size is not divisible by 2: {len(idx_blob)}")
    index_count = len(idx_blob) // 2
    indices = list(struct.unpack_from(f"<{index_count}H", idx_blob, 0))

    # Batches
    batch_blob = _entry_payload(model_blob, res13)
    if len(batch_blob) % 20 != 0:
        raise RuntimeError(f"Res13 size is not divisible by 20: {len(batch_blob)}")
    batch_count = len(batch_blob) // 20
    batches: list[tuple[int, int, int, int]] = []
    for i in range(batch_count):
        off = i * 20
        # Keep only fields used by renderer:
        # indexCount, indexStart, baseVertex
        idx_count = struct.unpack_from("<H", batch_blob, off + 8)[0]
        idx_start = struct.unpack_from("<I", batch_blob, off + 10)[0]
        base_vertex = struct.unpack_from("<I", batch_blob, off + 16)[0]
        batches.append((idx_count, idx_start, base_vertex, i))

    # Slots
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

    # Nodes / slot matrix
    res1_blob = _entry_payload(model_blob, res1)
    node_stride = int(res1["attr3"])
    node_count = int(res1["attr1"])
    node_slot_indices: list[int] = []
    if node_stride >= 38 and len(res1_blob) >= node_count * node_stride:
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

    # Build triangle list.
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
        # Fallback if slot matrix is unavailable: draw all batches.
        for bi in range(batch_count):
            append_batch(bi)
            if len(faces) >= max_faces:
                break

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
    if not faces:
        raise RuntimeError("no faces selected for rendering")
    return positions, faces, meta


def _write_ppm(path: Path, width: int, height: int, rgb: bytearray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as handle:
        handle.write(f"P6\n{width} {height}\n255\n".encode("ascii"))
        handle.write(rgb)


def _render_software(
    positions: list[tuple[float, float, float]],
    faces: list[tuple[int, int, int]],
    *,
    width: int,
    height: int,
    yaw_deg: float,
    pitch_deg: float,
    wireframe: bool,
) -> bytearray:
    xs = [p[0] for p in positions]
    ys = [p[1] for p in positions]
    zs = [p[2] for p in positions]
    cx = (min(xs) + max(xs)) * 0.5
    cy = (min(ys) + max(ys)) * 0.5
    cz = (min(zs) + max(zs)) * 0.5
    span = max(max(xs) - min(xs), max(ys) - min(ys), max(zs) - min(zs))
    radius = max(span * 0.5, 1e-3)

    yaw = math.radians(yaw_deg)
    pitch = math.radians(pitch_deg)
    cyaw = math.cos(yaw)
    syaw = math.sin(yaw)
    cpitch = math.cos(pitch)
    spitch = math.sin(pitch)

    camera_dist = radius * 3.2
    scale = min(width, height) * 0.95

    # Transform all vertices once.
    vx: list[float] = []
    vy: list[float] = []
    vz: list[float] = []
    sx: list[float] = []
    sy: list[float] = []
    for x, y, z in positions:
        x0 = x - cx
        y0 = y - cy
        z0 = z - cz
        x1 = cyaw * x0 + syaw * z0
        z1 = -syaw * x0 + cyaw * z0
        y2 = cpitch * y0 - spitch * z1
        z2 = spitch * y0 + cpitch * z1 + camera_dist
        if z2 < 1e-3:
            z2 = 1e-3
        vx.append(x1)
        vy.append(y2)
        vz.append(z2)
        sx.append(width * 0.5 + (x1 / z2) * scale)
        sy.append(height * 0.5 - (y2 / z2) * scale)

    rgb = bytearray([16, 18, 24] * (width * height))
    zbuf = [float("inf")] * (width * height)
    light_dir = (0.35, 0.45, 1.0)
    l_len = math.sqrt(light_dir[0] ** 2 + light_dir[1] ** 2 + light_dir[2] ** 2)
    light = (light_dir[0] / l_len, light_dir[1] / l_len, light_dir[2] / l_len)

    def edge(ax: float, ay: float, bx: float, by: float, px: float, py: float) -> float:
        return (px - ax) * (by - ay) - (py - ay) * (bx - ax)

    for i0, i1, i2 in faces:
        x0 = sx[i0]
        y0 = sy[i0]
        x1 = sx[i1]
        y1 = sy[i1]
        x2 = sx[i2]
        y2 = sy[i2]
        area = edge(x0, y0, x1, y1, x2, y2)
        if area == 0.0:
            continue

        # Shading from camera-space normal.
        ux = vx[i1] - vx[i0]
        uy = vy[i1] - vy[i0]
        uz = vz[i1] - vz[i0]
        wx = vx[i2] - vx[i0]
        wy = vy[i2] - vy[i0]
        wz = vz[i2] - vz[i0]
        nx = uy * wz - uz * wy
        ny = uz * wx - ux * wz
        nz = ux * wy - uy * wx
        n_len = math.sqrt(nx * nx + ny * ny + nz * nz)
        if n_len > 0.0:
            nx /= n_len
            ny /= n_len
            nz /= n_len
        intensity = nx * light[0] + ny * light[1] + nz * light[2]
        if intensity < 0.0:
            intensity = 0.0
        shade = int(45 + 200 * intensity)
        color = (shade, shade, min(255, shade + 18))

        minx = int(max(0, math.floor(min(x0, x1, x2))))
        maxx = int(min(width - 1, math.ceil(max(x0, x1, x2))))
        miny = int(max(0, math.floor(min(y0, y1, y2))))
        maxy = int(min(height - 1, math.ceil(max(y0, y1, y2))))
        if minx > maxx or miny > maxy:
            continue

        z0 = vz[i0]
        z1 = vz[i1]
        z2 = vz[i2]

        for py in range(miny, maxy + 1):
            fy = py + 0.5
            row = py * width
            for px in range(minx, maxx + 1):
                fx = px + 0.5
                w0 = edge(x1, y1, x2, y2, fx, fy)
                w1 = edge(x2, y2, x0, y0, fx, fy)
                w2 = edge(x0, y0, x1, y1, fx, fy)
                if area > 0:
                    if w0 < 0 or w1 < 0 or w2 < 0:
                        continue
                else:
                    if w0 > 0 or w1 > 0 or w2 > 0:
                        continue
                inv_area = 1.0 / area
                bz0 = w0 * inv_area
                bz1 = w1 * inv_area
                bz2 = w2 * inv_area
                depth = bz0 * z0 + bz1 * z1 + bz2 * z2
                idx = row + px
                if depth >= zbuf[idx]:
                    continue
                zbuf[idx] = depth
                p = idx * 3
                rgb[p + 0] = color[0]
                rgb[p + 1] = color[1]
                rgb[p + 2] = color[2]

    if wireframe:
        def draw_line(xa: float, ya: float, xb: float, yb: float) -> None:
            x0i = int(round(xa))
            y0i = int(round(ya))
            x1i = int(round(xb))
            y1i = int(round(yb))
            dx = abs(x1i - x0i)
            sx_step = 1 if x0i < x1i else -1
            dy = -abs(y1i - y0i)
            sy_step = 1 if y0i < y1i else -1
            err = dx + dy
            x = x0i
            y = y0i
            while True:
                if 0 <= x < width and 0 <= y < height:
                    p = (y * width + x) * 3
                    rgb[p + 0] = 240
                    rgb[p + 1] = 245
                    rgb[p + 2] = 255
                if x == x1i and y == y1i:
                    break
                e2 = 2 * err
                if e2 >= dy:
                    err += dy
                    x += sx_step
                if e2 <= dx:
                    err += dx
                    y += sy_step

        for i0, i1, i2 in faces:
            draw_line(sx[i0], sy[i0], sx[i1], sy[i1])
            draw_line(sx[i1], sy[i1], sx[i2], sy[i2])
            draw_line(sx[i2], sy[i2], sx[i0], sy[i0])

    return rgb


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


def cmd_render(args: argparse.Namespace) -> int:
    archive_path = Path(args.archive).resolve()
    output_path = Path(args.output).resolve()

    model_blob, model_label = _pick_model_payload(archive_path, args.model)
    positions, faces, meta = _extract_geometry(
        model_blob,
        lod=int(args.lod),
        group=int(args.group),
        max_faces=int(args.max_faces),
    )
    rgb = _render_software(
        positions,
        faces,
        width=int(args.width),
        height=int(args.height),
        yaw_deg=float(args.yaw),
        pitch_deg=float(args.pitch),
        wireframe=bool(args.wireframe),
    )
    _write_ppm(output_path, int(args.width), int(args.height), rgb)

    print(f"Rendered model: {model_label}")
    print(f"Output        : {output_path}")
    print(
        "Geometry      : "
        f"vertices={meta['vertex_count']}, faces={meta['face_count']}, "
        f"batches={meta['used_batches']}/{meta['batch_count']}, slots={meta['used_slots']}/{meta['slot_count']}"
    )
    print(f"Mode          : lod={args.lod}, group={args.group}, wireframe={bool(args.wireframe)}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Primitive NGI MSH renderer (software, dependency-free)."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    list_models = sub.add_parser("list-models", help="List .msh entries in an NRes archive.")
    list_models.add_argument("--archive", required=True, help="Path to archive (e.g. animals.rlb).")
    list_models.set_defaults(func=cmd_list_models)

    render = sub.add_parser("render", help="Render one model to PPM image.")
    render.add_argument("--archive", required=True, help="Path to NRes archive or direct model payload.")
    render.add_argument(
        "--model",
        help="Model entry name (*.msh) inside archive. If omitted, first .msh is used.",
    )
    render.add_argument("--output", required=True, help="Output .ppm file path.")
    render.add_argument("--lod", type=int, default=0, help="LOD index 0..2 (default: 0).")
    render.add_argument("--group", type=int, default=0, help="Group index 0..4 (default: 0).")
    render.add_argument("--max-faces", type=int, default=120000, help="Face limit (default: 120000).")
    render.add_argument("--width", type=int, default=1280, help="Image width (default: 1280).")
    render.add_argument("--height", type=int, default=720, help="Image height (default: 720).")
    render.add_argument("--yaw", type=float, default=35.0, help="Yaw angle in degrees (default: 35).")
    render.add_argument("--pitch", type=float, default=18.0, help="Pitch angle in degrees (default: 18).")
    render.add_argument("--wireframe", action="store_true", help="Draw white wireframe overlay.")
    render.set_defaults(func=cmd_render)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
