"""End-to-end GitHub release differential benchmark runner and report generator."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

USER_AGENT = "updater-plus-release-benchmark"
DOWNLOAD_CHUNK_BYTES = 256 * 1024
MAX_API_PAGES = 5
PER_PAGE = 30
DOWNLOAD_RETRIES = 3


@dataclass(frozen=True)
class TargetSpec:
    target_id: str
    repo: str
    asset_pattern: str
    format_label: str
    release_count: int
    include_prerelease: bool = False


@dataclass(frozen=True)
class ReleaseAsset:
    tag: str
    published_at: str
    asset_name: str
    download_url: str
    size: int
    digest: str | None = None


@dataclass(frozen=True)
class RunOptions:
    binary: Path
    output_dir: Path
    work_dir: Path | None
    token: str | None
    keep_downloads: bool


def load_preset_targets(
    config_path: Path,
    release_count_override: int | None = None,
) -> list[TargetSpec]:
    raw = json.loads(config_path.read_text(encoding="utf-8"))
    default_count = int(raw.get("defaultReleaseCount", 4))
    targets: list[TargetSpec] = []
    for entry in raw.get("targets", []):
        count = (
            release_count_override
            if release_count_override is not None
            else int(entry.get("releaseCount", default_count))
        )
        if count < 2:
            raise ValueError(
                f"target {entry['id']} requires at least 2 releases, got {count}"
            )
        targets.append(
            TargetSpec(
                target_id=entry["id"],
                repo=entry["repo"],
                asset_pattern=entry["assetPattern"],
                format_label=entry.get("format", "unknown"),
                release_count=count,
                include_prerelease=bool(entry.get("includePrerelease", False)),
            )
        )
    return targets


def resolve_targets(args: argparse.Namespace) -> list[TargetSpec]:
    count_override = args.release_count if args.release_count is not None else None
    if getattr(args, "repo", None):
        if not args.asset_pattern:
            raise ValueError("--asset-pattern is required when --repo is provided")
        count = count_override if count_override is not None else 4
        if count < 2:
            raise ValueError(f"ad-hoc target requires at least 2 releases, got {count}")
        slug = re.sub(r"[^a-zA-Z0-9._-]+", "-", args.repo.replace("/", "-")).strip("-")
        return [
            TargetSpec(
                target_id=args.target or slug.lower(),
                repo=args.repo,
                asset_pattern=args.asset_pattern,
                format_label=args.format_label or "custom",
                release_count=count,
                include_prerelease=bool(args.include_prerelease),
            )
        ]

    targets = load_preset_targets(args.config, release_count_override=count_override)
    if args.target:
        selected = [t for t in targets if t.target_id == args.target]
        if not selected:
            available = ", ".join(t.target_id for t in targets)
            raise ValueError(f"unknown target '{args.target}'; available: {available}")
        return selected
    return targets


def github_headers(token: str | None) -> dict[str, str]:
    headers = {
        "User-Agent": USER_AGENT,
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def select_matching_releases(
    releases: list[dict[str, Any]],
    spec: TargetSpec,
) -> list[ReleaseAsset]:
    pattern = re.compile(spec.asset_pattern)
    matched: list[ReleaseAsset] = []
    for release in releases:
        if release.get("draft"):
            continue
        if release.get("prerelease") and not spec.include_prerelease:
            continue
        asset = find_matching_asset(release.get("assets", []), pattern)
        if asset is None:
            continue
        matched.append(
            ReleaseAsset(
                tag=str(release["tag_name"]),
                published_at=str(release.get("published_at") or ""),
                asset_name=str(asset["name"]),
                download_url=str(asset["browser_download_url"]),
                size=int(asset["size"]),
                digest=asset.get("digest"),
            )
        )
        if len(matched) == spec.release_count:
            break
    matched.reverse()
    return matched


def find_matching_asset(
    assets: list[dict[str, Any]],
    pattern: re.Pattern[str],
) -> dict[str, Any] | None:
    for asset in assets:
        name = str(asset.get("name", ""))
        if pattern.search(name):
            return asset
    return None


def fetch_recent_releases(spec: TargetSpec, token: str | None) -> list[ReleaseAsset]:
    collected: list[dict[str, Any]] = []
    for page in range(1, MAX_API_PAGES + 1):
        url = (
            f"https://api.github.com/repos/{spec.repo}/releases"
            f"?per_page={PER_PAGE}&page={page}"
        )
        req = urllib.request.Request(url, headers=github_headers(token))
        with urllib.request.urlopen(req, timeout=30) as resp:
            batch = json.loads(resp.read().decode("utf-8"))
        if not batch:
            break
        collected.extend(batch)
        matched = select_matching_releases(collected, spec)
        if len(matched) >= spec.release_count:
            return matched

    matched = select_matching_releases(collected, spec)
    if len(matched) < spec.release_count:
        raise RuntimeError(
            f"{spec.repo} matching /{spec.asset_pattern}/ only has {len(matched)} releases "
            f"(need {spec.release_count})"
        )
    return matched


def sha256_file(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(DOWNLOAD_CHUNK_BYTES):
            hasher.update(chunk)
    return hasher.hexdigest()


def download_asset(asset: ReleaseAsset, dest: Path, token: str | None) -> str:
    headers = {"User-Agent": USER_AGENT}
    if token:
        headers["Authorization"] = f"Bearer {token}"

    last_err: Exception | None = None
    for attempt in range(1, DOWNLOAD_RETRIES + 1):
        try:
            return _download_once(asset, dest, headers)
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as exc:
            last_err = exc
            if dest.exists():
                dest.unlink()
            if attempt < DOWNLOAD_RETRIES:
                time.sleep(attempt * 2)
    raise RuntimeError(f"failed to download {asset.download_url}: {last_err}")


def _download_once(asset: ReleaseAsset, dest: Path, headers: dict[str, str]) -> str:
    req = urllib.request.Request(asset.download_url, headers=headers)
    hasher = hashlib.sha256()
    written = 0
    with urllib.request.urlopen(req, timeout=180) as resp, dest.open("wb") as out:
        while chunk := resp.read(DOWNLOAD_CHUNK_BYTES):
            out.write(chunk)
            hasher.update(chunk)
            written += len(chunk)

    if asset.size > 0 and written != asset.size:
        raise ValueError(
            f"size mismatch downloading {asset.asset_name}: expected {asset.size}, got {written}"
        )
    actual_sha256 = hasher.hexdigest()
    if asset.digest and asset.digest.startswith("sha256:"):
        expected_digest = asset.digest.removeprefix("sha256:").lower()
        if actual_sha256 != expected_digest:
            raise ValueError(
                f"GitHub digest mismatch for {asset.asset_name}: "
                f"expected {expected_digest}, got {actual_sha256}"
            )
    return actual_sha256


def verify_hop_hashes(
    hops: list[dict[str, Any]],
    source_hashes: list[str],
) -> None:
    if len(hops) != len(source_hashes) - 1:
        raise ValueError(
            f"hop count mismatch: got {len(hops)}, expected {len(source_hashes) - 1}"
        )
    for idx, hop in enumerate(hops):
        expected_src = source_hashes[idx + 1]
        if not hop.get("sha256Verified") or not hop.get("sha512Verified"):
            raise ValueError(
                f"hash verification flag false for {hop.get('fromVersion')} -> {hop.get('toVersion')}"
            )
        if (
            hop.get("expectedSha256") != expected_src
            or hop.get("actualSha256") != expected_src
        ):
            raise ValueError(
                f"rebuilt SHA-256 mismatch for {hop.get('fromVersion')} -> {hop.get('toVersion')}: "
                f"source={expected_src}, actual={hop.get('actualSha256')}"
            )


def run_benchmark_binary(
    binary: Path,
    artifact_paths: list[Path],
    versions: list[str],
) -> dict[str, Any]:
    cmd = [str(binary)]
    for version in versions:
        cmd.extend(["--version", version])
    cmd.extend(str(p) for p in artifact_paths)
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(
            f"benchmark_update failed (exit {proc.returncode}): {proc.stderr.strip()}"
        )
    return json.loads(proc.stdout)


def run_target(spec: TargetSpec, opts: RunOptions) -> dict[str, Any]:
    started_at = datetime.now(UTC).isoformat()
    tmp_root = (
        opts.work_dir / spec.target_id
        if opts.work_dir
        else Path(tempfile.mkdtemp(prefix=f"bench-{spec.target_id}-"))
    )
    tmp_root.mkdir(parents=True, exist_ok=True)
    try:
        releases = fetch_recent_releases(spec, opts.token)
        artifact_paths: list[Path] = []
        source_hashes: list[str] = []
        release_meta: list[dict[str, Any]] = []
        for idx, asset in enumerate(releases, start=1):
            safe_name = f"rev{idx}-{Path(asset.asset_name).name}"
            dest = tmp_root / safe_name
            print(
                f"[{spec.target_id}] downloading rev{idx} {asset.tag} ({asset.asset_name})...",
                file=sys.stderr,
                flush=True,
            )
            digest_hex = download_asset(asset, dest, opts.token)
            artifact_paths.append(dest)
            source_hashes.append(digest_hex)
            release_meta.append(
                {
                    "tag": asset.tag,
                    "publishedAt": asset.published_at,
                    "assetName": asset.asset_name,
                    "size": asset.size,
                    "sha256": digest_hex,
                }
            )

        print(
            f"[{spec.target_id}] running chained E2E benchmark: "
            + " -> ".join(r.tag for r in releases),
            file=sys.stderr,
            flush=True,
        )
        bench_data = run_benchmark_binary(
            opts.binary,
            artifact_paths,
            [r.tag for r in releases],
        )
        verify_hop_hashes(bench_data["hops"], source_hashes)
        return {
            "status": "ok",
            "id": spec.target_id,
            "repo": spec.repo,
            "format": spec.format_label,
            "assetPattern": spec.asset_pattern,
            "startedAt": started_at,
            "releases": release_meta,
            "seedOldMs": bench_data["seedOldMs"],
            "hops": bench_data["hops"],
            "summary": bench_data["summary"],
        }
    except (
        RuntimeError,
        ValueError,
        OSError,
        urllib.error.URLError,
        json.JSONDecodeError,
        KeyError,
        subprocess.SubprocessError,
    ) as exc:
        print(f"[{spec.target_id}] ERROR: {exc}", file=sys.stderr, flush=True)
        return {
            "status": "error",
            "id": spec.target_id,
            "repo": spec.repo,
            "format": spec.format_label,
            "assetPattern": spec.asset_pattern,
            "startedAt": started_at,
            "error": str(exc),
        }
    finally:
        if not opts.keep_downloads:
            shutil.rmtree(tmp_root, ignore_errors=True)


def format_mib(num_bytes: int) -> str:
    return f"{num_bytes / (1024 * 1024):.2f} MiB"


def format_pct(value: float) -> str:
    return f"{value:.2f}%"


def render_markdown_report(reports: list[dict[str, Any]]) -> str:
    lines = [
        "# Real-World Release Differential Update Benchmark Report",
        "",
        f"Generated at: `{datetime.now(UTC).strftime('%Y-%m-%d %H:%M:%S UTC')}`",
        "",
        "## Summary",
        "",
        (
            "| Target | Repo | Format | Version Chain | Diff Hops | Total Target Size | "
            "Downloaded Payload | Wire + Blockmap | Payload Saved | Wire Saved | Hash Check |"
        ),
        "| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |",
    ]

    agg_total = 0
    agg_payload = 0
    agg_wire_plus_map = 0
    agg_diff_hops = 0
    agg_hops = 0
    all_ok = True

    for rep in reports:
        if rep.get("status") != "ok":
            all_ok = False
            lines.append(
                f"| `{rep['id']}` | `{rep['repo']}` | {rep.get('format', '-')} | "
                f"- | - | - | - | - | - | - | FAIL |"
            )
            continue

        summary = rep["summary"]
        tags = [r["tag"] for r in rep["releases"]]
        chain = " -> ".join(f"`{t}`" for t in tags)
        total_b = int(summary["totalArtifactBytes"])
        payload_b = int(summary["totalDownloadedPayloadBytes"])
        wire_plus_map = int(summary["totalHttpWireBytes"]) + int(
            summary["totalBlockmapBytes"]
        )
        agg_total += total_b
        agg_payload += payload_b
        agg_wire_plus_map += wire_plus_map
        agg_diff_hops += int(summary["differentialHops"])
        agg_hops += int(summary["hopCount"])
        hash_ok = bool(summary.get("allHashesVerified"))
        if not hash_ok:
            all_ok = False
        lines.append(
            f"| `{rep['id']}` | `{rep['repo']}` | {rep.get('format', '-')} | {chain} | "
            f"{summary['differentialHops']}/{summary['hopCount']} | {format_mib(total_b)} | "
            f"{format_mib(payload_b)} | {format_mib(wire_plus_map)} | "
            f"{format_pct(summary['overallSavedPercent'])} | "
            f"{format_pct(summary['overallWireSavedPercent'])} | "
            f"{'PASS' if hash_ok else 'FAIL'} |"
        )

    if agg_total > 0:
        agg_payload_saved = (1.0 - agg_payload / agg_total) * 100.0
        agg_wire_saved = (1.0 - agg_wire_plus_map / agg_total) * 100.0
        lines.append(
            f"| **Total** | **{len(reports)} targets** | - | - | "
            f"**{agg_diff_hops}/{agg_hops}** | **{format_mib(agg_total)}** | "
            f"**{format_mib(agg_payload)}** | **{format_mib(agg_wire_plus_map)}** | "
            f"**{format_pct(agg_payload_saved)}** | **{format_pct(agg_wire_saved)}** | "
            f"**{'PASS' if all_ok else 'FAIL'}** |"
        )

    lines.extend(["", "## Hop-by-Hop Breakdown", ""])
    for rep in reports:
        lines.append(f"### `{rep['id']}` — `{rep['repo']}` ({rep.get('format', '-')})")
        lines.append("")
        if rep.get("status") != "ok":
            lines.append(f"> Failed: `{rep.get('error', 'unknown error')}`")
            lines.append("")
            continue

        lines.append(
            "| Transition | Mode | Target Size | Reused | Downloaded | Wire + Blockmap | "
            "Requests | Payload Saved | Wire Saved | Wall Time | SHA-256 (Verified) |"
        )
        lines.append(
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
        )
        for hop in rep["hops"]:
            wire_total = int(hop["httpWireBytes"]) + int(hop["blockmapBytes"])
            sha_short = str(hop["actualSha256"])[:12]
            verified = hop["sha256Verified"] and hop["sha512Verified"]
            status_tag = "PASS" if verified else "FAIL"
            lines.append(
                f"| `{hop['fromVersion']}` -> `{hop['toVersion']}` | `{hop['mode']}` | "
                f"{format_mib(int(hop['totalBytes']))} | {format_mib(int(hop['reusedBytes']))} | "
                f"{format_mib(int(hop['downloadedPayloadBytes']))} | {format_mib(wire_total)} | "
                f"{hop['httpRequests']} | {format_pct(hop['savedPercent'])} | "
                f"{format_pct(hop['wireSavedPercent'])} | {hop['updateWallMs']} ms | "
                f"`{sha_short}...` ({status_tag}) |"
            )
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def all_reports_passed(reports: list[dict[str, Any]]) -> bool:
    if not reports:
        return False
    return all(
        rep.get("status") == "ok"
        and bool(rep.get("summary", {}).get("allHashesVerified"))
        for rep in reports
    )


def write_summary_outputs(
    reports: list[dict[str, Any]],
    output_md: Path | None,
    output_json: Path | None,
    step_summary: Path | None,
) -> bool:
    ordered = sorted(reports, key=lambda item: str(item.get("id", "")))
    markdown = render_markdown_report(ordered)
    if output_md:
        output_md.parent.mkdir(parents=True, exist_ok=True)
        output_md.write_text(markdown, encoding="utf-8")
    if output_json:
        output_json.parent.mkdir(parents=True, exist_ok=True)
        output_json.write_text(json.dumps(ordered, indent=2) + "\n", encoding="utf-8")
    if step_summary:
        with step_summary.open("a", encoding="utf-8") as handle:
            handle.write(markdown)
    print(markdown)
    return all_reports_passed(ordered)


def cmd_list_matrix(args: argparse.Namespace) -> int:
    targets = resolve_targets(args)
    matrix = [
        {
            "id": t.target_id,
            "repo": t.repo,
            "asset_pattern": t.asset_pattern,
            "format": t.format_label,
            "release_count": t.release_count,
            "include_prerelease": t.include_prerelease,
        }
        for t in targets
    ]
    print(json.dumps(matrix))
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    targets = resolve_targets(args)
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    opts = RunOptions(
        binary=args.binary,
        output_dir=args.output_dir,
        work_dir=args.work_dir,
        token=token,
        keep_downloads=args.keep_downloads,
    )

    reports: list[dict[str, Any]] = []
    for spec in targets:
        report = run_target(spec, opts)
        reports.append(report)
        out_file = args.output_dir / f"{spec.target_id}.json"
        out_file.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    passed = all_reports_passed(reports)
    if args.write_summary:
        step_summary = Path(args.step_summary) if args.step_summary else None
        passed = write_summary_outputs(
            reports,
            args.output_dir / "summary.md",
            args.output_dir / "summary.json",
            step_summary,
        )
    return 0 if passed else 1


def cmd_summarize(args: argparse.Namespace) -> int:
    json_files = sorted(
        p for p in args.input_dir.rglob("*.json") if p.name != "summary.json"
    )
    reports = [json.loads(p.read_text(encoding="utf-8")) for p in json_files]
    step_summary = Path(args.step_summary) if args.step_summary else None
    passed = write_summary_outputs(
        reports, args.output_md, args.output_json, step_summary
    )
    return 0 if passed else 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    for name in ("list-matrix", "run"):
        sub = subparsers.add_parser(name)
        sub.add_argument("--config", type=Path, default=Path("benchmarks/repos.json"))
        sub.add_argument(
            "--target", help="Run only one preset target ID (or ad-hoc ID)."
        )
        sub.add_argument("--repo", help="Ad-hoc GitHub owner/repo override.")
        sub.add_argument("--asset-pattern", help="Regex matching release asset name.")
        sub.add_argument("--format-label", help="Display label for ad-hoc format.")
        sub.add_argument(
            "--release-count", type=int, help="Number of recent releases (default 4)."
        )
        sub.add_argument("--include-prerelease", action="store_true")
        if name == "run":
            sub.add_argument(
                "--binary",
                type=Path,
                default=Path("target/release/examples/benchmark_update"),
            )
            sub.add_argument(
                "--output-dir", type=Path, default=Path("benchmark-results")
            )
            sub.add_argument("--work-dir", type=Path)
            sub.add_argument("--keep-downloads", action="store_true")
            sub.add_argument("--write-summary", action="store_true")
            sub.add_argument(
                "--step-summary", help="Optional path to GITHUB_STEP_SUMMARY."
            )

    summ = subparsers.add_parser("summarize")
    summ.add_argument("--input-dir", type=Path, required=True)
    summ.add_argument("--output-md", type=Path)
    summ.add_argument("--output-json", type=Path)
    summ.add_argument("--step-summary", help="Optional path to GITHUB_STEP_SUMMARY.")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.command == "list-matrix":
        return cmd_list_matrix(args)
    if args.command == "run":
        return cmd_run(args)
    if args.command == "summarize":
        return cmd_summarize(args)
    raise ValueError(f"unsupported command: {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
