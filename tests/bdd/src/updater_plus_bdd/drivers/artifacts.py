"""Deterministic binary artifact generator for differential update scenarios."""

from __future__ import annotations

from pathlib import Path


class ArtifactFactory:
    """Produces deterministic binary AppImage-like payloads with controlled mutations."""

    def __init__(self, workspace: Path) -> None:
        self.workspace = workspace
        self.workspace.mkdir(parents=True, exist_ok=True)
        self._artifacts: dict[str, Path] = {}

    def build_base(self, version: str, size_bytes: int = 1_500_000, seed: int = 0x20261124) -> Path:
        data = bytearray(size_bytes)
        header = f"#!/bin/sh\n# Mock AppImage version={version}\n".encode()
        data[: len(header)] = header
        state = seed & 0xFFFFFFFFFFFFFFFF
        for idx in range(len(header), size_bytes):
            state = (state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
            data[idx] = (state >> 56) & 0xFF
        out = self.workspace / f"MyApp-{version}.AppImage"
        out.write_bytes(bytes(data))
        out.chmod(0o755)
        self._artifacts[version] = out
        return out

    def build_delta_from(
        self,
        base_version: str,
        new_version: str,
        mutated_offsets: tuple[int, ...] = (180_000, 720_000, 1_200_000),
        span_bytes: int = 2048,
    ) -> Path:
        base_data = bytearray(self.path_for(base_version).read_bytes())
        fill_seed = sum(ord(ch) for ch in new_version) & 0xFF
        for step, offset in enumerate(mutated_offsets):
            end = min(len(base_data), offset + span_bytes)
            base_data[offset:end] = bytes([(fill_seed + step * 17) & 0xFF]) * (end - offset)
        out = self.workspace / f"MyApp-{new_version}.AppImage"
        out.write_bytes(bytes(base_data))
        out.chmod(0o755)
        self._artifacts[new_version] = out
        return out

    def create_macos_bundle(self, bundle_name: str, version: str) -> Path:
        bundle_dir = self.workspace / bundle_name
        macos_dir = bundle_dir / "Contents" / "MacOS"
        fw_ver_dir = bundle_dir / "Contents" / "Frameworks" / "Core.framework" / "Versions" / "A"
        macos_dir.mkdir(parents=True, exist_ok=True)
        fw_ver_dir.mkdir(parents=True, exist_ok=True)

        exe = macos_dir / "MyApp"
        exe.write_bytes(f"#!/bin/sh\necho {version}\n".encode())
        exe.chmod(0o755)

        (fw_ver_dir / "Core").write_bytes(b"mach-o-framework-payload-" * 256)
        symlink_path = bundle_dir / "Contents" / "Frameworks" / "Core.framework" / "Core"
        if symlink_path.exists() or symlink_path.is_symlink():
            symlink_path.unlink()
        symlink_path.symlink_to("Versions/A/Core")
        return bundle_dir

    def path_for(self, version: str) -> Path:
        if version not in self._artifacts:
            raise KeyError(f"artifact version '{version}' has not been built")
        return self._artifacts[version]
