"""Object-oriented drivers for minisign signing and the updater_client harness."""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

from ..config import updater_client_binary

DEFAULT_SEED_HEX = "42" * 32


@dataclass(frozen=True)
class UpdaterRunOutcome:
    exit_code: int
    status: str
    error_kind: str | None
    message: str | None
    update_available: bool
    version: str | None
    notes: str | None
    mode: str | None
    total_bytes: int
    downloaded_bytes: int
    reused_bytes: int
    http_requests: int
    saved_percent: float
    fallback_reason: str | None
    events_count: int
    started_differential: bool | None
    progress_bytes_total: int
    has_finished_event: bool
    cached_version: str | None


class MinisignAuthority:
    """Issues real Ed25519 prehashed minisign public keys and signatures."""

    def __init__(self, seed_hex: str = DEFAULT_SEED_HEX, binary: Path | None = None) -> None:
        self.seed_hex = seed_hex
        self.binary = binary or updater_client_binary()
        self._pubkey: str | None = None

    @property
    def pubkey(self) -> str:
        if self._pubkey is None:
            proc = subprocess.run(
                [str(self.binary), "keygen", "--seed-hex", self.seed_hex],
                capture_output=True,
                text=True,
                check=True,
            )
            self._pubkey = str(json.loads(proc.stdout)["pubkey"])
        return self._pubkey

    def sign_artifact(self, artifact_path: Path, signed_version: str | None = None) -> str:
        digest_hex = hashlib.blake2b(artifact_path.read_bytes(), digest_size=64).hexdigest()
        parts = ["timestamp:1700000000", f"file:{artifact_path.name}"]
        if signed_version is not None:
            parts.append(f"version:{signed_version}")
        trusted_comment = "\t".join(parts)
        proc = subprocess.run(
            [
                str(self.binary),
                "sign",
                "--seed-hex",
                self.seed_hex,
                "--blake2b-hex",
                digest_hex,
                "--trusted-comment",
                trusted_comment,
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        return str(json.loads(proc.stdout)["signature"])


class UpdaterAppDriver:
    """Models an installed Tauri application invoking tauri-plugin-updater-plus."""

    def __init__(self, app_root: Path, binary: Path | None = None) -> None:
        self.app_root = app_root
        self.app_root.mkdir(parents=True, exist_ok=True)
        self.binary = binary or updater_client_binary()
        self.appimage_path = self.app_root / "MyApp.AppImage"
        self.cache_dir = self.app_root / "cache"
        self.current_version = "0.1.0"
        self.endpoints: list[str] = []
        self.pubkey: str | None = None
        self.require_signed_version = False
        self.differential = True
        self.full_download_threshold = 0.85
        self.use_multiple_range_request: bool | None = None
        self.max_concurrent_ranges = 4
        self.headers: list[tuple[str, str]] = []
        self.last_outcome: UpdaterRunOutcome | None = None
        self._spec_seq = 0

    def install_initial_appimage(
        self, source_artifact: Path, version: str, mode: int = 0o750
    ) -> None:
        shutil.copy2(source_artifact, self.appimage_path)
        self.appimage_path.chmod(mode)
        self.current_version = version

    def seed_cache(self, artifact_path: Path, version: str) -> None:
        subprocess.run(
            [
                str(self.binary),
                "seed-cache",
                "--cache-dir",
                str(self.cache_dir),
                "--input",
                str(artifact_path),
                "--version",
                version,
            ],
            capture_output=True,
            text=True,
            check=True,
        )

    def corrupt_cached_package(self, offset: int = 320_000, length: int = 64) -> None:
        cached_pkg = self.cache_dir / "current.pkg"
        raw = bytearray(cached_pkg.read_bytes())
        for idx in range(offset, min(len(raw), offset + length)):
            raw[idx] ^= 0xAA
        cached_pkg.write_bytes(bytes(raw))

    def execute(self, action: str = "download_and_install") -> UpdaterRunOutcome:
        self._spec_seq += 1
        spec_path = self.app_root / f"run-spec-{self._spec_seq}.json"
        spec = {
            "action": action,
            "appName": "myapp",
            "currentVersion": self.current_version,
            "extractPath": str(self.appimage_path),
            "cacheDir": str(self.cache_dir),
            "appimageFallback": str(self.appimage_path) if self.appimage_path.is_file() else None,
            "target": "linux-x86_64",
            "headers": [[k, v] for k, v in self.headers],
            "config": {
                "endpoints": self.endpoints,
                "pubkey": self.pubkey,
                "requireSignedVersion": self.require_signed_version,
                "differential": self.differential,
                "allowDowngrades": False,
                "fullDownloadThreshold": self.full_download_threshold,
                "maxRangesPerRequest": 128,
                "useMultipleRangeRequest": self.use_multiple_range_request,
                "maxConcurrentRanges": self.max_concurrent_ranges,
            },
        }
        spec_path.write_text(json.dumps(spec, indent=2), encoding="utf-8")
        proc = subprocess.run(
            [str(self.binary), "run", "--spec", str(spec_path)],
            capture_output=True,
            text=True,
            check=False,
        )
        payload = json.loads(proc.stdout)
        summary = payload.get("summary") or {}
        outcome = UpdaterRunOutcome(
            exit_code=proc.returncode,
            status=str(payload["status"]),
            error_kind=payload.get("errorKind"),
            message=payload.get("message"),
            update_available=bool(payload.get("updateAvailable", False)),
            version=payload.get("version"),
            notes=payload.get("notes"),
            mode=summary.get("mode"),
            total_bytes=int(summary.get("totalBytes", 0)),
            downloaded_bytes=int(summary.get("downloadedBytes", 0)),
            reused_bytes=int(summary.get("reusedBytes", 0)),
            http_requests=int(summary.get("httpRequests", 0)),
            saved_percent=float(summary.get("savedPercent", 0.0)),
            fallback_reason=summary.get("fallbackReason"),
            events_count=int(payload.get("eventsCount", 0)),
            started_differential=payload.get("startedDifferential"),
            progress_bytes_total=int(payload.get("progressBytesTotal", 0)),
            has_finished_event=bool(payload.get("hasFinishedEvent", False)),
            cached_version=payload.get("cachedVersion"),
        )
        if (
            outcome.exit_code == 0
            and outcome.version
            and action
            in (
                "download_and_install",
                "download_then_install",
            )
        ):
            self.current_version = outcome.version
        self.last_outcome = outcome
        return outcome

    def installed_bytes(self) -> bytes:
        return self.appimage_path.read_bytes()

    def installed_mode(self) -> int:
        return self.appimage_path.stat().st_mode & 0o777

    def cached_bytes(self) -> bytes:
        return (self.cache_dir / "current.pkg").read_bytes()

    def parse_manifest(
        self,
        manifest_path: Path,
        source_url: str,
        target: str = "linux-x86_64",
    ) -> dict:
        proc = subprocess.run(
            [
                str(self.binary),
                "parse-manifest",
                "--input",
                str(manifest_path),
                "--source-url",
                source_url,
                "--target",
                target,
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        return dict(json.loads(proc.stdout))
