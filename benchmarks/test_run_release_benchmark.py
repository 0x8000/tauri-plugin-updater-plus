"""Unit tests for benchmarks/run_release_benchmark.py."""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Self
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_release_benchmark as bench


class _FakeHttpResponse(io.BytesIO):
    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class ReleaseSelectionTests(unittest.TestCase):
    def test_selects_latest_four_matching_releases_oldest_to_newest(self) -> None:
        spec = bench.TargetSpec(
            target_id="paseo-appimage",
            repo="getpaseo/paseo",
            asset_pattern=r"^Paseo-x86_64\.AppImage$",
            format_label="AppImage",
            release_count=4,
        )
        releases = [
            {
                "tag_name": "v0.17.6-draft",
                "draft": True,
                "prerelease": False,
                "assets": [
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/draft",
                        "size": 100,
                    }
                ],
            },
            {
                "tag_name": "v0.17.5-rc.1",
                "draft": False,
                "prerelease": True,
                "assets": [
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/pre",
                        "size": 100,
                    }
                ],
            },
            {
                "tag_name": "v0.17.4",
                "published_at": "2026-11-24T00:00:00Z",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "latest-linux.yml",
                        "browser_download_url": "https://example.invalid/yml",
                        "size": 10,
                    },
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/4",
                        "size": 104,
                        "digest": "sha256:abcd",
                    },
                ],
            },
            {
                "tag_name": "v0.17.3-hotfix-mac-only",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "Paseo-0.17.3-arm64.dmg",
                        "browser_download_url": "https://example.invalid/mac",
                        "size": 200,
                    }
                ],
            },
            {
                "tag_name": "v0.17.3",
                "published_at": "2026-11-23T00:00:00Z",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/3",
                        "size": 103,
                    }
                ],
            },
            {
                "tag_name": "v0.17.2",
                "published_at": "2026-11-22T00:00:00Z",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/2",
                        "size": 102,
                    }
                ],
            },
            {
                "tag_name": "v0.17.1",
                "published_at": "2026-11-21T00:00:00Z",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/1",
                        "size": 101,
                    }
                ],
            },
            {
                "tag_name": "v0.17.0",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "Paseo-x86_64.AppImage",
                        "browser_download_url": "https://example.invalid/0",
                        "size": 100,
                    }
                ],
            },
        ]

        selected = bench.select_matching_releases(releases, spec)
        self.assertEqual(
            selected,
            [
                bench.ReleaseAsset(
                    "v0.17.1",
                    "2026-11-21T00:00:00Z",
                    "Paseo-x86_64.AppImage",
                    "https://example.invalid/1",
                    101,
                    None,
                ),
                bench.ReleaseAsset(
                    "v0.17.2",
                    "2026-11-22T00:00:00Z",
                    "Paseo-x86_64.AppImage",
                    "https://example.invalid/2",
                    102,
                    None,
                ),
                bench.ReleaseAsset(
                    "v0.17.3",
                    "2026-11-23T00:00:00Z",
                    "Paseo-x86_64.AppImage",
                    "https://example.invalid/3",
                    103,
                    None,
                ),
                bench.ReleaseAsset(
                    "v0.17.4",
                    "2026-11-24T00:00:00Z",
                    "Paseo-x86_64.AppImage",
                    "https://example.invalid/4",
                    104,
                    "sha256:abcd",
                ),
            ],
        )

    def test_selects_prereleases_when_enabled(self) -> None:
        spec = bench.TargetSpec(
            target_id="nightly",
            repo="acme/app",
            asset_pattern=r"^app\.zip$",
            format_label="ZIP",
            release_count=2,
            include_prerelease=True,
        )
        releases = [
            {
                "tag_name": "v2.0.0-rc.1",
                "draft": False,
                "prerelease": True,
                "assets": [
                    {
                        "name": "app.zip",
                        "browser_download_url": "https://example.invalid/rc1",
                        "size": 50,
                    }
                ],
            },
            {
                "tag_name": "v1.0.0",
                "draft": False,
                "prerelease": False,
                "assets": [
                    {
                        "name": "app.zip",
                        "browser_download_url": "https://example.invalid/v1",
                        "size": 40,
                    }
                ],
            },
        ]
        selected = bench.select_matching_releases(releases, spec)
        self.assertEqual([item.tag for item in selected], ["v1.0.0", "v2.0.0-rc.1"])


