"""Object-oriented driver for the `blockmap-gen` CLI binary."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from pathlib import Path

from ..config import blockmap_gen_binary


@dataclass(frozen=True)
class BlockmapGenReport:
    input_path: Path
    blockmap_path: Path
    size: int
    sha512: str
    blockmap_size: int
    block_count: int


@dataclass(frozen=True)
class BlockmapDiffReport:
    total_bytes: int
    reused_bytes: int
    download_bytes: int
    range_count: int
    saved_percent: float


class BlockmapCliDriver:
    """Program-oriented driver wrapping `blockmap-gen` subcommands."""

    def __init__(self, binary: Path | None = None) -> None:
        self.binary = binary or blockmap_gen_binary()

    def generate(self, input_file: Path, output_file: Path | None = None) -> BlockmapGenReport:
        cmd = [str(self.binary), "generate", "--input", str(input_file)]
        if output_file is not None:
            cmd.extend(["--output", str(output_file)])
        proc = subprocess.run(cmd, capture_output=True, text=True, check=True)
        payload = json.loads(proc.stdout)
        return BlockmapGenReport(
            input_path=Path(payload["input"]),
            blockmap_path=Path(payload["blockmap"]),
            size=int(payload["size"]),
            sha512=str(payload["sha512"]),
            blockmap_size=int(payload["blockmapSize"]),
            block_count=int(payload["blockCount"]),
        )

    def manifest(
        self,
        *,
        input_file: Path,
        version: str,
        url: str,
        output_file: Path,
        manifest_format: str = "tauri",
        target: str = "linux-x86_64",
        signature: str | None = None,
        notes: str | None = None,
        blockmap_output: Path | None = None,
    ) -> Path:
        cmd = [
            str(self.binary),
            "manifest",
            "--input",
            str(input_file),
            "--version",
            version,
            "--url",
            url,
            "--format",
            manifest_format,
            "--target",
            target,
            "--output",
            str(output_file),
        ]
        if signature is not None:
            cmd.extend(["--signature", signature])
        if notes is not None:
            cmd.extend(["--notes", notes])
        if blockmap_output is not None:
            cmd.extend(["--blockmap-output", str(blockmap_output)])
        subprocess.run(cmd, capture_output=True, text=True, check=True)
        return output_file

    def diff(
        self,
        old_blockmap: Path,
        new_blockmap: Path,
        max_range_gap: int = 0,
    ) -> BlockmapDiffReport:
        cmd = [
            str(self.binary),
            "diff",
            "--old-blockmap",
            str(old_blockmap),
            "--new-blockmap",
            str(new_blockmap),
            "--max-range-gap",
            str(max_range_gap),
        ]
        proc = subprocess.run(cmd, capture_output=True, text=True, check=True)
        payload = json.loads(proc.stdout)
        return BlockmapDiffReport(
            total_bytes=int(payload["totalBytes"]),
            reused_bytes=int(payload["reusedBytes"]),
            download_bytes=int(payload["downloadBytes"]),
            range_count=int(payload["rangeCount"]),
            saved_percent=float(payload["savedPercent"]),
        )

    def pack_zip(self, directory: Path, output_zip: Path) -> Path:
        subprocess.run(
            [
                str(self.binary),
                "pack-zip",
                "--dir",
                str(directory),
                "--output",
                str(output_zip),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        return output_zip
