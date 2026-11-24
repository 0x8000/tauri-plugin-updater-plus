"""Thin Behave step glue for the blockmap-gen CLI surface."""

from __future__ import annotations

import base64
import hashlib
import zipfile

from behave import given, then, when


@given('a base release artifact "{version:S}" of size {size_bytes:d} bytes')
def step_base_artifact(context, version: str, size_bytes: int) -> None:
    context.artifacts.build_base(version, size_bytes=size_bytes)


@given(
    'a delta release artifact "{new_version:S}" derived from "{base_version:S}" at offsets "{offsets:S}"'
)
def step_delta_artifact(context, new_version: str, base_version: str, offsets: str) -> None:
    parsed_offsets = tuple(int(part) for part in offsets.split(",") if part)
    context.artifacts.build_delta_from(base_version, new_version, mutated_offsets=parsed_offsets)


@given(
    'a heavily changed release artifact "{new_version:S}" of size {size_bytes:d} bytes with seed {seed:d}'
)
def step_heavy_artifact(context, new_version: str, size_bytes: int, seed: int) -> None:
    context.artifacts.build_base(new_version, size_bytes=size_bytes, seed=seed)


@given('a macOS app bundle "{bundle_name:S}" at version "{version:S}" with a framework symlink')
def step_macos_bundle(context, bundle_name: str, version: str) -> None:
    context.artifacts.create_macos_bundle(bundle_name, version)


@when('I run blockmap-gen generate on artifact "{version:S}"')
def step_run_generate(context, version: str) -> None:
    artifact = context.artifacts.path_for(version)
    context.last_gen_report = context.cli.generate(artifact)


@when(
    'I run blockmap-gen manifest for artifact "{version:S}" in "{fmt:S}" format with notes "{notes}"'
)
def step_run_manifest(context, version: str, fmt: str, notes: str) -> None:
    artifact = context.artifacts.path_for(version)
    ext = "json" if fmt == "tauri" else "yml"
    out = context.stack.temp_dir / f"cli-manifest-{version}.{ext}"
    decoded_notes = notes.replace("\\n", "\n")
    context.last_manifest_path = context.cli.manifest(
        input_file=artifact,
        version=version,
        url=f"https://releases.example.com/{artifact.name}",
        output_file=out,
        manifest_format=fmt,
        notes=decoded_notes,
    )


@when('I run blockmap-gen diff from "{old_version:S}" to "{new_version:S}"')
def step_run_diff(context, old_version: str, new_version: str) -> None:
    old_report = context.cli.generate(context.artifacts.path_for(old_version))
    new_report = context.cli.generate(context.artifacts.path_for(new_version))
    context.last_diff_report = context.cli.diff(old_report.blockmap_path, new_report.blockmap_path)


@when('I run blockmap-gen pack-zip on bundle "{bundle_name:S}"')
def step_run_pack_zip(context, bundle_name: str) -> None:
    bundle_dir = context.artifacts.workspace / bundle_name
    out_zip = context.stack.temp_dir / f"{bundle_name}.zip"
    context.last_packed_zip = context.cli.pack_zip(bundle_dir, out_zip)


@then(
    "the generated blockmap reports {size_bytes:d} total bytes and at least {min_blocks:d} blocks"
)
def step_assert_gen_report(context, size_bytes: int, min_blocks: int) -> None:
    report = context.last_gen_report
    assert report is not None, "no blockmap-gen report recorded"
    assert report.size == size_bytes, f"expected {size_bytes}, got {report.size}"
    assert report.block_count >= min_blocks, (
        f"expected >={min_blocks} blocks, got {report.block_count}"
    )
    assert report.blockmap_path.is_file()


@then(
    'the generated "{fmt:S}" manifest parses cleanly with version "{version:S}" and notes "{notes}"'
)
def step_assert_manifest(context, fmt: str, version: str, notes: str) -> None:
    artifact = context.artifacts.path_for(version)
    expected_notes = notes.replace("\\n", "\n")
    expected_sha512 = base64.b64encode(hashlib.sha512(artifact.read_bytes()).digest()).decode(
        "ascii"
    )
    source_name = "latest.json" if fmt == "tauri" else "latest-linux.yml"
    parsed = context.app.parse_manifest(
        context.last_manifest_path,
        source_url=f"https://releases.example.com/{source_name}",
    )
    assert parsed["version"] == version, f"expected {version}, got {parsed}"
    assert parsed["notes"] == expected_notes, (
        f"expected notes {expected_notes!r}, got {parsed['notes']!r}"
    )
    assert parsed["url"] == f"https://releases.example.com/{artifact.name}", parsed
    assert parsed["sha512"] == expected_sha512, f"sha512 mismatch: {parsed}"
    assert parsed["size"] == artifact.stat().st_size, f"size mismatch: {parsed}"
    assert parsed["blockmapSize"] and parsed["blockmapSize"] > 0, f"missing blockmapSize: {parsed}"


@then(
    "the blockmap diff saves more than {min_saved:f} percent across {expected_ranges:d} download ranges"
)
def step_assert_diff(context, min_saved: float, expected_ranges: int) -> None:
    diff = context.last_diff_report
    assert diff is not None, "no diff report recorded"
    assert diff.saved_percent > min_saved, f"expected >{min_saved}%, got {diff.saved_percent}%"
    assert diff.range_count == expected_ranges, (
        f"expected {expected_ranges} ranges, got {diff.range_count}"
    )


@then(
    'the packed zip preserves the "{bundle_name:S}" root folder executable mode and framework symlink'
)
def step_assert_packed_zip(context, bundle_name: str) -> None:
    with zipfile.ZipFile(context.last_packed_zip, "r") as zf:
        names = zf.namelist()
        assert names, "packed zip is empty"
        assert all(name.startswith(f"{bundle_name}/") for name in names), f"bad root names: {names}"

        exe_info = zf.getinfo(f"{bundle_name}/Contents/MacOS/MyApp")
        unix_mode = (exe_info.external_attr >> 16) & 0o777
        assert unix_mode == 0o755, f"expected 0o755 executable mode, got {oct(unix_mode)}"

        link_info = zf.getinfo(f"{bundle_name}/Contents/Frameworks/Core.framework/Core")
        file_type = (link_info.external_attr >> 16) & 0o170000
        assert file_type == 0o120000, f"expected symlink mode 0o120000, got {oct(file_type)}"
        assert zf.read(link_info).decode("utf-8") == "Versions/A/Core"
