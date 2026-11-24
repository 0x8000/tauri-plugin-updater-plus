Feature: Object-store single-range degradation 302 CDN redirects and endpoint failover
  As an application hosted on GitHub Releases S3 or R2
  I want signed CDN redirects single-range degradation and mirror failover handled transparently
  So that differential updates stay fast and credentials never leak across origins

  Background:
    Given a base release artifact "0.1.0" of size 2400000 bytes
    And a delta release artifact "0.2.0" derived from "0.1.0" at offsets "150000,750000,1350000,1950000"
    And an installed AppImage at version "0.1.0" with seeded cache

  Scenario: Cross-origin 302 CDN redirects strip credentials reuse signed URLs and refresh expired tokens
    Given a running CDN release gateway "origin" in "redirect" mode
    And CDN gateway "origin" publishes artifact "0.2.0" as "latest.json" in "tauri" format with notes "Signed CDN release"
    And the updater configures useMultipleRangeRequest to "false"
    And the updater configures maxConcurrentRanges to 1
    And the updater sends Authorization "Bearer gh-token-secret" and Cookie "session=gh-cookie"
    And the updater endpoints are "cdn:origin/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 75.0 percent of package bytes
    And CDN gateway "origin" recorded 2 redirects 0 leaked credentials and at least 4 single-range 206 responses
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Object store returning 200 OK on multi-range degrades to concurrent single-range differential transfer
    Given a running CDN release gateway "origin" in "s3_single_only_200_on_multi" mode
    And CDN gateway "origin" publishes artifact "0.2.0" as "latest.json" in "tauri" format with notes "S3 single-range degradation"
    And the updater endpoints are "cdn:origin/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 75.0 percent of package bytes
    And CDN gateway "origin" recorded 0 redirects 0 leaked credentials and at least 4 single-range 206 responses
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Server advertising Accept-Ranges none reuses the initial 200 OK stream for full fallback
    Given a running CDN release gateway "origin" in "accept_ranges_none" mode
    And CDN gateway "origin" publishes artifact "0.2.0" as "latest.json" in "tauri" format with notes "No range support"
    And the updater endpoints are "cdn:origin/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "full" mode to version "0.2.0"
    And the fallback reason contains "200 OK"
    And CDN gateway "origin" recorded 3 total origin requests
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Failing primary endpoint fails over to a healthy mirror serving Electron YAML with multiline notes
    Given a running CDN release gateway "broken" in "error_500" mode
    And a running CDN release gateway "mirror" in "s3_single_only_200_on_multi" mode
    And CDN gateway "mirror" publishes artifact "0.2.0" as "latest-linux.yml" in "electron" format with notes "Mirror failover\nversion: 0.0.1\nRecovered cleanly"
    And the updater endpoints are "cdn:broken/latest.json,cdn:mirror/latest-linux.yml"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the update notes equal "Mirror failover\nversion: 0.0.1\nRecovered cleanly"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"
