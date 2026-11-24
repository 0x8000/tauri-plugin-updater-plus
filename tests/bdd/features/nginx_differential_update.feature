Feature: Differential update reconstruction over a real Nginx HTTP server
  As a desktop user upgrading a Tauri application
  I want unchanged local blocks reused and changed ranges fetched over HTTP 206 multipart/byteranges
  So that sequential releases transfer only changed chunks and atomically replace the AppImage

  Background:
    Given a running Nginx release server "primary"
    And a base release artifact "0.1.0" of size 2000000 bytes
    And a delta release artifact "0.2.0" derived from "0.1.0" at offsets "200000,900000,1600000"

  Scenario: Running AppImage serves as the differential base when the update cache is empty
    Given an installed AppImage at version "0.1.0" with an empty update cache
    And Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 75.0 percent of package bytes
    And the transfer emits Started Progress and Finished events
    And Nginx server "primary" served at least 1 multipart 206 response for "0.2.0"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Sequential updates chain cached packages across three versions via Nginx multipart ranges
    Given a delta release artifact "0.3.0" derived from "0.2.0" at offsets "350000,1100000"
    And an installed AppImage at version "0.1.0" with seeded cache
    And Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"
    When Nginx server "primary" publishes artifact "0.3.0" as "latest.json" in "tauri" format
    And the application executes "download_then_install"
    Then the update succeeds in "differential" mode to version "0.3.0"
    And the transfer saves more than 80.0 percent of package bytes
    And the installed AppImage matches artifact "0.3.0" with mode "0750"

  Scenario: Corrupted cached local blocks are re-fetched over HTTP Range without full fallback
    Given an installed AppImage at version "0.1.0" with seeded cache
    And Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the updater endpoints are "nginx:primary/latest.json"
    When local cached package bytes at offset 520000 are corrupted on disk
    And the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And Nginx server "primary" served at least 4 byte ranges across 206 responses for "0.2.0"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Delta exceeding fullDownloadThreshold falls back cleanly to a full Nginx download
    Given a heavily changed release artifact "0.2.0" of size 2000000 bytes with seed 987654321
    And an installed AppImage at version "0.1.0" with seeded cache
    And Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the updater configures fullDownloadThreshold to 0.50
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "full" mode to version "0.2.0"
    And the fallback reason contains "delta requires"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"
