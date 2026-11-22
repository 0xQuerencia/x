#!/usr/bin/env python3
"""Create or verify unsigned X preview provenance without external dependencies."""

import argparse
import hashlib
import json
from pathlib import Path
import re

SHA = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,120}")


def artifact(root: Path, name: str) -> dict:
    if not NAME.fullmatch(name) or name in {".", ".."}:
        raise ValueError("Artifact name must be an ordinary filename")
    path = root / name
    if path.is_symlink() or not path.is_file() or path.resolve().parent != root.resolve():
        raise ValueError("Artifact must be a regular file inside the selected directory")
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return {"name": name, "bytes": path.stat().st_size, "sha256": digest.hexdigest()}


def create(args: argparse.Namespace) -> None:
    if not SHA.fullmatch(args.source_sha):
        raise ValueError("Source SHA must be an exact 40- or 64-character lowercase commit")
    if not args.artifact or len(args.artifact) > 24:
        raise ValueError("Select between 1 and 24 build artifacts")
    if len(args.compiler) > 150 or any(ord(ch) < 32 for ch in args.compiler):
        raise ValueError("Compiler identity is invalid")
    root = Path(args.directory)
    entries = [artifact(root, name) for name in args.artifact]
    if len({entry["name"] for entry in entries}) != len(entries):
        raise ValueError("Duplicate artifact names are not allowed")
    manifest = {
        "schema": 1,
        "build": "unsigned-preview",
        "source_sha": args.source_sha,
        "platform": args.platform,
        "target": args.target,
        "compiler": args.compiler,
        "signed": False,
        "artifacts": sorted(entries, key=lambda item: item["name"]),
    }
    target = root / "BUILD_INFO.json"
    if target.is_symlink():
        raise ValueError("Refusing a linked provenance destination")
    target.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def verify(args: argparse.Namespace) -> None:
    root = Path(args.directory)
    path = root / "BUILD_INFO.json"
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 64 * 1024:
        raise ValueError("Missing or unsafe preview manifest")
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema", "build", "source_sha", "platform", "target", "compiler", "signed", "artifacts"
    }:
        raise ValueError("Unexpected preview manifest schema")
    if (manifest["schema"] != 1 or manifest["build"] != "unsigned-preview"
            or manifest["signed"] is not False or manifest["source_sha"] != args.source_sha
            or manifest["platform"] != args.platform):
        raise ValueError("Preview provenance does not match this build")
    entries = manifest["artifacts"]
    if not isinstance(entries, list) or not (1 <= len(entries) <= 24):
        raise ValueError("Invalid preview artifact list")
    seen = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"name", "bytes", "sha256"}:
            raise ValueError("Invalid artifact record")
        if not isinstance(entry["name"], str) or entry["name"] in seen:
            raise ValueError("Invalid or duplicate artifact name")
        seen.add(entry["name"])
        if entry != artifact(root, entry["name"]):
            raise ValueError("Preview artifact does not match recorded SHA256 and size")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("create")
    build.add_argument("--directory", required=True)
    build.add_argument("--source-sha", required=True)
    build.add_argument("--platform", choices=["linux", "windows"], required=True)
    build.add_argument("--target", required=True)
    build.add_argument("--compiler", required=True)
    build.add_argument("--artifact", action="append", required=True)
    check = commands.add_parser("verify")
    check.add_argument("--directory", required=True)
    check.add_argument("--source-sha", required=True)
    check.add_argument("--platform", choices=["linux", "windows"], required=True)
    args = parser.parse_args()
    try:
        if args.command == "create":
            create(args)
        else:
            verify(args)
    except (OSError, ValueError, TypeError, json.JSONDecodeError) as error:
        parser.exit(1, f"X preview provenance failed: {error}\n")


if __name__ == "__main__":
    main()