class HashVerificationTests(unittest.TestCase):
    def test_verify_hop_hashes_passes_for_matching_chain(self) -> None:
        source_hashes = ["aa" * 32, "bb" * 32, "cc" * 32, "dd" * 32]
        hops = [
            {
                "fromVersion": "v1",
                "toVersion": "v2",
                "expectedSha256": source_hashes[1],
                "actualSha256": source_hashes[1],
                "sha256Verified": True,
                "sha512Verified": True,
            },
            {
                "fromVersion": "v2",
                "toVersion": "v3",
                "expectedSha256": source_hashes[2],
                "actualSha256": source_hashes[2],
                "sha256Verified": True,
                "sha512Verified": True,
            },
            {
                "fromVersion": "v3",
                "toVersion": "v4",
                "expectedSha256": source_hashes[3],
                "actualSha256": source_hashes[3],
                "sha256Verified": True,
                "sha512Verified": True,
            },
        ]
        bench.verify_hop_hashes(hops, source_hashes)

    def test_verify_hop_hashes_rejects_mismatched_rebuilt_digest(self) -> None:
        source_hashes = ["aa" * 32, "bb" * 32]
        hops = [
            {
                "fromVersion": "v1",
                "toVersion": "v2",
                "expectedSha256": source_hashes[1],
                "actualSha256": "ff" * 32,
                "sha256Verified": True,
                "sha512Verified": True,
            }
        ]
        with self.assertRaisesRegex(
            ValueError, r"rebuilt SHA-256 mismatch for v1 -> v2"
        ):
            bench.verify_hop_hashes(hops, source_hashes)

    def test_verify_hop_hashes_rejects_expected_hash_diverging_from_source(
        self,
    ) -> None:
        source_hashes = ["aa" * 32, "bb" * 32]
        wrong_hash = "ee" * 32
        hops = [
            {
                "fromVersion": "v1",
                "toVersion": "v2",
                "expectedSha256": wrong_hash,
                "actualSha256": wrong_hash,
                "sha256Verified": True,
                "sha512Verified": True,
            }
        ]
        with self.assertRaisesRegex(
            ValueError, r"rebuilt SHA-256 mismatch for v1 -> v2"
        ):
            bench.verify_hop_hashes(hops, source_hashes)

    def test_verify_hop_hashes_rejects_unverified_flags(self) -> None:
        source_hashes = ["aa" * 32, "bb" * 32]
        for sha256_ok, sha512_ok in ((True, False), (False, True)):
            hops = [
                {
                    "fromVersion": "v1",
                    "toVersion": "v2",
                    "expectedSha256": source_hashes[1],
                    "actualSha256": source_hashes[1],
                    "sha256Verified": sha256_ok,
                    "sha512Verified": sha512_ok,
                }
            ]
            with self.assertRaisesRegex(
                ValueError, r"hash verification flag false for v1 -> v2"
            ):
                bench.verify_hop_hashes(hops, source_hashes)

    def test_verify_hop_hashes_rejects_hop_count_mismatch(self) -> None:
        source_hashes = ["aa" * 32, "bb" * 32, "cc" * 32]
        hops = [
            {
                "fromVersion": "v1",
                "toVersion": "v2",
                "expectedSha256": source_hashes[1],
                "actualSha256": source_hashes[1],
                "sha256Verified": True,
                "sha512Verified": True,
            }
        ]
        with self.assertRaisesRegex(
            ValueError, r"hop count mismatch: got 1, expected 2"
        ):
            bench.verify_hop_hashes(hops, source_hashes)


