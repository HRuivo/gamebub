#!/usr/bin/env python3
"""Non-flashing post-change checks for Game Bub handheld firmware."""

from __future__ import annotations

import argparse
import csv
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parent
VALID_REVISIONS = ("rev1", "rev2", "rev3", "rev4")
FLASH_SIZE = 8 * 1024 * 1024


@dataclass
class Check:
    name: str
    status: str
    detail: str = ""


@dataclass(frozen=True)
class Risk:
    severity: str
    category: str
    file: str
    detail: str


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )


def concise_output(output: str, line_limit: int = 12) -> str:
    lines = [line.rstrip() for line in output.splitlines() if line.strip()]
    return "\n".join(lines[-line_limit:])


def changed_files(base: str) -> tuple[list[str], str | None]:
    tracked = run(
        ["git", "diff", "--relative", "--name-only", "--diff-filter=ACMRD", base, "--"]
    )
    if tracked.returncode:
        return [], concise_output(tracked.stdout)
    untracked = run(["git", "ls-files", "--others", "--exclude-standard"])
    if untracked.returncode:
        return [], concise_output(untracked.stdout)
    files = set(tracked.stdout.splitlines()) | set(untracked.stdout.splitlines())
    return sorted(path for path in files if path), None


def added_lines(path: str, base: str, untracked: set[str]) -> list[tuple[int, str]]:
    if path in untracked:
        try:
            return list(enumerate((ROOT / path).read_text(errors="replace").splitlines(), 1))
        except OSError:
            return []

    diff = run(
        ["git", "diff", "--relative", "--unified=0", "--no-color", base, "--", path]
    )
    result: list[tuple[int, str]] = []
    new_line = 0
    for line in diff.stdout.splitlines():
        match = re.match(r"@@ -(?:\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
        if match:
            new_line = int(match.group(1))
        elif line.startswith("+") and not line.startswith("+++"):
            result.append((new_line, line[1:]))
            new_line += 1
        elif line.startswith(" "):
            new_line += 1
    return result


def scan_risks(files: list[str], base: str) -> list[Risk]:
    untracked_result = run(["git", "ls-files", "--others", "--exclude-standard"])
    untracked = set(untracked_result.stdout.splitlines())
    risks: set[Risk] = set()

    path_rules = (
        ("HIGH", "power/pin hardware", re.compile(r"^(src/(device|power\.rs|led\.rs)|sdkconfig)")),
        ("HIGH", "FPGA/bitstream protocol", re.compile(r"^src/(bitstream|core)/")),
        ("HIGH", "flash/partition layout", re.compile(r"(^|/)(partitions\.csv|espflash\.toml|generate_firmware_uf2\.py|build_firmware\.sh)$")),
        ("MEDIUM", "USB/control protocol", re.compile(r"^src/(control|usb|cart_backup)")),
    )
    for path in files:
        for severity, category, pattern in path_rules:
            if pattern.search(path):
                risks.add(Risk(severity, category, path, "hardware-sensitive file changed"))

    line_rules = (
        ("CRITICAL", "destructive command", re.compile(r"\b(?:erase-flash|erase_flash|rm\s+-rf|esptool(?:\.py)?\s+erase)\b", re.I)),
        ("HIGH", "GPIO/electrical state", re.compile(r"\b(?:gpio|pin|set_high|set_low|drive_strength|pull_up|pull_down)\b", re.I)),
        ("HIGH", "power/analog control", re.compile(r"\b(?:voltage|current|charger|battery|backlight|pwm|dac|power[_ -]?enable)\b", re.I)),
        ("HIGH", "raw hardware/register write", re.compile(r"\b(?:write_u(?:8|16|32)|spi_write|i2c.*write|register.*write)\b", re.I)),
        ("HIGH", "clock/timing change", re.compile(r"\b(?:clock|frequency|baud|delay_(?:ms|us)|sleep|mhz|khz)\b", re.I)),
        ("HIGH", "unsafe code", re.compile(r"\bunsafe\b")),
        ("MEDIUM", "flash operation", re.compile(r"\b(?:cargo\s+(?:run|espflash)|espflash\s+flash|dfu-util)\b", re.I)),
    )
    ignored_suffixes = (".md", ".lock")
    for path in files:
        if path.endswith(ignored_suffixes) or path == Path(__file__).name:
            continue
        for line_number, text in added_lines(path, base, untracked):
            stripped = text.strip()
            if not stripped or stripped.startswith("//") or stripped.startswith("#"):
                continue
            for severity, category, pattern in line_rules:
                if pattern.search(stripped):
                    detail = stripped if len(stripped) <= 120 else stripped[:117] + "..."
                    risks.add(Risk(severity, category, path, f"line {line_number}: {detail}"))
    return sorted(risks, key=lambda risk: (risk.severity, risk.category, risk.file, risk.detail))


def parse_size(value: str) -> int:
    value = value.strip()
    suffixes = {"K": 1024, "M": 1024 * 1024}
    if value[-1:].upper() in suffixes:
        return int(value[:-1], 0) * suffixes[value[-1].upper()]
    return int(value, 0)


def check_partitions() -> Check:
    path = ROOT / "partitions.csv"
    try:
        rows = []
        with path.open(newline="") as source:
            for row in csv.reader(line for line in source if not line.lstrip().startswith("#")):
                if not row or not any(field.strip() for field in row):
                    continue
                name, kind, _subtype, offset, size, *_rest = [field.strip() for field in row]
                rows.append((name, kind.lower(), parse_size(offset), parse_size(size)))
        rows.sort(key=lambda row: row[2])
        for (name, _kind, offset, size), next_row in zip(rows, rows[1:]):
            if offset + size > next_row[2]:
                raise ValueError(f"{name} overlaps {next_row[0]}")
        for name, kind, offset, size in rows:
            if size <= 0 or offset < 0 or offset + size > FLASH_SIZE:
                raise ValueError(f"{name} is outside the 8 MiB flash layout")
            alignment = 0x10000 if kind in ("app", "0") else 0x1000
            if offset % alignment:
                raise ValueError(
                    f"{name} offset {offset:#x} is not aligned to {alignment:#x}"
                )
        return Check("partition bounds", "PASS", f"{len(rows)} partitions fit without overlap")
    except (OSError, ValueError, IndexError) as error:
        return Check("partition bounds", "FAIL", str(error))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="HEAD", help="git base used to identify changes")
    parser.add_argument("--revision", action="append", choices=VALID_REVISIONS, dest="revisions")
    parser.add_argument("--no-build", action="store_true", help="skip cargo checks (must be reported)")
    parser.add_argument("--strict-risk", action="store_true", help="return failure when manual review is required")
    args = parser.parse_args()
    revisions = list(dict.fromkeys(args.revisions or ["rev4"]))

    checks: list[Check] = []
    files, file_error = changed_files(args.base)
    if file_error:
        checks.append(Check("changed file discovery", "FAIL", file_error))
        files = []
    else:
        checks.append(Check("changed file discovery", "PASS", f"{len(files)} file(s) since {args.base}"))

    whitespace = run(["git", "diff", "--check", args.base, "--"])
    checks.append(Check("git diff hygiene", "PASS" if whitespace.returncode == 0 else "FAIL", concise_output(whitespace.stdout)))

    rust_files = [path for path in files if path.endswith(".rs") and (ROOT / path).is_file()]
    if rust_files:
        formatting = run(["rustfmt", "--edition", "2021", "--check", *rust_files])
        checks.append(Check("changed Rust formatting", "PASS" if formatting.returncode == 0 else "FAIL", concise_output(formatting.stdout)))
    else:
        checks.append(Check("changed Rust formatting", "PASS", "no changed Rust files"))

    if "partitions.csv" in files:
        checks.append(check_partitions())
    else:
        checks.append(Check("partition bounds", "PASS", "layout unchanged"))

    if args.no_build:
        checks.append(Check("firmware compilation", "NOT RUN", "explicitly skipped; selected revisions: " + ", ".join(revisions)))
    else:
        for revision in revisions:
            build = run(["cargo", "check", "--features", revision])
            checks.append(Check(f"cargo check ({revision})", "PASS" if build.returncode == 0 else "FAIL", concise_output(build.stdout)))

    risks = scan_risks(files, args.base)
    failed = any(check.status == "FAIL" for check in checks)
    skipped = any(check.status == "NOT RUN" for check in checks)
    conclusion = "FAIL" if failed else "REVIEW REQUIRED" if risks or skipped else "PASS"

    print("\nPOST-CHANGE HARDWARE SAFETY REPORT")
    print("=" * 34)
    print(f"Base: {args.base}")
    print(f"Revisions: {', '.join(revisions)}")
    print(f"Changed files: {len(files)}")
    for path in files:
        print(f"  - {path}")
    print("\nAutomated checks:")
    for check in checks:
        print(f"  [{check.status}] {check.name}")
        if check.detail:
            for line in check.detail.splitlines():
                print(f"      {line}")
    print("\nHardware-sensitive review:")
    if risks:
        for risk in risks:
            print(f"  [{risk.severity}] {risk.category}: {risk.file} — {risk.detail}")
    else:
        print("  No hardware-sensitive changed paths or added lines detected.")
    print("\nManual gates before flashing:")
    print("  - Match Cargo feature, PCB revision, eFuse, and FPGA target.")
    print("  - Resolve every risk above against schematics/datasheets/protocol source.")
    print("  - Use the current-limited first-test procedure in HARDWARE_SAFETY.md.")
    print("  - Preserve a known-good recovery image and capture the serial boot log.")
    print("\nNo hardware was flashed, erased, reset, or contacted by this pass.")
    print(f"RESULT: {conclusion}")

    if failed or (args.strict_risk and (risks or skipped)):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
