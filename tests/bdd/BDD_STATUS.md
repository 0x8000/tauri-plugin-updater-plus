# BDD Tracker

Last updated: 2026-11-24

Latest run: Full suite (`just test-bdd`) — all 16 scenarios passed on Linux.

Stable key: **feature path + scenario name**. Line numbers are omitted because
they move whenever a feature is edited; every scenario name is unique across the
suite.

Legend: `[x]` passed · `[ ]` failing · `[?]` tagged but not run this round · `-`
no tag for that platform (or explicitly skipped)

Scenarios: 16 total · 0 platform-tagged · 16 platform-neutral

## Platform-neutral scenarios (default suite)

| #  | Feature                                      | Scenario                                                                                                | Status |
| -- | -------------------------------------------- | ------------------------------------------------------------------------------------------------------- | ------ |
| 1  | `features/blockmap_cli.feature`              | Generate a v2 blockmap and Tauri JSON manifest for a release artifact                                   | [x]    |
| 2  | `features/blockmap_cli.feature`              | Generate an Electron YAML manifest with multiline release notes and compute blockmap diff               | [x]    |
| 3  | `features/blockmap_cli.feature`              | Pack a macOS app bundle into a blockmap-friendly zip preserving root folder permissions and symlinks    | [x]    |
| 4  | `features/nginx_differential_update.feature` | Running AppImage serves as the differential base when the update cache is empty                         | [x]    |
| 5  | `features/nginx_differential_update.feature` | Sequential updates chain cached packages across three versions via Nginx multipart ranges               | [x]    |
| 6  | `features/nginx_differential_update.feature` | Corrupted cached local blocks are re-fetched over HTTP Range without full fallback                      | [x]    |
| 7  | `features/nginx_differential_update.feature` | Delta exceeding fullDownloadThreshold falls back cleanly to a full Nginx download                       | [x]    |
| 8  | `features/cdn_hosting_and_fallback.feature`  | Cross-origin 302 CDN redirects strip credentials reuse signed URLs and refresh expired tokens           | [x]    |
| 9  | `features/cdn_hosting_and_fallback.feature`  | Object store returning 200 OK on multi-range degrades to concurrent single-range differential transfer  | [x]    |
| 10 | `features/cdn_hosting_and_fallback.feature`  | Server advertising Accept-Ranges none reuses the initial 200 OK stream for full fallback                | [x]    |
| 11 | `features/cdn_hosting_and_fallback.feature`  | Failing primary endpoint fails over to a healthy mirror serving Electron YAML with multiline notes      | [x]    |
| 12 | `features/security_verification.feature`     | Valid minisign signature with matching signed version verifies and installs the AppImage                | [x]    |
| 13 | `features/security_verification.feature`     | Replaying a valid minisign signature from an older version is rejected with SignedVersionMismatch       | [x]    |
| 14 | `features/security_verification.feature`     | Tampered remote artifact fails SHA-512 verification while preserving cached base and installed AppImage | [x]    |
| 15 | `features/security_verification.feature`     | Manifest omitting both sha512 and configured pubkey is rejected before downloading                      | [x]    |
| 16 | `features/security_verification.feature`     | Oversized gzip bomb blockmap is rejected safely and falls back to verified full download                | [x]    |
