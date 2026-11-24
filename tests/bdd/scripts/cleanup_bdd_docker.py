#!/usr/bin/env python3
"""Remove updater-plus BDD-owned Docker containers and networks only."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

BDD_ROOT = Path(__file__).resolve().parents[1]
SRC = BDD_ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

from updater_plus_bdd.config import BDD_LABEL_KEY, image_repo_prefix  # noqa: E402

KEEP_NETWORKS = {"bridge", "host", "none"}
UUID_NETWORK = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, capture_output=True, text=True, check=False)


def parse_labels(raw: str) -> dict[str, str]:
    labels: dict[str, str] = {}
    for item in raw.split(","):
        if "=" in item:
            key, val = item.split("=", 1)
            labels[key.strip()] = val.strip()
    return labels


def select_containers() -> list[tuple[str, str]]:
    prefix = image_repo_prefix()
    listing = run(
        ["docker", "ps", "-a", "--format", "{{.ID}}\t{{.Image}}\t{{.Names}}\t{{.Labels}}"]
    )
    selected: list[tuple[str, str]] = []
    for line in listing.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        cid, image, name = parts[0], parts[1], parts[2]
        labels = parse_labels(parts[3] if len(parts) > 3 else "")
        if labels.get(BDD_LABEL_KEY) == "1" or image.startswith(prefix):
            selected.append((cid, f"{name} ({image})"))
    return selected


def select_networks(bdd_cids: set[str]) -> list[tuple[str, str]]:
    listing = run(["docker", "network", "ls", "--format", "{{.ID}}\t{{.Name}}\t{{.Labels}}"])
    selected: list[tuple[str, str]] = []
    for line in listing.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        net_id, name = parts[0], parts[1]
        if name in KEEP_NETWORKS:
            continue
        labels = parse_labels(parts[2] if len(parts) > 2 else "")
        if labels.get(BDD_LABEL_KEY) == "1":
            selected.append((net_id, name))
            continue
        if UUID_NETWORK.fullmatch(name):
            inspected = run(
                [
                    "docker",
                    "network",
                    "inspect",
                    net_id,
                    "--format",
                    "{{range $id, $c := .Containers}}{{$id}}\n{{end}}",
                ]
            )
            endpoints = {ln.strip() for ln in inspected.stdout.splitlines() if ln.strip()}
            if endpoints & bdd_cids:
                selected.append((net_id, name))
    return selected


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    containers = select_containers()
    networks = select_networks({cid for cid, _ in containers})
    print(f"containers to remove: {len(containers)}")
    for cid, desc in containers:
        print(f"  [container] {cid} {desc}")
    print(f"networks to remove: {len(networks)}")
    for net_id, name in networks:
        print(f"  [network] {net_id} {name}")

    if args.dry_run:
        return 0
    if containers:
        run(["docker", "rm", "-f", *(cid for cid, _ in containers)])
    if networks:
        run(["docker", "network", "rm", *(nid for nid, _ in networks)])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
