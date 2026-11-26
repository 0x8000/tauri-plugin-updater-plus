Feature: Cryptographic verification anti-replay and fail-closed update security
  As a security-conscious Tauri application maintainer
  I want tampered unsigned or version-replayed updates rejected before installation
  So that local cached bases and installed binaries remain intact under attack

  Background:
    Given a running Nginx release server "primary"
    And a base release artifact "0.1.0" of size 1200000 bytes
    And a delta release artifact "0.2.0" derived from "0.1.0" at offsets "220000,820000"
    And an installed AppImage at version "0.1.0" with seeded cache

  Scenario: Valid minisign signature with matching signed version verifies and installs the AppImage
    Given Nginx server "primary" publishes signed artifact "0.2.0" with signed version "v0.2.0"
    And the updater trusts the minisign authority with requireSignedVersion "true"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Replaying a valid minisign signature from an older version is rejected with SignedVersionMismatch
    Given Nginx server "primary" publishes signed artifact "0.2.0" with signed version "0.1.0"
    And the updater trusts the minisign authority with requireSignedVersion "true"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update fails with error kind "SignedVersionMismatch"
    And the cached base package still matches artifact "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"

  Scenario: Tampered remote artifact fails SHA-512 verification while preserving cached base and installed AppImage
    Given Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And Nginx server "primary" tampers with published artifact "0.2.0"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update fails with error kind "ChecksumMismatch"
    And the cached base package still matches artifact "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"

  Scenario: Manifest omitting both sha512 and configured pubkey is rejected before downloading
    Given Nginx server "primary" publishes an unverified manifest "latest.json" for version "0.2.0"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update fails with error kind "InvalidManifest"
    And Nginx server "primary" recorded 0 artifact requests for "0.2.0"
    And the cached base package still matches artifact "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"

  Scenario: Oversized gzip bomb blockmap is rejected safely and falls back to verified full download
    Given Nginx server "primary" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And Nginx server "primary" replaces the blockmap for "0.2.0" with a gzip bomb
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "full" mode to version "0.2.0"
    And the fallback reason contains "exceeds 64 MiB"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: Signature omitting version comment is rejected with MissingSignedVersion when requireSignedVersion is enabled
    Given Nginx server "primary" publishes signed artifact "0.2.0" without a version comment
    And the updater trusts the minisign authority with requireSignedVersion "true"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update fails with error kind "MissingSignedVersion"
    And the cached base package still matches artifact "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"

  Scenario: HTTP error messages redact presigned query tokens from failing artifact URLs
    Given Nginx server "primary" publishes a presigned manifest "latest.json" for missing artifact "0.2.0" with query "X-Amz-Signature=supersecret999"
    And the updater configures differential to "false"
    And the updater endpoints are "nginx:primary/latest.json"
    When the application executes "download_and_install"
    Then the update fails with error kind "HttpStatus"
    And the error message contains "?<redacted>" and does not contain "supersecret999"
    And the cached base package still matches artifact "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"
