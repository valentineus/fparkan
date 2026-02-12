#!/usr/bin/env python3
"""
Deterministic audit for FXID "absolute parity" checklist.

What this script produces:
1) strict parsing stats across all FXID payloads in NRes archives,
2) opcode histogram and rare-branch counters (op6, op1 tail usage),
3) reference vectors for RNG core (sub_10002220 semantics).
"""

from __future__ import annotations

import argparse
import json
import struct
from collections import Counter
from pathlib import Path
from typing import Any

import archive_roundtrip_validator as arv

TYPE_FXID = 0x44495846
FX_CMD_SIZE = {1: 224, 2: 148, 3: 200, 4: 204, 5: 112, 6: 4, 7: 208, 8: 248, 9: 208, 10: 208}


def _entry_payload(blob: bytes, entry: dict[str, Any]) -> bytes:
    start = int(entry["data_offset"])
    end = start + int(entry["size"])
    return blob[start:end]


def _cstr32(raw: bytes) -> str:
    return raw.split(b"\x00", 1)[0].decode("latin1", errors="replace")


def _rng_step_sub_10002220(state32: int) -> tuple[int, int]:
    """
    sub_10002220 semantics in 32-bit packed state form:
      lo = state[15:0], hi = state[31:16]
      new_lo = hi ^ (lo << 1)
      new_hi = (hi >> 1) ^ new_lo
      return new_hi (u16), update state=(new_hi<<16)|new_lo
    """
    lo = state32 & 0xFFFF
    hi = (state32 >> 16) & 0xFFFF
    new_lo = (hi ^ ((lo << 1) & 0xFFFF)) & 0xFFFF
    new_hi = ((hi >> 1) ^ new_lo) & 0xFFFF
    return ((new_hi << 16) | new_lo), new_hi


def _rng_vectors() -> dict[str, Any]:
    seeds = [0x00000000, 0x00000001, 0x12345678, 0x89ABCDEF, 0xFFFFFFFF]
    out: list[dict[str, Any]] = []
    for seed in seeds:
        state = seed
        outputs: list[int] = []
        states: list[int] = []
        for _ in range(16):
            state, value = _rng_step_sub_10002220(state)
            outputs.append(value)
            states.append(state)
        out.append(
            {
                "seed_hex": f"0x{seed:08X}",
                "outputs_u16_hex": [f"0x{x:04X}" for x in outputs],
                "states_u32_hex": [f"0x{x:08X}" for x in states],
            }
        )
    return {"generator": "sub_10002220", "vectors": out}


