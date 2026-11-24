#!/usr/bin/env python3
"""List active Behave scenario locations from feature files."""

from __future__ import annotations

from pathlib import Path

BDD_ROOT = Path(__file__).resolve().parents[1]


def emit_feature(feature: Path) -> None:
    pending_tags: list[str] = []
    feature_skipped = False
    for line_number, raw_line in enumerate(
        feature.read_text(encoding="utf-8").splitlines(), start=1
    ):
        stripped = raw_line.strip()
        if not stripped:
            continue
        if stripped.startswith("@"):
            pending_tags = stripped.split()
            continue
        if stripped.startswith("Feature:"):
            feature_skipped = "@skip" in pending_tags
            pending_tags = []
            continue
        if stripped.startswith("Scenario:"):
            if not feature_skipped and "@skip" not in pending_tags:
                print(f"{feature.relative_to(BDD_ROOT)}:{line_number}")
            pending_tags = []
            continue
        pending_tags = []


def main() -> None:
    for feature in sorted((BDD_ROOT / "features").glob("*.feature")):
        emit_feature(feature)


if __name__ == "__main__":
    main()