class DownloadAndConfigTests(unittest.TestCase):
    def test_preset_config_loads_filters_and_rejects_invalid_counts(self) -> None:
        fixture = {
            "defaultReleaseCount": 4,
            "targets": [
                {
                    "id": "alpha-appimage",
                    "repo": "acme/alpha",
                    "assetPattern": r"^Alpha-x86_64\.AppImage$",
                    "format": "AppImage",
                    "includePrerelease": False,
                },
                {
                    "id": "beta-zip",
                    "repo": "acme/beta",
                    "assetPattern": r"^Beta-.*\.zip$",
                    "format": "ZIP",
                    "releaseCount": 3,
                    "includePrerelease": True,
                },
            ],
        }
        with tempfile.TemporaryDirectory() as tmp:
            config_path = Path(tmp) / "repos.json"
            config_path.write_text(json.dumps(fixture), encoding="utf-8")
            parser = bench.build_parser()

            args = parser.parse_args(["list-matrix", "--config", str(config_path)])
            self.assertEqual(
                bench.resolve_targets(args),
                [
                    bench.TargetSpec(
                        "alpha-appimage",
                        "acme/alpha",
                        r"^Alpha-x86_64\.AppImage$",
                        "AppImage",
                        4,
                        False,
                    ),
                    bench.TargetSpec(
                        "beta-zip",
                        "acme/beta",
                        r"^Beta-.*\.zip$",
                        "ZIP",
                        3,
                        True,
                    ),
                ],
            )

            filtered_args = parser.parse_args(
                [
                    "list-matrix",
                    "--config",
                    str(config_path),
                    "--target",
                    "beta-zip",
                    "--release-count",
                    "5",
                ]
            )
            self.assertEqual(
                bench.resolve_targets(filtered_args),
                [
                    bench.TargetSpec(
                        "beta-zip", "acme/beta", r"^Beta-.*\.zip$", "ZIP", 5, True
                    )
                ],
            )

            zero_count_args = parser.parse_args(
                ["list-matrix", "--config", str(config_path), "--release-count", "0"]
            )
            with self.assertRaisesRegex(
                ValueError, r"requires at least 2 releases, got 0"
            ):
                bench.resolve_targets(zero_count_args)

            unknown_args = parser.parse_args(
                ["list-matrix", "--config", str(config_path), "--target", "missing"]
            )
            with self.assertRaisesRegex(ValueError, r"unknown target 'missing'"):
                bench.resolve_targets(unknown_args)

    def test_adhoc_repo_resolution_and_validation(self) -> None:
        parser = bench.build_parser()
        args = parser.parse_args(
            [
                "list-matrix",
                "--repo",
                "getpaseo/paseo",
                "--asset-pattern",
                r"^Paseo-x86_64\.AppImage$",
                "--release-count",
                "4",
            ]
        )
        self.assertEqual(
            bench.resolve_targets(args),
            [
                bench.TargetSpec(
                    "getpaseo-paseo",
                    "getpaseo/paseo",
                    r"^Paseo-x86_64\.AppImage$",
                    "custom",
                    4,
                    False,
                )
            ],
        )

        missing_pattern = parser.parse_args(["list-matrix", "--repo", "getpaseo/paseo"])
        with self.assertRaisesRegex(ValueError, r"--asset-pattern is required"):
            bench.resolve_targets(missing_pattern)

        bad_count = parser.parse_args(
            [
                "list-matrix",
                "--repo",
                "getpaseo/paseo",
                "--asset-pattern",
                "x",
                "--release-count",
                "1",
            ]
        )
        with self.assertRaisesRegex(ValueError, r"requires at least 2 releases, got 1"):
            bench.resolve_targets(bad_count)

    def test_repo_presets_file_has_valid_schema(self) -> None:
        config_path = Path(__file__).resolve().parent / "repos.json"
        targets = bench.load_preset_targets(config_path)
        self.assertTrue(targets)
        self.assertEqual(len({t.target_id for t in targets}), len(targets))
        for target in targets:
            self.assertRegex(target.repo, r"^[^/]+/[^/]+$")
            self.assertGreaterEqual(target.release_count, 2)
            re.compile(target.asset_pattern)

    def test_download_asset_verifies_digest_and_deletes_corrupt_file(self) -> None:
        payload = b"valid-release-bytes"
        expected_sha = hashlib.sha256(payload).hexdigest()
        good_asset = bench.ReleaseAsset(
            tag="v1.0.0",
            published_at="",
            asset_name="app.AppImage",
            download_url="https://example.invalid/app.AppImage",
            size=len(payload),
            digest=f"sha256:{expected_sha}",
        )
        bad_asset = bench.ReleaseAsset(
            tag="v1.0.0",
            published_at="",
            asset_name="app.AppImage",
            download_url="https://example.invalid/app.AppImage",
            size=len(payload),
            digest="sha256:" + ("00" * 32),
        )

        with tempfile.TemporaryDirectory() as tmp:
            good_dest = Path(tmp) / "good.AppImage"
            with mock.patch.object(
                bench.urllib.request,
                "urlopen",
                side_effect=lambda *_a, **_k: _FakeHttpResponse(payload),
            ):
                self.assertEqual(
                    bench.download_asset(good_asset, good_dest, None), expected_sha
                )
            self.assertEqual(good_dest.read_bytes(), payload)

            bad_dest = Path(tmp) / "bad.AppImage"
            with (
                mock.patch.object(
                    bench.urllib.request,
                    "urlopen",
                    side_effect=lambda *_a, **_k: _FakeHttpResponse(payload),
                ),
                mock.patch.object(bench.time, "sleep"),
                self.assertRaisesRegex(RuntimeError, r"GitHub digest mismatch"),
            ):
                bench.download_asset(bad_asset, bad_dest, None)
            self.assertFalse(bad_dest.exists())


