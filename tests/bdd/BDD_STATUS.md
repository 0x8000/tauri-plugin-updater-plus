# BDD Tracker

Last updated: 2026-11-26

Latest run: Full suite (`just test-bdd`) — all 33 scenarios passed on Linux.

Stable key: **feature path + scenario name**. Line numbers are omitted because
they move whenever a feature is edited; every scenario name is unique across the
suite.

Legend: `[x]` passed · `[ ]` failing · `[?]` tagged but not run this round · `-`
no tag for that platform (or explicitly skipped)

Scenarios: 33 total · 0 platform-tagged · 33 platform-neutral

## Platform-neutral scenarios (default suite)

| #  | Feature                                          | Scenario                                                                                                                  | Status |
| -- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------- | ------ |
| 1  | `features/blockmap_cli.feature`                  | Generate a v2 blockmap and Tauri JSON manifest for a release artifact                                                     | [x]    |
| 2  | `features/blockmap_cli.feature`                  | Generate an Electron YAML manifest with multiline release notes and compute blockmap diff                                 | [x]    |
| 3  | `features/blockmap_cli.feature`                  | Pack a macOS app bundle into a blockmap-friendly zip preserving root folder permissions and symlinks                      | [x]    |
| 4  | `features/nginx_differential_update.feature`     | Running AppImage serves as the differential base when the update cache is empty                                           | [x]    |
| 5  | `features/nginx_differential_update.feature`     | Sequential updates chain cached packages across three versions via Nginx multipart ranges                                 | [x]    |
| 6  | `features/nginx_differential_update.feature`     | Corrupted cached local blocks are re-fetched over HTTP Range without full fallback                                        | [x]    |
| 7  | `features/nginx_differential_update.feature`     | Delta exceeding fullDownloadThreshold falls back cleanly to a full Nginx download                                         | [x]    |
| 8  | `features/nginx_differential_update.feature`     | Stale remote blockmap triggers full fallback and regenerates clean cache for the next differential update                 | [x]    |
| 9  | `features/cdn_hosting_and_fallback.feature`      | Cross-origin 302 CDN redirects strip credentials reuse signed URLs and refresh expired tokens                             | [x]    |
| 10 | `features/cdn_hosting_and_fallback.feature`      | Concurrent single-range workers hitting an expired 302 CDN token serialize refresh without thundering herd                | [x]    |
| 11 | `features/cdn_hosting_and_fallback.feature`      | Object store returning 200 OK on multi-range degrades to concurrent single-range differential transfer                    | [x]    |
| 12 | `features/cdn_hosting_and_fallback.feature`      | Server advertising Accept-Ranges none reuses the initial 200 OK stream for full fallback                                  | [x]    |
| 13 | `features/cdn_hosting_and_fallback.feature`      | Failing primary endpoint fails over to a healthy mirror serving Electron YAML with multiline notes                        | [x]    |
| 14 | `features/security_verification.feature`         | Valid minisign signature with matching signed version verifies and installs the AppImage                                  | [x]    |
| 15 | `features/security_verification.feature`         | Replaying a valid minisign signature from an older version is rejected with SignedVersionMismatch                         | [x]    |
| 16 | `features/security_verification.feature`         | Tampered remote artifact fails SHA-512 verification while preserving cached base and installed AppImage                   | [x]    |
| 17 | `features/security_verification.feature`         | Manifest omitting both sha512 and configured pubkey is rejected before downloading                                        | [x]    |
| 18 | `features/security_verification.feature`         | Oversized gzip bomb blockmap is rejected safely and falls back to verified full download                                  | [x]    |
| 19 | `features/security_verification.feature`         | Signature omitting version comment is rejected with MissingSignedVersion when requireSignedVersion is enabled             | [x]    |
| 20 | `features/security_verification.feature`         | HTTP error messages redact presigned query tokens from failing artifact URLs                                              | [x]    |
| 21 | `features/mqtt_transport.feature`                | MQTT Pull provider performs differential update over a real MQTT broker with zero HTTP traffic                            | [x]    |
| 22 | `features/mqtt_transport.feature`                | MQTT Push receiver reconstructs via DiffSession with out-of-order overlapping and duplicate QoS-1 chunks                  | [x]    |
| 23 | `features/mqtt_transport.feature`                | MQTT Push receiver rejects incomplete chunk stream with IncompleteTransfer while preserving cache and AppImage            | [x]    |
| 24 | `features/mqtt_transport.feature`                | Cross-protocol FailoverProvider falls over from an unreachable MQTT broker to a healthy Nginx HTTP mirror                 | [x]    |
| 25 | `features/http_providers_and_middleware.feature` | GitHubProvider downloads a private release via Releases API and strips credentials on cross-origin CDN redirect           | [x]    |
| 26 | `features/http_providers_and_middleware.feature` | GitHubProvider never leaks its bearer token when a manifest points to a third-party external host                         | [x]    |
| 27 | `features/http_providers_and_middleware.feature` | S3Provider authenticates manifest blockmap and single-range 206 transfers with verified AWS SigV4 and STS token           | [x]    |
| 28 | `features/http_providers_and_middleware.feature` | S3Provider falls back to verified full download when private bucket returns 403 for missing blockmap                      | [x]    |
| 29 | `features/http_providers_and_middleware.feature` | Custom HTTP retry middleware recovers from transient 503s while per-hop authorizer strips custom X-API-Key across origins | [x]    |
| 30 | `features/http_providers_and_middleware.feature` | Missing remote blockmap 404 cleanly falls back to verified full download                                                  | [x]    |
| 31 | `features/http_providers_and_middleware.feature` | Forced differential disabled skips blockmap fetch and downloads full artifact                                             | [x]    |
| 32 | `features/http_providers_and_middleware.feature` | Up-to-date check returns no update and allowDowngrades controls rollback behavior                                         | [x]    |
| 33 | `features/http_providers_and_middleware.feature` | Multi-batch multipart ranges and maxRangeGap coalescing minimize HTTP requests across scattered deltas                    | [x]    |
