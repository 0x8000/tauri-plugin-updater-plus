"""Staleness-aware Docker image builder for BDD service images."""

from __future__ import annotations

import hashlib
import subprocess
from pathlib import Path

from .config import (
    BDD_BUILT_SERVICES,
    BDD_LABEL_KEY,
    BDD_ROOT,
    BUILD_HASH_LABEL,
    FORCE_IMAGE_REBUILD,
    REBUILD_IMAGES,
    REPO_ROOT,
    image_name,
)

DOCKERFILES: dict[str, str] = {
    "nginx-release": "docker/nginx-release.Dockerfile",
    "cdn-gateway": "docker/cdn-gateway.Dockerfile",
    "mqtt-broker": "docker/mqtt-broker.Dockerfile",
}

BUILD_INPUTS: dict[str, tuple[str, ...]] = {
    "nginx-release": ("tests/bdd/docker/nginx-release",),
    "cdn-gateway": ("tests/bdd/docker/cdn-gateway",),
    "mqtt-broker": ("tests/bdd/docker/mqtt-broker",),
}


class ImageBuilder:
    """Builds and caches BDD Docker service images keyed by input content hash."""

    def ensure_all(
        self, services: tuple[str, ...] = BDD_BUILT_SERVICES, *, force: bool = False
    ) -> None:
        for service in services:
            self.ensure(service, force=force)

    def ensure(self, service: str, *, force: bool = False) -> str:
        if service not in DOCKERFILES:
            raise ValueError(f"unknown BDD service image '{service}'")
        tag = image_name(service)
        expected_hash = self.compute_build_hash(service)
        must_rebuild = force or REBUILD_IMAGES or FORCE_IMAGE_REBUILD
        if not must_rebuild and self._current_image_hash(tag) == expected_hash:
            return tag

        dockerfile = BDD_ROOT / DOCKERFILES[service]
        cmd = [
            "docker",
            "build",
            "--file",
            str(dockerfile),
            "--tag",
            tag,
            "--label",
            f"{BDD_LABEL_KEY}=1",
            "--label",
            f"{BUILD_HASH_LABEL}={expected_hash}",
            str(REPO_ROOT),
        ]
        subprocess.run(cmd, check=True)
        return tag

    def compute_build_hash(self, service: str) -> str:
        digest = hashlib.sha256()
        dockerfile = BDD_ROOT / DOCKERFILES[service]
        self._hash_file(digest, dockerfile.relative_to(REPO_ROOT), dockerfile)
        for rel_root in BUILD_INPUTS.get(service, ()):
            root_path = REPO_ROOT / rel_root
            if root_path.is_file():
                self._hash_file(digest, Path(rel_root), root_path)
            elif root_path.is_dir():
                for child in sorted(p for p in root_path.rglob("*") if p.is_file()):
                    self._hash_file(digest, child.relative_to(REPO_ROOT), child)
        return digest.hexdigest()[:24]

    @staticmethod
    def _hash_file(digest: hashlib._Hash, rel: Path, path: Path) -> None:
        digest.update(rel.as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")

    @staticmethod
    def _current_image_hash(tag: str) -> str | None:
        result = subprocess.run(
            [
                "docker",
                "image",
                "inspect",
                tag,
                "--format",
                f'{{{{index .Config.Labels "{BUILD_HASH_LABEL}"}}}}',
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            return None
        value = result.stdout.strip()
        return value or None
