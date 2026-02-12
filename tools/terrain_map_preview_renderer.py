#!/usr/bin/env python3
"""
Software 3D renderer for terrain Land.msh + Land.map overlay.

Output format: binary PPM (P6), dependency-free.
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


def _downsample_faces(
    faces: list[tuple[int, int, int]],
    max_faces: int,
) -> list[tuple[int, int, int]]:
    if max_faces <= 0 or len(faces) <= max_faces:
        return faces
    step = len(faces) / max_faces
    out: list[tuple[int, int, int]] = []
    pos = 0.0
    while len(out) < max_faces and int(pos) < len(faces):
        out.append(faces[int(pos)])
        pos += step
    return out


def load_terrain_msh(
    path: Path,
    *,
    max_faces: int,
) -> tuple[list[tuple[float, float, float]], list[tuple[int, int, int]], dict[str, int]]:
    blob = path.read_bytes()
    parsed = _parse_nres(blob, str(path))
    by_type = _by_type(parsed["entries"])

    res3 = _get_single(by_type, 3, "positions")
    res21 = _get_single(by_type, 21, "terrain faces")

    pos_blob = _entry_payload(blob, res3)
    if len(pos_blob) % 12 != 0:
        raise RuntimeError(f"{path}: type 3 payload size is not divisible by 12")
    vertex_count = len(pos_blob) // 12
    positions = [struct.unpack_from("<3f", pos_blob, i * 12) for i in range(vertex_count)]

    face_blob = _entry_payload(blob, res21)
    if len(face_blob) % 28 != 0:
        raise RuntimeError(f"{path}: type 21 payload size is not divisible by 28")
    all_faces: list[tuple[int, int, int]] = []
    raw_face_count = len(face_blob) // 28
    dropped = 0
    for i in range(raw_face_count):
        off = i * 28
        i0, i1, i2 = struct.unpack_from("<HHH", face_blob, off + 8)
        if i0 >= vertex_count or i1 >= vertex_count or i2 >= vertex_count:
            dropped += 1
            continue
        all_faces.append((i0, i1, i2))

    faces = _downsample_faces(all_faces, max_faces)
    meta = {
        "vertex_count": vertex_count,
        "face_count_raw": raw_face_count,
        "face_count_valid": len(all_faces),
        "face_count_rendered": len(faces),
        "face_dropped_invalid": dropped,
    }
    return positions, faces, meta


def load_areal_map(path: Path) -> tuple[list[dict[str, Any]], dict[str, int]]:
    blob = path.read_bytes()
    parsed = _parse_nres(blob, str(path))
    by_type = _by_type(parsed["entries"])
    chunk = _get_single(by_type, 12, "ArealMapGeometry")

    payload = _entry_payload(blob, chunk)
    areal_count = int(chunk["attr1"])
    ptr = 0
    areals: list[dict[str, Any]] = []
    for idx in range(areal_count):
        if ptr + 56 > len(payload):
            raise RuntimeError(f"{path}: truncated areal header at index={idx}")
        class_id = struct.unpack_from("<I", payload, ptr + 40)[0]
        vertex_count, poly_count = struct.unpack_from("<II", payload, ptr + 48)
        verts_off = ptr + 56
        verts_size = 12 * vertex_count
        if verts_off + verts_size > len(payload):
            raise RuntimeError(f"{path}: areal[{idx}] vertices out of bounds")
        verts = [struct.unpack_from("<3f", payload, verts_off + 12 * i) for i in range(vertex_count)]

        links_off = verts_off + verts_size
        links_size = 8 * (vertex_count + 3 * poly_count)
        p = links_off + links_size
        for _ in range(poly_count):
            if p + 4 > len(payload):
                raise RuntimeError(f"{path}: areal[{idx}] poly header out of bounds")
            n = struct.unpack_from("<I", payload, p)[0]
            p += 4 * (3 * n + 1)
            if p > len(payload):
                raise RuntimeError(f"{path}: areal[{idx}] poly data out of bounds")

        areals.append(
            {
                "index": idx,
                "class_id": class_id,
                "vertices": verts,
            }
        )
        ptr = p

    if ptr + 8 > len(payload):
        raise RuntimeError(f"{path}: missing cells section")
    cells_x, cells_y = struct.unpack_from("<II", payload, ptr)
    ptr += 8
    for _x in range(cells_x):
        for _y in range(cells_y):
            if ptr + 2 > len(payload):
                raise RuntimeError(f"{path}: cells section truncated")
            hit_count = struct.unpack_from("<H", payload, ptr)[0]
            ptr += 2 + 2 * hit_count
            if ptr > len(payload):
                raise RuntimeError(f"{path}: cells section out of bounds")
    if ptr != len(payload):
        raise RuntimeError(f"{path}: trailing bytes in chunk12 parse ({len(payload) - ptr})")

    meta = {
        "areal_count": areal_count,
        "cells_x": cells_x,
        "cells_y": cells_y,
    }
    return areals, meta


def _color_for_class(class_id: int) -> tuple[int, int, int]:
    x = (class_id * 1103515245 + 12345) & 0x7FFFFFFF
    r = 60 + (x & 0x7F)
    g = 60 + ((x >> 7) & 0x7F)
    b = 60 + ((x >> 14) & 0x7F)
    return r, g, b


def _write_ppm(path: Path, width: int, height: int, rgb: bytearray) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as handle:
        handle.write(f"P6\n{width} {height}\n255\n".encode("ascii"))
        handle.write(rgb)


def _render_scene(
    terrain_positions: list[tuple[float, float, float]],
    terrain_faces: list[tuple[int, int, int]],
    areals: list[dict[str, Any]],
    *,
    width: int,
    height: int,
    yaw_deg: float,
    pitch_deg: float,
    wireframe: bool,
    areal_overlay: bool,
) -> bytearray:
    all_positions = list(terrain_positions)
    if areal_overlay:
        for area in areals:
            all_positions.extend(area["vertices"])
    if not all_positions:
        raise RuntimeError("scene is empty")

    xs = [p[0] for p in all_positions]
    ys = [p[1] for p in all_positions]
    zs = [p[2] for p in all_positions]
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
    scale = min(width, height) * 0.96

    # Terrain transform cache.
    vx: list[float] = []
    vy: list[float] = []
    vz: list[float] = []
    sx: list[float] = []
    sy: list[float] = []
    for x, y, z in terrain_positions:
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

    def project_point(x: float, y: float, z: float) -> tuple[float, float, float]:
        x0 = x - cx
        y0 = y - cy
        z0 = z - cz
        x1 = cyaw * x0 + syaw * z0
        z1 = -syaw * x0 + cyaw * z0
        y2 = cpitch * y0 - spitch * z1
        z2 = spitch * y0 + cpitch * z1 + camera_dist
        if z2 < 1e-3:
            z2 = 1e-3
        px = width * 0.5 + (x1 / z2) * scale
        py = height * 0.5 - (y2 / z2) * scale
        return px, py, z2

    rgb = bytearray([14, 16, 20] * (width * height))
    zbuf = [float("inf")] * (width * height)
    light_dir = (0.35, 0.45, 1.0)
    l_len = math.sqrt(light_dir[0] ** 2 + light_dir[1] ** 2 + light_dir[2] ** 2)
    light = (light_dir[0] / l_len, light_dir[1] / l_len, light_dir[2] / l_len)

    def edge(ax: float, ay: float, bx: float, by: float, px: float, py: float) -> float:
        return (px - ax) * (by - ay) - (py - ay) * (bx - ax)

    for i0, i1, i2 in terrain_faces:
        x0 = sx[i0]
        y0 = sy[i0]
        x1 = sx[i1]
        y1 = sy[i1]
        x2 = sx[i2]
        y2 = sy[i2]
        area = edge(x0, y0, x1, y1, x2, y2)
        if area == 0.0:
            continue

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
        shade = int(45 + 185 * intensity)
        color = (min(255, shade + 6), min(255, shade + 14), min(255, shade + 28))

        minx = int(max(0, math.floor(min(x0, x1, x2))))
        maxx = int(min(width - 1, math.ceil(max(x0, x1, x2))))
        miny = int(max(0, math.floor(min(y0, y1, y2))))
        maxy = int(min(height - 1, math.ceil(max(y0, y1, y2))))
        if minx > maxx or miny > maxy:
            continue

        z0 = vz[i0]
        z1 = vz[i1]
        z2 = vz[i2]
        inv_area = 1.0 / area
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

    def draw_line(
        xa: float,
        ya: float,
        xb: float,
        yb: float,
        color: tuple[int, int, int],
    ) -> None:
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
                rgb[p + 0] = color[0]
                rgb[p + 1] = color[1]
                rgb[p + 2] = color[2]
            if x == x1i and y == y1i:
                break
            e2 = 2 * err
            if e2 >= dy:
                err += dy
                x += sx_step
            if e2 <= dx:
                err += dx
                y += sy_step

    if wireframe:
        wf = (225, 232, 246)
        for i0, i1, i2 in terrain_faces:
            draw_line(sx[i0], sy[i0], sx[i1], sy[i1], wf)
            draw_line(sx[i1], sy[i1], sx[i2], sy[i2], wf)
            draw_line(sx[i2], sy[i2], sx[i0], sy[i0], wf)

    if areal_overlay:
        for area in areals:
            verts = area["vertices"]
            if len(verts) < 2:
                continue
            color = _color_for_class(int(area["class_id"]))
            projected = [project_point(x, y, z + 0.35) for x, y, z in verts]
            for i in range(len(projected)):
                x0, y0, _ = projected[i]
                x1, y1, _ = projected[(i + 1) % len(projected)]
                draw_line(x0, y0, x1, y1, color)

    return rgb


def cmd_render(args: argparse.Namespace) -> int:
    msh_path = Path(args.land_msh).resolve()
    map_path = Path(args.land_map).resolve() if args.land_map else None
    output_path = Path(args.output).resolve()

    positions, faces, terrain_meta = load_terrain_msh(msh_path, max_faces=int(args.max_faces))
    areals: list[dict[str, Any]] = []
    map_meta: dict[str, int] = {"areal_count": 0, "cells_x": 0, "cells_y": 0}
    if map_path:
        areals, map_meta = load_areal_map(map_path)

    rgb = _render_scene(
        positions,
        faces,
        areals,
        width=int(args.width),
        height=int(args.height),
        yaw_deg=float(args.yaw),
        pitch_deg=float(args.pitch),
        wireframe=bool(args.wireframe),
        areal_overlay=bool(args.overlay_areals),
    )
    _write_ppm(output_path, int(args.width), int(args.height), rgb)

    print(f"Rendered terrain : {msh_path}")
    if map_path:
        print(f"Areal overlay    : {map_path}")
    print(f"Output           : {output_path}")
    print(
        "Terrain geometry : "
        f"vertices={terrain_meta['vertex_count']}, "
        f"faces={terrain_meta['face_count_rendered']}/{terrain_meta['face_count_valid']} "
        f"(raw={terrain_meta['face_count_raw']}, dropped={terrain_meta['face_dropped_invalid']})"
    )
    if map_path:
        print(
            "Areal map        : "
            f"areals={map_meta['areal_count']}, cells={map_meta['cells_x']}x{map_meta['cells_y']}"
        )
    return 0


def cmd_render_batch(args: argparse.Namespace) -> int:
    maps_root = Path(args.maps_root).resolve()
    output_dir = Path(args.output_dir).resolve()
    msh_paths = sorted(maps_root.rglob("Land.msh"))
    if not msh_paths:
        raise RuntimeError(f"no Land.msh files under {maps_root}")

    rendered = 0
    skipped = 0
    for msh_path in msh_paths:
        map_path = msh_path.with_name("Land.map")
        if not map_path.exists():
            skipped += 1
            continue
        rel = msh_path.parent.relative_to(maps_root)
        out = output_dir / f"{rel.as_posix().replace('/', '__')}.ppm"
        cmd_render(
            argparse.Namespace(
                land_msh=str(msh_path),
                land_map=str(map_path),
                output=str(out),
                max_faces=args.max_faces,
                width=args.width,
                height=args.height,
                yaw=args.yaw,
                pitch=args.pitch,
                wireframe=args.wireframe,
                overlay_areals=args.overlay_areals,
            )
        )
        rendered += 1

    print(f"Batch summary: rendered={rendered}, skipped_no_map={skipped}, output_dir={output_dir}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Software 3D terrain renderer (Land.msh + optional Land.map overlay)."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    render = sub.add_parser("render", help="Render one terrain map to PPM.")
    render.add_argument("--land-msh", required=True, help="Path to Land.msh")
    render.add_argument("--land-map", help="Path to Land.map (optional)")
    render.add_argument("--output", required=True, help="Output .ppm path")
    render.add_argument("--max-faces", type=int, default=220000, help="Face limit (default: 220000)")
    render.add_argument("--width", type=int, default=1280, help="Image width (default: 1280)")
    render.add_argument("--height", type=int, default=720, help="Image height (default: 720)")
    render.add_argument("--yaw", type=float, default=38.0, help="Yaw angle in degrees (default: 38)")
    render.add_argument("--pitch", type=float, default=26.0, help="Pitch angle in degrees (default: 26)")
    render.add_argument("--wireframe", action="store_true", help="Draw terrain wireframe overlay")
    render.add_argument(
        "--overlay-areals",
        action="store_true",
        help="Draw ArealMap polygon overlay",
    )
    render.set_defaults(func=cmd_render)

    batch = sub.add_parser("render-batch", help="Render all MAPS/**/Land.msh under root.")
    batch.add_argument(
        "--maps-root",
        default="tmp/gamedata/DATA/MAPS",
        help="Root directory with MAPS subfolders (default: tmp/gamedata/DATA/MAPS)",
    )
    batch.add_argument("--output-dir", required=True, help="Directory for output PPM files")
    batch.add_argument("--max-faces", type=int, default=90000, help="Face limit per map (default: 90000)")
    batch.add_argument("--width", type=int, default=960, help="Image width (default: 960)")
    batch.add_argument("--height", type=int, default=540, help="Image height (default: 540)")
    batch.add_argument("--yaw", type=float, default=38.0, help="Yaw angle in degrees (default: 38)")
    batch.add_argument("--pitch", type=float, default=26.0, help="Pitch angle in degrees (default: 26)")
    batch.add_argument("--wireframe", action="store_true", help="Draw terrain wireframe overlay")
    batch.add_argument(
        "--overlay-areals",
        action="store_true",
        help="Draw ArealMap polygon overlay",
    )
    batch.set_defaults(func=cmd_render_batch)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())

