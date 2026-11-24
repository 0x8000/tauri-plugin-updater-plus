"""Object-oriented drivers for Nginx and Origin/CDN release servers."""

from __future__ import annotations

import gzip
import json
import shutil
from dataclasses import dataclass
from pathlib import Path

import requests

from ..containers import CdnGatewayContainer, NginxReleaseContainer
from .blockmap_cli import BlockmapCliDriver


@dataclass(frozen=True)
class NginxAccessEntry:
    method: str
    uri: str
    status: int
    range_header: str
    sent_content_type: str
    bytes_sent: int


class NginxReleaseDriver:
    """Publishes release artifacts to a real Nginx server and inspects access telemetry."""

    def __init__(self, container: NginxReleaseContainer, cli: BlockmapCliDriver) -> None:
        self.container = container
        self.cli = cli

    @property
    def base_url(self) -> str:
        return self.container.base_url

    def manifest_url(self, filename: str = "latest.json") -> str:
        return f"{self.base_url}{filename}"

    def publish_release(
        self,
        *,
        artifact_path: Path,
        version: str,
        manifest_name: str = "latest.json",
        manifest_format: str = "tauri",
        signature: str | None = None,
        notes: str | None = None,
    ) -> str:
        dest_artifact = self.container.releases_dir / artifact_path.name
        shutil.copy2(artifact_path, dest_artifact)
        dest_artifact.chmod(0o644)

        dest_blockmap = self.container.releases_dir / f"{artifact_path.name}.blockmap"
        dest_manifest = self.container.releases_dir / manifest_name
        artifact_url = f"{self.base_url}{artifact_path.name}"
        self.cli.manifest(
            input_file=dest_artifact,
            version=version,
            url=artifact_url,
            output_file=dest_manifest,
            manifest_format=manifest_format,
            signature=signature,
            notes=notes,
            blockmap_output=dest_blockmap,
        )
        dest_blockmap.chmod(0o644)
        dest_manifest.chmod(0o644)
        return self.manifest_url(manifest_name)

    def publish_raw_json(self, filename: str, payload: dict) -> str:
        dest = self.container.releases_dir / filename
        dest.write_text(json.dumps(payload, indent=2), encoding="utf-8")
        dest.chmod(0o644)
        return self.manifest_url(filename)

    def tamper_published_artifact(self, filename: str, offset: int = 220_500) -> None:
        target = self.container.releases_dir / filename
        raw = bytearray(target.read_bytes())
        raw[offset] ^= 0xFF
        target.write_bytes(bytes(raw))

    def replace_blockmap_with_gzip_bomb(
        self, artifact_filename: str, uncompressed_mib: int = 66
    ) -> None:
        target = self.container.releases_dir / f"{artifact_filename}.blockmap"
        with gzip.open(target, "wb", compresslevel=9) as gz:
            gz.write(b'{"version":"2","pad":"')
            chunk = b"a" * (1024 * 1024)
            for _ in range(uncompressed_mib):
                gz.write(chunk)
        target.chmod(0o644)

    def access_entries(self, uri_suffix: str | None = None) -> list[NginxAccessEntry]:
        log_file = self.container.logs_dir / "access.jsonl"
        if not log_file.is_file():
            return []
        entries: list[NginxAccessEntry] = []
        for line in log_file.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            item = json.loads(line)
            entry = NginxAccessEntry(
                method=str(item["method"]),
                uri=str(item["uri"]),
                status=int(item["status"]),
                range_header=str(item.get("range", "")),
                sent_content_type=str(item.get("sent_content_type", "")),
                bytes_sent=int(item["bytes_sent"]),
            )
            if uri_suffix is None or entry.uri.endswith(uri_suffix):
                entries.append(entry)
        return entries

    def multipart_206_count(self, artifact_filename: str) -> int:
        return sum(
            1
            for e in self.access_entries(artifact_filename)
            if e.status == 206 and "multipart/byteranges" in e.sent_content_type.lower()
        )

    def range_206_count(self, artifact_filename: str) -> int:
        return sum(1 for e in self.access_entries(artifact_filename) if e.status == 206)

    def requested_range_spec_count(self, artifact_filename: str) -> int:
        total = 0
        for entry in self.access_entries(artifact_filename):
            if entry.status == 206 and entry.range_header.startswith("bytes="):
                specs = entry.range_header[len("bytes=") :].split(",")
                total += sum(1 for spec in specs if spec.strip())
        return total


class CdnOriginDriver:
    """Publishes releases to the Origin/CDN gateway and queries HTTP telemetry."""

    def __init__(self, gateway: CdnGatewayContainer, cli: BlockmapCliDriver) -> None:
        self.gateway = gateway
        self.cli = cli

    @property
    def base_url(self) -> str:
        return self.gateway.origin_base_url

    def manifest_url(self, filename: str = "latest.json") -> str:
        return f"{self.base_url}{filename}"

    def set_mode(self, mode: str, *, token_max_hits: int = 1) -> None:
        requests.post(
            f"{self.gateway.admin_base_url}/config",
            json={"mode": mode, "token_max_hits": token_max_hits},
            timeout=5.0,
        ).raise_for_status()

    def publish_release(
        self,
        *,
        artifact_path: Path,
        version: str,
        manifest_name: str = "latest.json",
        manifest_format: str = "tauri",
        notes: str | None = None,
    ) -> str:
        dest_artifact = self.gateway.releases_dir / artifact_path.name
        shutil.copy2(artifact_path, dest_artifact)
        dest_artifact.chmod(0o644)

        dest_blockmap = self.gateway.releases_dir / f"{artifact_path.name}.blockmap"
        dest_manifest = self.gateway.releases_dir / manifest_name
        artifact_url = f"{self.base_url}{artifact_path.name}"
        self.cli.manifest(
            input_file=dest_artifact,
            version=version,
            url=artifact_url,
            output_file=dest_manifest,
            manifest_format=manifest_format,
            notes=notes,
            blockmap_output=dest_blockmap,
        )
        dest_blockmap.chmod(0o644)
        dest_manifest.chmod(0o644)
        return self.manifest_url(manifest_name)

    def telemetry(self) -> dict:
        resp = requests.get(f"{self.gateway.admin_base_url}/telemetry", timeout=5.0)
        resp.raise_for_status()
        return dict(resp.json())
