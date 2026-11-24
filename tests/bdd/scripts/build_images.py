#!/usr/bin/env python3
"""Build BDD service Docker images when inputs change."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

BDD_ROOT = Path(__file__).resolve().parents[1]
SRC = BDD_ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from updater_plus_bdd.config import BDD_BUILT_SERVICES  # noqa: E402
from updater_plus_bdd.images import ImageBuilder  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("services", nargs="*", default=list(BDD_BUILT_SERVICES))
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    builder = ImageBuilder()
    for service in args.services:
        tag = builder.ensure(service, force=args.force)
        print(f"[bdd] image ready: {tag}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
