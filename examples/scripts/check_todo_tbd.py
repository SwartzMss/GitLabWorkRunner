#!/usr/bin/env python
import sys
from pathlib import Path

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
    if len(sys.argv) != 3:
        print("usage: check_todo_tbd.py <check_root> <result_path>")
        return 2
    root = Path(sys.argv[1]).resolve()
    result_path = Path(sys.argv[2]).resolve()
    print(f"checking root: {root}")
    findings = []
    for path in iter_files(root):
        print(f"scan {path.relative_to(root)}")
        text = read_text(path)
        if text is None:
            continue
        for line_no, line in enumerate(text.splitlines(), start=1):
            if any(pattern in line for pattern in PATTERNS):
                findings.append(f"{path.relative_to(root)}:{line_no}: {line.strip()}")

    if not findings:
        result_path.write_text("No //TODO or //TBD markers found.\n", encoding="utf-8")
        print("No //TODO or //TBD markers found.")
        return 0

    result_path.write_text(
        "Found //TODO or //TBD markers:\n" + "\n".join(findings) + "\n",
        encoding="utf-8",
    )
    print("Found //TODO or //TBD markers:")
    for finding in findings:
        print(finding)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
