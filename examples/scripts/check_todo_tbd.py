#!/usr/bin/env python3
import os
from pathlib import Path

ROOT = Path(os.environ.get("GITLAB_WORK_RUNNER_CHECK_ROOT", Path.cwd())).resolve()
SKIP_DIRS = {".git", ".idea", ".vscode", "target", "node_modules", "vendor"}
PATTERNS = ("//TODO", "//TBD")


def iter_files(root: Path):
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        if any(part in SKIP_DIRS for part in path.parts):
            continue
        yield path


def read_text(path: Path):
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return None
    except OSError as err:
        print(f"skip {path}: {err}")
        return None


def main() -> int:
    findings = []
    for path in iter_files(ROOT):
        text = read_text(path)
        if text is None:
            continue
        for line_no, line in enumerate(text.splitlines(), start=1):
            if any(pattern in line for pattern in PATTERNS):
                findings.append(f"{path.relative_to(ROOT)}:{line_no}: {line.strip()}")

    if not findings:
        print("No //TODO or //TBD markers found.")
        return 0

    print("Found //TODO or //TBD markers:")
    for finding in findings:
        print(finding)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
