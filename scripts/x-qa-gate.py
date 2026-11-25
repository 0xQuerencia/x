#!/usr/bin/env python3
"""Local, privacy-minimal operator record for X's physical release checks."""

import argparse
from collections.abc import Iterable
import json
import os
from pathlib import Path
import re
import tempfile

SCENARIOS = {
    "hyprland": (
        "clean_install", "doctor", "microphone", "spoken_20", "window_identity",
        "undo", "approvals", "workflow", "stop", "privacy", "rollback",
    ),
    "kde": (
        "clean_install", "doctor", "kwin_list", "focus", "workspace",
        "stale_window", "microphone", "workflow", "stop", "privacy",
    ),
    "windows11": (
        "clean_install", "doctor", "service", "microphone", "hotkey",
        "window_list", "focus", "window_undo", "stale_window",
        "desktop_refusal", "browser_tabs", "clipboard", "typing",
        "screenshot", "workflow", "stop", "privacy", "rollback",
    ),
    "macos": (
        "clean_install", "doctor", "microphone", "hotkey", "window_list",
        "focus", "clipboard", "typing", "screenshot", "screen_permission",
        "workflow", "stop", "privacy",
    ),
}
RESULTS = ("pass", "fail", "unsupported")
HASH = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?\Z")


def validate(data):
    if not isinstance(data, dict) or set(data) != {"version", "platform", "source_sha", "cases"}:
        raise ValueError("Invalid X acceptance schema")
    platform = data["platform"]
    if type(data["version"]) is not int or data["version"] != 1:
        raise ValueError("Unknown acceptance record version")
    if not isinstance(platform, str) or platform not in SCENARIOS:
        raise ValueError("Unknown acceptance platform")
    sha = data["source_sha"]
    if not isinstance(sha, str) or not HASH.fullmatch(sha):
        raise ValueError("Record must have an exact source commit SHA")
    cases = data["cases"]
    if not isinstance(cases, dict):
        raise ValueError("Invalid acceptance case data")
    for name, status in cases.items():
        if name not in SCENARIOS[platform] or status not in RESULTS:
            raise ValueError("Acceptance record has unknown case or status")
    return data


def read(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024:
        raise ValueError("Missing, linked or oversized acceptance record")
    return validate(json.loads(path.read_text(encoding="utf-8")))


def save(path, data, *, creating=False):
    validate(data)
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError("Refusing a linked or non-file acceptance destination")
    if creating and path.exists():
        raise ValueError("Existing acceptance records must not be overwritten")
    if not path.parent.is_dir():
        raise ValueError("Create the destination directory first")
    encoded = (json.dumps(data, indent=2, sort_keys=True) + "\n").encode("utf-8")
    with tempfile.NamedTemporaryFile(prefix=".x-qa-", dir=path.parent, delete=False) as tmp:
        tmp_name = tmp.name
        try:
            os.chmod(tmp_name, 0o600)
            tmp.write(encoded)
            tmp.flush()
            os.fsync(tmp.fileno())
        except BaseException:
            os.unlink(tmp_name)
            raise
    try:
        if creating and path.exists():
            raise ValueError("Acceptance record was created by another process")
        os.replace(tmp_name, path)
    finally:
        if os.path.exists(tmp_name):
            os.unlink(tmp_name)


def gate(data):
    required = SCENARIOS[data["platform"]]
    cases = data["cases"]
    missing = [name for name in required if name not in cases]
    failed = [name for name in required if cases.get(name) == "fail"]
    unsupported = [name for name in required if cases.get(name) == "unsupported"]
    return {
        "platform": data["platform"],
        "source_sha": data["source_sha"],
        "ready": not (missing or failed or unsupported),
        "passed": sum(cases.get(name) == "pass" for name in required),
        "required": len(required),
        "missing": missing,
        "failed": failed,
        "unsupported": unsupported,
        "operator_report_only": True,
        "release_published": False,
    }


def check_release(
    source_sha: str, records: Iterable[Path],
) -> dict[str, object]:
    """Combine operator records for the explicitly selected source commit."""
    if not HASH.fullmatch(source_sha):
        raise ValueError("Candidate must have an exact source commit SHA")
    platforms: dict[str, dict[str, object]] = {}
    for path in records:
        record = read(path)
        platform = record["platform"]
        if record["source_sha"] != source_sha:
            raise ValueError(f"{platform} record belongs to a different source commit")
        if platform in platforms:
            raise ValueError(f"Duplicate acceptance record for {platform}")
        platforms[platform] = gate(record)

    missing = [platform for platform in SCENARIOS if platform not in platforms]
    return {
        "source_sha": source_sha,
        "ready": not missing and all(report["ready"] for report in platforms.values()),
        "missing_platforms": missing,
        "platforms": platforms,
        "operator_report_only": True,
        "release_published": False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    start = commands.add_parser("init")
    start.add_argument("--file", required=True)
    start.add_argument("--platform", choices=SCENARIOS, required=True)
    start.add_argument("--source-sha", required=True)
    record = commands.add_parser("record")
    record.add_argument("--file", required=True)
    record.add_argument("--case", required=True)
    record.add_argument("--result", choices=RESULTS, required=True)
    check = commands.add_parser("check")
    check.add_argument("--file", required=True)
    release = commands.add_parser("check-release")
    release.add_argument("--source-sha", required=True)
    release.add_argument("records", nargs="+", type=Path)
    args = parser.parse_args()
    try:
        if args.action == "check-release":
            result = check_release(args.source_sha, args.records)
            print(json.dumps(result, indent=2, sort_keys=True))
            return 0 if result["ready"] else 1

        path = Path(args.file)
        if args.action == "init":
            save(path, {
                "version": 1, "platform": args.platform, "source_sha": args.source_sha,
                "cases": {},
            }, creating=True)
            print("Initialized local acceptance record; no case has passed yet.")
        elif args.action == "record":
            data = read(path)
            if args.case not in SCENARIOS[data["platform"]]:
                raise ValueError("Unknown case for this platform")
            data["cases"][args.case] = args.result
            save(path, data)
            print(f"Recorded {args.case}: {args.result} (operator self-report)")
        else:
            result = gate(read(path))
            print(json.dumps(result, indent=2, sort_keys=True))
            if not result["ready"]:
                parser.exit(1)
    except (ValueError, OSError, json.JSONDecodeError) as error:
        parser.exit(2, f"X acceptance record refused: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
