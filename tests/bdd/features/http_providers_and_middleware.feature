Feature: Built-in HTTP providers middleware customization and range coalescing edge cases
  As a developer hosting releases on GitHub S3 or custom authenticated HTTP servers
  I want composable HTTP providers SigV4 signing retry middleware and edge-case fallbacks
  So that authentication retries and bandwidth optimizations work without rewriting download logic

  Background:
    Given a base release artifact "0.1.0" of size 2000000 bytes
    And a delta release artifact "0.2.0" derived from "0.1.0" at offsets "180000,720000,1260000,1740000"
    And an installed AppImage at version "0.1.0" with seeded cache

  Scenario: GitHubProvider downloads a private release via Releases API and strips credentials on cross-origin CDN redirect
    Given a running CDN release gateway "gh" in "github_private" mode
    And CDN gateway "gh" publishes private GitHub release "0.2.0" with token "ghp_bdd_secret"
    And the updater uses GitHubProvider against "gh" with token "ghp_bdd_secret"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 70.0 percent of package bytes
    And CDN gateway "gh" recorded 2 redirects 0 leaked credentials and at least 4 single-range 206 responses
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: GitHubProvider never leaks its bearer token when a manifest points to a third-party external host
    Given a running CDN release gateway "gh" in "github_private" mode
    And CDN gateway "gh" publishes private GitHub release "0.2.0" with token "ghp_bdd_secret" pointing to external CDN
    And the updater uses GitHubProvider against "gh" with token "ghp_bdd_secret"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 70.0 percent of package bytes
    And CDN gateway "gh" recorded 0 redirects 0 leaked credentials and at least 4 single-range 206 responses
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: S3Provider authenticates manifest blockmap and single-range 206 transfers with verified AWS SigV4 and STS token
    Given a running CDN release gateway "s3" in "s3_sigv4" mode
    And CDN gateway "s3" enforces S3 SigV4 in region "us-west-2" with STS token "BDD_STS_TOKEN_V1"
    And CDN gateway "s3" publishes artifact "0.2.0" as "latest.json" in "tauri" format with notes "Private S3 SigV4 release"
    And the updater uses S3Provider in region "us-west-2" with STS token "BDD_STS_TOKEN_V1" for "cdn:s3/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 70.0 percent of package bytes
    And CDN gateway "s3" verified at least 6 SigV4 requests and rejected 0
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: S3Provider falls back to verified full download when private bucket returns 403 for missing blockmap
    Given a running CDN release gateway "s3" in "s3_sigv4" mode
    And CDN gateway "s3" enforces S3 SigV4 in region "us-west-2" with STS token "BDD_STS_TOKEN_V1"
    And CDN gateway "s3" returns 403 for missing blockmaps
    And CDN gateway "s3" publishes artifact "0.2.0" as "latest.json" in "tauri" format with notes "Private S3 missing blockmap"
    And CDN gateway "s3" removes the blockmap for "0.2.0"
    And the updater uses S3Provider in region "us-west-2" with STS token "BDD_STS_TOKEN_V1" for "cdn:s3/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "full" mode to version "0.2.0"
    And the fallback reason contains "403"
    And CDN gateway "s3" verified at least 3 SigV4 full-fallback requests and rejected 0
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Custom HTTP retry middleware recovers from transient 503s while per-hop authorizer strips custom X-API-Key across origins
    Given a running CDN release gateway "custom" in "retry_503_and_api_key" mode
    And CDN gateway "custom" requires X-API-Key "corp-secret-999" and fails first 2 requests with 503
    And CDN gateway "custom" publishes artifact "0.2.0" as "latest.json" in "tauri" format with notes "Retry middleware release"
    And the updater uses HTTP retry middleware with maxRetries 3 and X-API-Key "corp-secret-999" for "cdn:custom/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 70.0 percent of package bytes
    And CDN gateway "custom" recorded 2 transient 503 responses and 0 leaked credentials
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Missing remote blockmap 404 cleanly falls back to verified full download
    Given a running Nginx release server "primary"
    And Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And Nginx server "primary" removes the blockmap for "0.2.0"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "full" mode to version "0.2.0"
    And the fallback reason contains "404"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Forced differential disabled skips blockmap fetch and downloads full artifact
    Given a running Nginx release server "primary"
    And Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the updater configures differential to "false"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "full" mode to version "0.2.0"
    And the fallback reason contains "differential updates disabled"
    And Nginx server "primary" recorded 0 blockmap requests for "0.2.0"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Up-to-date check returns no update and allowDowngrades controls rollback behavior
    Given a running Nginx release server "primary"
    And Nginx server "primary" publishes artifact "0.1.0" as "latest.json" in "tauri" format
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "check"
    Then the update check reports no update available
    When Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    When Nginx server "primary" publishes artifact "0.1.0" as "latest.json" in "tauri" format
    And the application executes "check"
    Then the update check reports no update available
    When the updater configures allowDowngrades to "true"
    And the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"

  Scenario: Multi-batch multipart ranges and maxRangeGap coalescing minimize HTTP requests across scattered deltas
    Given a delta release artifact "0.3.0" derived from "0.1.0" at offsets "100000,260000,900000,1060000,1600000"
    And a running Nginx release server "primary"
    And Nginx server "primary" publishes artifact "0.3.0" as "latest.json" in "tauri" format
    And the updater configures maxRangesPerRequest to 2
    And the updater configures maxRangeGap to 200000
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.3.0"
    And the differential transfer completed in exact 2 HTTP requests
    And Nginx server "primary" served at least 1 multipart 206 response for "0.3.0"
    And the installed AppImage matches artifact "0.3.0" with mode "0750"
