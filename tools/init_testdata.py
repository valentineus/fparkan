#!/usr/bin/env python3
"""
Initialize test data folders by archive signatures.

The script scans all files in --input and copies matching archives into:
  --output/nres/<relative path>
  --output/rsli/<relative path>
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

MAGIC_NRES = b"NRes"
MAGIC_RSLI = b"NL\x00\x01"


def is_relative_to(path: Path, base: Path) -> bool:
    try:
        path.relative_to(base)
    except ValueError:
        return False
    return True


def detect_archive_type(path: Path) -> str | None:
    try:
        with path.open("rb") as handle:
            magic = handle.read(4)
    except OSError as exc:
        print(f"[warn] cannot read {path}: {exc}", file=sys.stderr)
        return None

    if magic == MAGIC_NRES:
        return "nres"
    if magic == MAGIC_RSLI:
        return "rsli"
    return None


def scan_archives(input_root: Path, excluded_root: Path | None) -> list[tuple[Path, str]]:
    found: list[tuple[Path, str]] = []
    for path in sorted(input_root.rglob("*")):
        if not path.is_file():
            continue
        if excluded_root and is_relative_to(path.resolve(), excluded_root):
            continue

        archive_type = detect_archive_type(path)
        if archive_type:
            found.append((path, archive_type))
    return found


def confirm_overwrite(path: Path) -> str:
    prompt = (
        f"File exists: {path}\n"
        "Overwrite? [y]es / [n]o / [a]ll / [q]uit (default: n): "
    )
    while True:
        try:
            answer = input(prompt).strip().lower()
        except EOFError:
            return "quit"

        if answer in {"", "n", "no"}:
            return "no"
        if answer in {"y", "yes"}:
            return "yes"
        if answer in {"a", "all"}:
            return "all"
        if answer in {"q", "quit"}:
            return "quit"
        print("Please answer with y, n, a, or q.")


def copy_archives(
    archives: list[tuple[Path, str]],
    input_root: Path,
    output_root: Path,
    force: bool,
) -> int:
    copied = 0
    skipped = 0
    overwritten = 0
    overwrite_all = force

    type_counts = {"nres": 0, "rsli": 0}
    for _, archive_type in archives:
        type_counts[archive_type] += 1

    print(
        f"Found archives: total={len(archives)}, "
        f"nres={type_counts['nres']}, rsli={type_counts['rsli']}"
    )

    for source, archive_type in archives:
        rel_path = source.relative_to(input_root)
        destination = output_root / archive_type / rel_path
        destination.parent.mkdir(parents=True, exist_ok=True)

        if destination.exists():
            if destination.is_dir():
                print(
                    f"[error] destination is a directory, expected file: {destination}",
                    file=sys.stderr,
                )
                return 2

            if not overwrite_all:
                if not sys.stdin.isatty():
                    print(
                        "[error] destination file exists but stdin is not interactive. "
                        "Use --force to overwrite without prompts.",
                        file=sys.stderr,
                    )
                    return 2

                decision = confirm_overwrite(destination)
                if decision == "quit":
                    print("Aborted by user.")
                    return 130
                if decision == "no":
                    skipped += 1
                    continue
                if decision == "all":
                    overwrite_all = True

            overwritten += 1

        try:
            shutil.copy2(source, destination)
        except OSError as exc:
            print(f"[error] failed to copy {source} -> {destination}: {exc}", file=sys.stderr)
            return 2
        copied += 1

    print(
        f"Done: copied={copied}, overwritten={overwritten}, skipped={skipped}, "
        f"output={output_root}"
    )
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Initialize test data by scanning NRes/RsLi signatures."
    )
    parser.add_argument(
        "--input",
        required=True,
        help="Input directory to scan recursively.",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Output root directory (archives go to nres/ and rsli/ subdirs).",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite destination files without confirmation prompts.",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()

    input_root = Path(args.input)
    if not input_root.exists():
        print(f"[error] input directory does not exist: {input_root}", file=sys.stderr)
        return 2
    if not input_root.is_dir():
        print(f"[error] input path is not a directory: {input_root}", file=sys.stderr)
        return 2

    output_root = Path(args.output)
    if output_root.exists() and not output_root.is_dir():
        print(f"[error] output path exists and is not a directory: {output_root}", file=sys.stderr)
        return 2

    input_resolved = input_root.resolve()
    output_resolved = output_root.resolve()
    if input_resolved == output_resolved:
        print("[error] input and output directories must be different.", file=sys.stderr)
        return 2

    excluded_root: Path | None = None
    if is_relative_to(output_resolved, input_resolved):
        excluded_root = output_resolved
        print(f"Notice: output is inside input, skipping scan under: {excluded_root}")

    archives = scan_archives(input_root, excluded_root)

    output_root.mkdir(parents=True, exist_ok=True)
    return copy_archives(archives, input_root, output_root, force=args.force)


if __name__ == "__main__":
    raise SystemExit(main())
