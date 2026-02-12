#!/usr/bin/env python3
"""
Validate terrain/map documentation assumptions against real game data.

Targets:
- tmp/gamedata/DATA/MAPS/**/Land.msh
- tmp/gamedata/DATA/MAPS/**/Land.map
"""

from __future__ import annotations

import argparse
import json
import math
import struct
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import archive_roundtrip_validator as arv

MAGIC_NRES = b"NRes"

REQUIRED_MSH_TYPES = (1, 2, 3, 4, 5, 11, 18, 21)
OPTIONAL_MSH_TYPES = (14,)
EXPECTED_MSH_ORDER = (1, 2, 3, 4, 5, 18, 14, 11, 21)

MSH_STRIDES = {
    1: 38,
    3: 12,
    4: 4,
    5: 4,
    11: 4,
    14: 4,
    18: 4,
    21: 28,
}

SLOT_TABLE_OFFSET = 0x8C


@dataclass
class ValidationIssue:
    severity: str  # error | warning
    category: str
    resource: str
    message: str


class TerrainMapDocValidator:
    def __init__(self) -> None:
        self.issues: list[ValidationIssue] = []
        self.stats: dict[str, Any] = {
            "maps_total": 0,
            "msh_total": 0,
            "map_total": 0,
            "msh_type_orders": Counter(),
            "msh_attr_triplets": defaultdict(Counter),  # type_id -> Counter[(a1,a2,a3)]
            "msh_type11_header_words": Counter(),
            "msh_type21_flags_top": Counter(),
            "map_logic_flags": Counter(),
            "map_class_ids": Counter(),  # record +40
            "map_poly_count": Counter(),
            "map_vertex_count_min": None,
            "map_vertex_count_max": None,
            "map_cell_dims": Counter(),
            "map_reserved_u12": Counter(),
            "map_reserved_u36": Counter(),
            "map_reserved_u44": Counter(),
            "map_area_delta_abs_max": 0.0,
            "map_area_delta_rel_max": 0.0,
            "map_area_rel_gt_05_count": 0,
            "map_normal_len_min": None,
            "map_normal_len_max": None,
            "map_records_total": 0,
        }

    def add_issue(self, severity: str, category: str, resource: Path, message: str) -> None:
        self.issues.append(
            ValidationIssue(
                severity=severity,
                category=category,
                resource=str(resource),
                message=message,
            )
        )

    def _entry_payload(self, blob: bytes, entry: dict[str, Any]) -> bytes:
        start = int(entry["data_offset"])
        end = start + int(entry["size"])
        return blob[start:end]

    def _entry_by_type(self, entries: list[dict[str, Any]]) -> dict[int, list[dict[str, Any]]]:
        by_type: dict[int, list[dict[str, Any]]] = {}
        for item in entries:
            by_type.setdefault(int(item["type_id"]), []).append(item)
        return by_type

    def _expect_single_type(
        self,
        *,
        by_type: dict[int, list[dict[str, Any]]],
        type_id: int,
        label: str,
        resource: Path,
        required: bool,
    ) -> dict[str, Any] | None:
        rows = by_type.get(type_id, [])
        if not rows:
            if required:
                self.add_issue(
                    "error",
                    "msh-chunk",
                    resource,
                    f"missing required chunk type={type_id} ({label})",
                )
            return None
        if len(rows) > 1:
            self.add_issue(
                "warning",
                "msh-chunk",
                resource,
                f"multiple chunks type={type_id} ({label}); using first",
            )
        return rows[0]

    def _check_stride(
        self,
        *,
        resource: Path,
        entry: dict[str, Any],
        stride: int,
        label: str,
    ) -> int:
        size = int(entry["size"])
        attr1 = int(entry["attr1"])
        attr2 = int(entry["attr2"])
        attr3 = int(entry["attr3"])
        self.stats["msh_attr_triplets"][int(entry["type_id"])][(attr1, attr2, attr3)] += 1

        if size % stride != 0:
            self.add_issue(
                "error",
                "msh-stride",
                resource,
                f"{label}: size={size} is not divisible by stride={stride}",
            )
            return -1

        count = size // stride
        if attr1 != count:
            self.add_issue(
                "error",
                "msh-attr",
                resource,
                f"{label}: attr1={attr1} != size/stride={count}",
            )
        if attr3 != stride:
            self.add_issue(
                "error",
                "msh-attr",
                resource,
                f"{label}: attr3={attr3} != {stride}",
            )
        if attr2 != 0 and int(entry["type_id"]) not in (1,):
            # type 1 has non-zero attr2 in real assets, others are expected zero.
            self.add_issue(
                "warning",
                "msh-attr",
                resource,
                f"{label}: attr2={attr2} (expected 0 for this chunk type)",
            )
        return count

    def validate_msh(self, path: Path) -> None:
        self.stats["msh_total"] += 1
        blob = path.read_bytes()
        if blob[:4] != MAGIC_NRES:
            self.add_issue("error", "msh-container", path, "file is not NRes")
            return

        try:
            parsed = arv.parse_nres(blob, source=str(path))
        except Exception as exc:  # pylint: disable=broad-except
            self.add_issue("error", "msh-container", path, f"failed to parse NRes: {exc}")
            return

        for issue in parsed.get("issues", []):
            self.add_issue("warning", "msh-nres", path, issue)

        entries = parsed["entries"]
        types_order = tuple(int(item["type_id"]) for item in entries)
        self.stats["msh_type_orders"][types_order] += 1
        if types_order != EXPECTED_MSH_ORDER:
            self.add_issue(
                "warning",
                "msh-order",
                path,
                f"unexpected chunk order {types_order}, expected {EXPECTED_MSH_ORDER}",
            )

        by_type = self._entry_by_type(entries)

        chunks: dict[int, dict[str, Any]] = {}
        for type_id in REQUIRED_MSH_TYPES:
            chunk = self._expect_single_type(
                by_type=by_type,
                type_id=type_id,
                label=f"type{type_id}",
                resource=path,
                required=True,
            )
            if chunk:
                chunks[type_id] = chunk
        for type_id in OPTIONAL_MSH_TYPES:
            chunk = self._expect_single_type(
                by_type=by_type,
                type_id=type_id,
                label=f"type{type_id}",
                resource=path,
                required=False,
            )
            if chunk:
                chunks[type_id] = chunk

        for type_id, stride in MSH_STRIDES.items():
            chunk = chunks.get(type_id)
            if not chunk:
                continue
            self._check_stride(resource=path, entry=chunk, stride=stride, label=f"type{type_id}")

        # type 2 includes 0x8C-byte header + 68-byte slot table entries.
        type2 = chunks.get(2)
        if type2:
            size = int(type2["size"])
            attr1 = int(type2["attr1"])
            attr2 = int(type2["attr2"])
            attr3 = int(type2["attr3"])
            self.stats["msh_attr_triplets"][2][(attr1, attr2, attr3)] += 1
            if attr3 != 68:
                self.add_issue(
                    "error",
                    "msh-attr",
                    path,
                    f"type2: attr3={attr3} != 68",
                )
            if attr2 != 0:
                self.add_issue(
                    "warning",
                    "msh-attr",
                    path,
                    f"type2: attr2={attr2} (expected 0)",
                )
            if size < SLOT_TABLE_OFFSET:
                self.add_issue(
                    "error",
                    "msh-size",
                    path,
                    f"type2: size={size} < header_size={SLOT_TABLE_OFFSET}",
                )
            elif (size - SLOT_TABLE_OFFSET) % 68 != 0:
                self.add_issue(
                    "error",
                    "msh-size",
                    path,
                    f"type2: (size - 0x8C) is not divisible by 68 (size={size})",
                )
            else:
                slots_by_size = (size - SLOT_TABLE_OFFSET) // 68
                if attr1 != slots_by_size:
                    self.add_issue(
                        "error",
                        "msh-attr",
                        path,
                        f"type2: attr1={attr1} != (size-0x8C)/68={slots_by_size}",
                    )

        verts = chunks.get(3)
        face = chunks.get(21)
        slots = chunks.get(2)
        nodes = chunks.get(1)
        type11 = chunks.get(11)

        if verts and face:
            vcount = int(verts["attr1"])
            face_payload = self._entry_payload(blob, face)
            fcount = int(face["attr1"])
            if len(face_payload) >= 28:
                for idx in range(fcount):
                    off = idx * 28
                    if off + 28 > len(face_payload):
                        self.add_issue(
                            "error",
                            "msh-face",
                            path,
                            f"type21 truncated at face {idx}",
                        )
                        break
                    flags = struct.unpack_from("<I", face_payload, off)[0]
                    self.stats["msh_type21_flags_top"][flags] += 1
                    i0, i1, i2 = struct.unpack_from("<HHH", face_payload, off + 8)
                    for name, value in (("i0", i0), ("i1", i1), ("i2", i2)):
                        if value >= vcount:
                            self.add_issue(
                                "error",
                                "msh-face-index",
                                path,
                                f"type21[{idx}].{name}={value} out of range vertex_count={vcount}",
                            )
                    n0, n1, n2 = struct.unpack_from("<HHH", face_payload, off + 14)
                    for name, value in (("n0", n0), ("n1", n1), ("n2", n2)):
                        if value != 0xFFFF and value >= fcount:
                            self.add_issue(
                                "error",
                                "msh-face-neighbour",
                                path,
                                f"type21[{idx}].{name}={value} out of range face_count={fcount}",
                            )

        if slots and face:
            slot_count = int(slots["attr1"])
            face_count = int(face["attr1"])
            slot_payload = self._entry_payload(blob, slots)
            need = SLOT_TABLE_OFFSET + slot_count * 68
            if len(slot_payload) < need:
                self.add_issue(
                    "error",
                    "msh-slot",
                    path,
                    f"type2 payload too short: size={len(slot_payload)}, need_at_least={need}",
                )
            else:
                if len(slot_payload) != need:
                    self.add_issue(
                        "warning",
                        "msh-slot",
                        path,
                        f"type2 payload has trailing bytes: size={len(slot_payload)}, expected={need}",
                    )
                for idx in range(slot_count):
                    off = SLOT_TABLE_OFFSET + idx * 68
                    tri_start, tri_count = struct.unpack_from("<HH", slot_payload, off)
                    if tri_start + tri_count > face_count:
                        self.add_issue(
                            "error",
                            "msh-slot-range",
                            path,
                            f"type2 slot[{idx}] range [{tri_start}, {tri_start + tri_count}) exceeds face_count={face_count}",
                        )

        if nodes and slots:
            node_payload = self._entry_payload(blob, nodes)
            slot_count = int(slots["attr1"])
            node_count = int(nodes["attr1"])
            for node_idx in range(node_count):
                off = node_idx * 38
                if off + 38 > len(node_payload):
                    self.add_issue(
                        "error",
                        "msh-node",
                        path,
                        f"type1 truncated at node {node_idx}",
                    )
                    break
                for j in range(19):
                    slot_id = struct.unpack_from("<H", node_payload, off + j * 2)[0]
                    if slot_id != 0xFFFF and slot_id >= slot_count:
                        self.add_issue(
                            "error",
                            "msh-node-slot",
                            path,
                            f"type1 node[{node_idx}] slot[{j}]={slot_id} out of range slot_count={slot_count}",
                        )

        if type11:
            payload = self._entry_payload(blob, type11)
            if len(payload) >= 8:
                w0, w1 = struct.unpack_from("<II", payload, 0)
                self.stats["msh_type11_header_words"][(w0, w1)] += 1
            else:
                self.add_issue(
                    "error",
                    "msh-type11",
                    path,
                    f"type11 payload too short: {len(payload)}",
                )

    def _update_minmax(self, key_min: str, key_max: str, value: float) -> None:
        if self.stats[key_min] is None or value < self.stats[key_min]:
            self.stats[key_min] = value
        if self.stats[key_max] is None or value > self.stats[key_max]:
            self.stats[key_max] = value

    def validate_map(self, path: Path) -> None:
        self.stats["map_total"] += 1
        blob = path.read_bytes()
        if blob[:4] != MAGIC_NRES:
            self.add_issue("error", "map-container", path, "file is not NRes")
            return

        try:
            parsed = arv.parse_nres(blob, source=str(path))
        except Exception as exc:  # pylint: disable=broad-except
            self.add_issue("error", "map-container", path, f"failed to parse NRes: {exc}")
            return

        for issue in parsed.get("issues", []):
            self.add_issue("warning", "map-nres", path, issue)

        entries = parsed["entries"]
        if len(entries) != 1 or int(entries[0]["type_id"]) != 12:
            self.add_issue(
                "error",
                "map-chunk",
                path,
                f"expected single chunk type=12, got {[int(e['type_id']) for e in entries]}",
            )
            return

        entry = entries[0]
        areal_count = int(entry["attr1"])
        if areal_count <= 0:
            self.add_issue("error", "map-areal", path, f"invalid areal_count={areal_count}")
            return

        payload = self._entry_payload(blob, entry)
        ptr = 0
        records: list[dict[str, Any]] = []

        for idx in range(areal_count):
            if ptr + 56 > len(payload):
                self.add_issue(
                    "error",
                    "map-record",
                    path,
                    f"truncated areal header at index={idx}, ptr={ptr}, size={len(payload)}",
                )
                return

            anchor_x, anchor_y, anchor_z = struct.unpack_from("<fff", payload, ptr)
            u12 = struct.unpack_from("<I", payload, ptr + 12)[0]
            area_f = struct.unpack_from("<f", payload, ptr + 16)[0]
            nx, ny, nz = struct.unpack_from("<fff", payload, ptr + 20)
            logic_flag = struct.unpack_from("<I", payload, ptr + 32)[0]
            u36 = struct.unpack_from("<I", payload, ptr + 36)[0]
            class_id = struct.unpack_from("<I", payload, ptr + 40)[0]
            u44 = struct.unpack_from("<I", payload, ptr + 44)[0]
            vertex_count, poly_count = struct.unpack_from("<II", payload, ptr + 48)

            self.stats["map_records_total"] += 1
            self.stats["map_logic_flags"][logic_flag] += 1
            self.stats["map_class_ids"][class_id] += 1
            self.stats["map_poly_count"][poly_count] += 1
            self.stats["map_reserved_u12"][u12] += 1
            self.stats["map_reserved_u36"][u36] += 1
            self.stats["map_reserved_u44"][u44] += 1
            self._update_minmax("map_vertex_count_min", "map_vertex_count_max", float(vertex_count))

            normal_len = math.sqrt(nx * nx + ny * ny + nz * nz)
            self._update_minmax("map_normal_len_min", "map_normal_len_max", normal_len)
            if abs(normal_len - 1.0) > 1e-3:
                self.add_issue(
                    "warning",
                    "map-normal",
                    path,
                    f"record[{idx}] normal length={normal_len:.6f} (expected ~1.0)",
                )

            vertices_off = ptr + 56
            vertices_size = 12 * vertex_count
            if vertices_off + vertices_size > len(payload):
                self.add_issue(
                    "error",
                    "map-vertices",
                    path,
                    f"record[{idx}] vertices out of bounds",
                )
                return

            vertices: list[tuple[float, float, float]] = []
            for i in range(vertex_count):
                vertices.append(struct.unpack_from("<fff", payload, vertices_off + i * 12))

            if vertex_count >= 3:
                # signed shoelace area in XY.
                shoelace = 0.0
                for i in range(vertex_count):
                    x1, y1, _ = vertices[i]
                    x2, y2, _ = vertices[(i + 1) % vertex_count]
                    shoelace += x1 * y2 - x2 * y1
                area_xy = abs(shoelace) * 0.5
                delta = abs(area_xy - area_f)
                if delta > self.stats["map_area_delta_abs_max"]:
                    self.stats["map_area_delta_abs_max"] = delta
                rel_delta = delta / max(1.0, area_xy)
                if rel_delta > self.stats["map_area_delta_rel_max"]:
                    self.stats["map_area_delta_rel_max"] = rel_delta
                if rel_delta > 0.05:
                    self.stats["map_area_rel_gt_05_count"] += 1

            links_off = vertices_off + vertices_size
            link_count = vertex_count + 3 * poly_count
            links_size = 8 * link_count
            if links_off + links_size > len(payload):
                self.add_issue(
                    "error",
                    "map-links",
                    path,
                    f"record[{idx}] link table out of bounds",
                )
                return

            edge_links: list[tuple[int, int]] = []
            for i in range(vertex_count):
                area_ref, edge_ref = struct.unpack_from("<ii", payload, links_off + i * 8)
                edge_links.append((area_ref, edge_ref))

            poly_links_off = links_off + 8 * vertex_count
            poly_links: list[tuple[int, int]] = []
            for i in range(3 * poly_count):
                area_ref, edge_ref = struct.unpack_from("<ii", payload, poly_links_off + i * 8)
                poly_links.append((area_ref, edge_ref))

            p = links_off + links_size
            for poly_idx in range(poly_count):
                if p + 4 > len(payload):
                    self.add_issue(
                        "error",
                        "map-poly",
                        path,
                        f"record[{idx}] poly header truncated at poly_idx={poly_idx}",
                    )
                    return
                n = struct.unpack_from("<I", payload, p)[0]
                poly_size = 4 * (3 * n + 1)
                if p + poly_size > len(payload):
                    self.add_issue(
                        "error",
                        "map-poly",
                        path,
                        f"record[{idx}] poly data out of bounds at poly_idx={poly_idx}",
                    )
                    return
                p += poly_size

            records.append(
                {
                    "index": idx,
                    "anchor": (anchor_x, anchor_y, anchor_z),
                    "logic": logic_flag,
                    "class_id": class_id,
                    "vertex_count": vertex_count,
                    "poly_count": poly_count,
                    "edge_links": edge_links,
                    "poly_links": poly_links,
                }
            )
            ptr = p

        vertex_counts = [int(item["vertex_count"]) for item in records]
        for rec in records:
            idx = int(rec["index"])
            for link_idx, (area_ref, edge_ref) in enumerate(rec["edge_links"]):
                if area_ref == -1:
                    if edge_ref != -1:
                        self.add_issue(
                            "warning",
                            "map-link",
                            path,
                            f"record[{idx}] edge_link[{link_idx}] has area_ref=-1 but edge_ref={edge_ref}",
                        )
                    continue
                if area_ref < 0 or area_ref >= areal_count:
                    self.add_issue(
                        "error",
                        "map-link",
                        path,
                        f"record[{idx}] edge_link[{link_idx}] area_ref={area_ref} out of range",
                    )
                    continue
                dst_vcount = vertex_counts[area_ref]
                if edge_ref < 0 or edge_ref >= dst_vcount:
                    self.add_issue(
                        "error",
                        "map-link",
                        path,
                        f"record[{idx}] edge_link[{link_idx}] edge_ref={edge_ref} out of range dst_vertex_count={dst_vcount}",
                    )

            for link_idx, (area_ref, edge_ref) in enumerate(rec["poly_links"]):
                if area_ref == -1:
                    if edge_ref != -1:
                        self.add_issue(
                            "warning",
                            "map-poly-link",
                            path,
                            f"record[{idx}] poly_link[{link_idx}] has area_ref=-1 but edge_ref={edge_ref}",
                        )
                    continue
                if area_ref < 0 or area_ref >= areal_count:
                    self.add_issue(
                        "error",
                        "map-poly-link",
                        path,
                        f"record[{idx}] poly_link[{link_idx}] area_ref={area_ref} out of range",
                    )

        if ptr + 8 > len(payload):
            self.add_issue(
                "error",
                "map-cells",
                path,
                f"missing cells header at ptr={ptr}, size={len(payload)}",
            )
            return

        cells_x, cells_y = struct.unpack_from("<II", payload, ptr)
        self.stats["map_cell_dims"][(cells_x, cells_y)] += 1
        ptr += 8
        if cells_x <= 0 or cells_y <= 0:
            self.add_issue(
                "error",
                "map-cells",
                path,
                f"invalid cells dimensions {cells_x}x{cells_y}",
            )
            return

        for x in range(cells_x):
            for y in range(cells_y):
                if ptr + 2 > len(payload):
                    self.add_issue(
                        "error",
                        "map-cells",
                        path,
                        f"truncated hitCount at cell ({x},{y})",
                    )
                    return
                hit_count = struct.unpack_from("<H", payload, ptr)[0]
                ptr += 2
                need = 2 * hit_count
                if ptr + need > len(payload):
                    self.add_issue(
                        "error",
                        "map-cells",
                        path,
                        f"truncated areaIds at cell ({x},{y}), hitCount={hit_count}",
                    )
                    return
                for i in range(hit_count):
                    area_id = struct.unpack_from("<H", payload, ptr + 2 * i)[0]
                    if area_id >= areal_count:
                        self.add_issue(
                            "error",
                            "map-cells",
                            path,
                            f"cell ({x},{y}) has area_id={area_id} out of range areal_count={areal_count}",
                        )
                ptr += need

        if ptr != len(payload):
            self.add_issue(
                "error",
                "map-size",
                path,
                f"payload tail mismatch: consumed={ptr}, payload_size={len(payload)}",
            )

    def validate(self, maps_root: Path) -> None:
        msh_paths = sorted(maps_root.rglob("Land.msh"))
        map_paths = sorted(maps_root.rglob("Land.map"))

        msh_by_dir = {path.parent: path for path in msh_paths}
        map_by_dir = {path.parent: path for path in map_paths}

        all_dirs = sorted(set(msh_by_dir) | set(map_by_dir))
        self.stats["maps_total"] = len(all_dirs)

        for folder in all_dirs:
            msh_path = msh_by_dir.get(folder)
            map_path = map_by_dir.get(folder)
            if msh_path is None:
                self.add_issue("error", "pairing", folder, "missing Land.msh")
                continue
            if map_path is None:
                self.add_issue("error", "pairing", folder, "missing Land.map")
                continue
            self.validate_msh(msh_path)
            self.validate_map(map_path)

    def build_report(self) -> dict[str, Any]:
        errors = [i for i in self.issues if i.severity == "error"]
        warnings = [i for i in self.issues if i.severity == "warning"]

        # Convert counters/defaultdicts to JSON-friendly dicts.
        msh_orders = {
            str(list(order)): count
            for order, count in self.stats["msh_type_orders"].most_common()
        }
        msh_attrs = {
            str(type_id): {str(list(k)): v for k, v in counter.most_common()}
            for type_id, counter in self.stats["msh_attr_triplets"].items()
        }
        type11_hdr = {
            str(list(key)): value
            for key, value in self.stats["msh_type11_header_words"].most_common()
        }
        type21_flags = {
            f"0x{key:08X}": value
            for key, value in self.stats["msh_type21_flags_top"].most_common(32)
        }

        return {
            "summary": {
                "maps_total": self.stats["maps_total"],
                "msh_total": self.stats["msh_total"],
                "map_total": self.stats["map_total"],
                "issues_total": len(self.issues),
                "errors_total": len(errors),
                "warnings_total": len(warnings),
            },
            "stats": {
                "msh_type_orders": msh_orders,
                "msh_attr_triplets": msh_attrs,
                "msh_type11_header_words": type11_hdr,
                "msh_type21_flags_top": type21_flags,
                "map_logic_flags": dict(self.stats["map_logic_flags"]),
                "map_class_ids": dict(self.stats["map_class_ids"]),
                "map_poly_count": dict(self.stats["map_poly_count"]),
                "map_vertex_count_min": self.stats["map_vertex_count_min"],
                "map_vertex_count_max": self.stats["map_vertex_count_max"],
                "map_cell_dims": {str(list(k)): v for k, v in self.stats["map_cell_dims"].items()},
                "map_reserved_u12": dict(self.stats["map_reserved_u12"]),
                "map_reserved_u36": dict(self.stats["map_reserved_u36"]),
                "map_reserved_u44": dict(self.stats["map_reserved_u44"]),
                "map_area_delta_abs_max": self.stats["map_area_delta_abs_max"],
                "map_area_delta_rel_max": self.stats["map_area_delta_rel_max"],
                "map_area_rel_gt_05_count": self.stats["map_area_rel_gt_05_count"],
                "map_normal_len_min": self.stats["map_normal_len_min"],
                "map_normal_len_max": self.stats["map_normal_len_max"],
                "map_records_total": self.stats["map_records_total"],
            },
            "issues": [
                {
                    "severity": item.severity,
                    "category": item.category,
                    "resource": item.resource,
                    "message": item.message,
                }
                for item in self.issues
            ],
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Validate terrain/map doc assumptions")
    parser.add_argument(
        "--maps-root",
        type=Path,
        default=Path("tmp/gamedata/DATA/MAPS"),
        help="Root directory containing MAPS/**/Land.msh and Land.map",
    )
    parser.add_argument(
        "--report-json",
        type=Path,
        default=None,
        help="Optional path to save full JSON report",
    )
    parser.add_argument(
        "--fail-on-warning",
        action="store_true",
        help="Return non-zero exit code on warnings too",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    validator = TerrainMapDocValidator()
    validator.validate(args.maps_root)
    report = validator.build_report()

    print(
        json.dumps(
            report["summary"],
            indent=2,
            ensure_ascii=False,
        )
    )

    if args.report_json:
        args.report_json.parent.mkdir(parents=True, exist_ok=True)
        with args.report_json.open("w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, ensure_ascii=False)
            handle.write("\n")
        print(f"report written: {args.report_json}")

    has_errors = report["summary"]["errors_total"] > 0
    has_warnings = report["summary"]["warnings_total"] > 0
    if has_errors:
        return 1
    if args.fail_on_warning and has_warnings:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