def run_audit(root: Path) -> dict[str, Any]:
    counters: Counter[str] = Counter()
    opcode_hist: Counter[int] = Counter()
    issues: list[dict[str, Any]] = []
    op1_tail6_samples: list[dict[str, Any]] = []
    op1_optref_samples: list[dict[str, Any]] = []

    for item in arv.scan_archives(root):
        if item["type"] != "nres":
            continue
        archive_path = root / item["relative_path"]
        counters["archives_total"] += 1
        data = archive_path.read_bytes()
        try:
            parsed = arv.parse_nres(data, source=str(archive_path))
        except Exception as exc:  # pylint: disable=broad-except
            issues.append(
                {
                    "severity": "error",
                    "archive": str(archive_path),
                    "entry": None,
                    "message": f"cannot parse NRes: {exc}",
                }
            )
            continue

        for entry in parsed["entries"]:
            if int(entry["type_id"]) != TYPE_FXID:
                continue
            counters["fxid_total"] += 1
            payload = _entry_payload(data, entry)
            entry_name = str(entry["name"])

            if len(payload) < 60:
                issues.append(
                    {
                        "severity": "error",
                        "archive": str(archive_path),
                        "entry": entry_name,
                        "message": f"payload too small: {len(payload)}",
                    }
                )
                continue

            cmd_count = struct.unpack_from("<I", payload, 0)[0]
            ptr = 0x3C
            ok = True
            for idx in range(cmd_count):
                if ptr + 4 > len(payload):
                    issues.append(
                        {
                            "severity": "error",
                            "archive": str(archive_path),
                            "entry": entry_name,
                            "message": f"command {idx}: missing header at offset={ptr}",
                        }
                    )
                    ok = False
                    break

                word = struct.unpack_from("<I", payload, ptr)[0]
                opcode = word & 0xFF
                size = FX_CMD_SIZE.get(opcode)
                if size is None:
                    issues.append(
                        {
                            "severity": "error",
                            "archive": str(archive_path),
                            "entry": entry_name,
                            "message": f"command {idx}: unknown opcode={opcode} at offset={ptr}",
                        }
                    )
                    ok = False
                    break

                if ptr + size > len(payload):
                    issues.append(
                        {
                            "severity": "error",
                            "archive": str(archive_path),
                            "entry": entry_name,
                            "message": f"command {idx}: truncated end={ptr + size}, payload={len(payload)}",
                        }
                    )
                    ok = False
                    break

                opcode_hist[opcode] += 1
                if opcode == 6:
                    counters["op6_commands"] += 1
                if opcode == 1:
                    tail6 = payload[ptr + 136 : ptr + 160]
                    if any(tail6):
                        counters["op1_tail6_nonzero"] += 1
                        if len(op1_tail6_samples) < 16:
                            dwords = list(struct.unpack("<6I", tail6))
                            op1_tail6_samples.append(
                                {
                                    "archive": str(archive_path),
                                    "entry": entry_name,
                                    "cmd_index": idx,
                                    "tail6_u32_hex": [f"0x{x:08X}" for x in dwords],
                                }
                            )

                    archive_s = _cstr32(payload[ptr + 160 : ptr + 192])
                    name_s = _cstr32(payload[ptr + 192 : ptr + 224])
                    if archive_s or name_s:
                        counters["op1_optref_nonempty"] += 1
                        if len(op1_optref_samples) < 16:
                            op1_optref_samples.append(
                                {
                                    "archive": str(archive_path),
                                    "entry": entry_name,
                                    "cmd_index": idx,
                                    "opt_archive": archive_s,
                                    "opt_name": name_s,
                                }
                            )

                ptr += size

            if ok and ptr != len(payload):
                issues.append(
                    {
                        "severity": "error",
                        "archive": str(archive_path),
                        "entry": entry_name,
                        "message": f"tail bytes after command stream: parsed_end={ptr}, payload={len(payload)}",
                    }
                )
                ok = False

            if ok:
                counters["fxid_ok"] += 1

    return {
        "input_root": str(root),
        "summary": {
            "archives_total": counters["archives_total"],
            "fxid_total": counters["fxid_total"],
            "fxid_ok": counters["fxid_ok"],
            "issues_total": len(issues),
            "op6_commands": counters["op6_commands"],
            "op1_tail6_nonzero": counters["op1_tail6_nonzero"],
            "op1_optref_nonempty": counters["op1_optref_nonempty"],
        },
        "opcode_histogram": {str(k): opcode_hist[k] for k in sorted(opcode_hist)},
        "op1_tail6_samples": op1_tail6_samples,
        "op1_optref_samples": op1_optref_samples,
        "rng_reference": _rng_vectors(),
        "rng_states_fx_path": [
            {"state": "dword_10023688", "seed_init": "sub_10002660", "used_by": ["sub_10001720", "sub_10001A40"]},
            {"state": "dword_100238C0", "seed_init": "sub_10003A50", "used_by": ["sub_10002BE0"]},
            {"state": "dword_10024110", "seed_init": "sub_10009180", "used_by": ["sub_10008120", "sub_10007D10"]},
            {"state": "dword_10024810", "seed_init": "sub_1000D370", "used_by": ["sub_1000BF30", "sub_1000C1A0"]},
            {"state": "dword_10024A48", "seed_init": "sub_1000F420", "used_by": ["sub_1000EC50"]},
            {"state": "dword_10024C80", "seed_init": "sub_10010370", "used_by": ["sub_1000F6E0"]},
            {"state": "dword_100250F0", "seed_init": "sub_10012C70", "used_by": ["sub_10011230", "sub_100115C0"]},
        ],
        "issues": issues,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="FXID absolute parity audit.")
    parser.add_argument("--input", required=True, help="Root directory with game/test archives.")
    parser.add_argument("--report", required=True, help="Output JSON report path.")
    args = parser.parse_args()

    root = Path(args.input).resolve()
    report_path = Path(args.report).resolve()
    payload = run_audit(root)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    summary = payload["summary"]
    print(f"Input root           : {root}")
    print(f"NRes archives        : {summary['archives_total']}")
    print(f"FXID payloads        : {summary['fxid_ok']}/{summary['fxid_total']} valid")
    print(f"Issues               : {summary['issues_total']}")
    print(f"Opcode6 commands     : {summary['op6_commands']}")
    print(f"Op1 tail6 nonzero    : {summary['op1_tail6_nonzero']}")
    print(f"Op1 optref non-empty : {summary['op1_optref_nonempty']}")
    print(f"Report               : {report_path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
