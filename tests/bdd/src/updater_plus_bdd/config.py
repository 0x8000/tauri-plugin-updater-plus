"""Shared constants and binary paths for the updater-plus BDD suite."""

from __future__ import annotations

import os
from pathlib import Path

PACKAGE_DIR = Path(__file__).resolve().parent
BDD_ROOT = PACKAGE_DIR.parents[1]
REPO_ROOT = BDD_ROOT.parents[1]

IMAGE_PREFIX = os.environ.get("UPDATER_PLUS_BDD_IMAGE_PREFIX", "updater-plus-bdd")
BDD_LABEL_KEY = os.environ.get("UPDATER_PLUS_BDD_LABEL_KEY", "updater-plus.bdd")
BDD_SCENARIO_LABEL_KEY = f"{BDD_LABEL_KEY}.scenario"
BUILD_HASH_LABEL = "updater-plus.bdd.buildhash"

REBUILD_IMAGES = os.environ.get("UPDATER_PLUS_BDD_REBUILD_IMAGES", "0") == "1"
FORCE_IMAGE_REBUILD = os.environ.get("UPDATER_PLUS_BDD_FORCE_IMAGE_REBUILD", "0") == "1"

BDD_BUILT_SERVICES = ("nginx-release", "cdn-gateway", "mqtt-broker")

NGINX_CONTAINER_PORT = 80
CDN_ORIGIN_PORT = 8080
CDN_ASSET_PORT = 8081
MQTT_BROKER_PORT = 1883
MQTT_ADMIN_PORT = 8082


def image_name(service: str) -> str:
    """Return the local Docker image tag for a BDD-built service."""
    return f"{IMAGE_PREFIX}/{service}:latest"


def image_repo_prefix() -> str:
    """Return the repository prefix for BDD-built images."""
    return f"{IMAGE_PREFIX}/"


def blockmap_gen_binary() -> Path:
    """Resolve the compiled blockmap-gen CLI binary."""
    override = os.environ.get("UPDATER_PLUS_BDD_BLOCKMAP_GEN")
    if override:
        path = Path(override)
        if not path.is_file():
            raise RuntimeError(f"UPDATER_PLUS_BDD_BLOCKMAP_GEN does not exist: {path}")
        return path
    candidate = REPO_ROOT / "target" / "debug" / "blockmap-gen"
    if not candidate.is_file():
        raise RuntimeError(f"missing {candidate}; build with `just bdd binaries` first")
    return candidate


def updater_client_binary() -> Path:
    """Resolve the compiled updater_client harness binary."""
    override = os.environ.get("UPDATER_PLUS_BDD_CLIENT")
    if override:
        path = Path(override)
        if not path.is_file():
            raise RuntimeError(f"UPDATER_PLUS_BDD_CLIENT does not exist: {path}")
        return path
    candidate = REPO_ROOT / "target" / "debug" / "examples" / "updater_client"
    if not candidate.is_file():
        raise RuntimeError(f"missing {candidate}; build with `just bdd binaries` first")
    return candidate