class SummaryReportTests(unittest.TestCase):
    def _sample_ok_report(self) -> dict[str, object]:
        return {
            "status": "ok",
            "id": "demo-app",
            "repo": "acme/demo",
            "format": "AppImage",
            "releases": [{"tag": "v1.0.0"}, {"tag": "v1.0.1"}],
            "hops": [
                {
                    "fromVersion": "v1.0.0",
                    "toVersion": "v1.0.1",
                    "mode": "differential",
                    "totalBytes": 100 * 1024 * 1024,
                    "reusedBytes": 90 * 1024 * 1024,
                    "downloadedPayloadBytes": 10 * 1024 * 1024,
                    "httpWireBytes": 10 * 1024 * 1024 + 4096,
                    "blockmapBytes": 100 * 1024,
                    "httpRequests": 3,
                    "savedPercent": 90.0,
                    "wireSavedPercent": 89.9,
                    "updateWallMs": 450,
                    "actualSha256": "1234567890abcdef" * 4,
                    "sha256Verified": True,
                    "sha512Verified": True,
                }
            ],
            "summary": {
                "hopCount": 1,
                "differentialHops": 1,
                "fullHops": 0,
                "totalArtifactBytes": 100 * 1024 * 1024,
                "totalReusedBytes": 90 * 1024 * 1024,
                "totalDownloadedPayloadBytes": 10 * 1024 * 1024,
                "totalHttpWireBytes": 10 * 1024 * 1024 + 4096,
                "totalBlockmapBytes": 100 * 1024,
                "totalHttpRequests": 3,
                "overallSavedPercent": 90.0,
                "overallWireSavedPercent": 89.9,
                "allHashesVerified": True,
            },
        }

    def test_summarize_writes_markdown_and_json_for_passing_reports(self) -> None:
        ok_report = self._sample_ok_report()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            in_dir = root / "in"
            in_dir.mkdir()
            (in_dir / "demo-app.json").write_text(
                json.dumps(ok_report), encoding="utf-8"
            )
            md_path = root / "out" / "summary.md"
            json_path = root / "out" / "summary.json"
            with contextlib.redirect_stdout(io.StringIO()):
                exit_ok = bench.main(
                    [
                        "summarize",
                        "--input-dir",
                        str(in_dir),
                        "--output-md",
                        str(md_path),
                        "--output-json",
                        str(json_path),
                    ]
                )
            self.assertEqual(exit_ok, 0)
            self.assertEqual(
                json.loads(json_path.read_text(encoding="utf-8")), [ok_report]
            )
            md_text = md_path.read_text(encoding="utf-8")
            self.assertIn(
                "| `demo-app` | `acme/demo` | AppImage | `v1.0.0` -> `v1.0.1` | 1/1 | "
                "100.00 MiB | 10.00 MiB | 10.10 MiB | 90.00% | 89.90% | PASS |",
                md_text,
            )
            self.assertIn(
                "| **Total** | **1 targets** | - | - | **1/1** | **100.00 MiB** | "
                "**10.00 MiB** | **10.10 MiB** | **90.00%** | **89.90%** | **PASS** |",
                md_text,
            )

    def test_summarize_fails_on_error_unverified_or_empty_inputs_and_appends_step_summary(
        self,
    ) -> None:
        ok_report = self._sample_ok_report()
        err_report = {
            "status": "error",
            "id": "broken-app",
            "repo": "acme/broken",
            "format": "ZIP",
            "error": "hash verification failed",
        }
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            empty_dir = root / "empty"
            empty_dir.mkdir()
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    bench.main(["summarize", "--input-dir", str(empty_dir)]), 1
                )

            mixed_dir = root / "mixed"
            mixed_dir.mkdir()
            (mixed_dir / "demo-app.json").write_text(
                json.dumps(ok_report), encoding="utf-8"
            )
            (mixed_dir / "broken-app.json").write_text(
                json.dumps(err_report), encoding="utf-8"
            )
            step_summary = root / "step.md"
            step_summary.write_text("header\n", encoding="utf-8")
            md_path = root / "summary.md"
            json_path = root / "summary.json"
            with contextlib.redirect_stdout(io.StringIO()):
                exit_fail = bench.main(
                    [
                        "summarize",
                        "--input-dir",
                        str(mixed_dir),
                        "--output-md",
                        str(md_path),
                        "--output-json",
                        str(json_path),
                        "--step-summary",
                        str(step_summary),
                    ]
                )
            self.assertEqual(exit_fail, 1)
            ordered_ids = [
                item["id"] for item in json.loads(json_path.read_text(encoding="utf-8"))
            ]
            self.assertEqual(ordered_ids, ["broken-app", "demo-app"])
            fail_md = md_path.read_text(encoding="utf-8")
            self.assertIn(
                "| `broken-app` | `acme/broken` | ZIP | - | - | - | - | - | - | - | FAIL |",
                fail_md,
            )
            self.assertIn("**FAIL**", fail_md)
            self.assertIn("> Failed: `hash verification failed`", fail_md)
            self.assertTrue(
                step_summary.read_text(encoding="utf-8").startswith("header\n")
            )


if __name__ == "__main__":
    unittest.main()
