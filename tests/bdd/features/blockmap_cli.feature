Feature: blockmap-gen CLI generation manifest and archive packaging
  As a release engineer building Tauri updates
  I want blockmap-gen to emit Electron v2 blockmaps manifests and bundle archives
  So that differential updates work across Tauri JSON Electron YAML and macOS bundles

  Scenario: Generate a v2 blockmap and Tauri JSON manifest for a release artifact
    Given a base release artifact "0.1.0" of size 1200000 bytes
    When I run blockmap-gen generate on artifact "0.1.0"
    And I run blockmap-gen manifest for artifact "0.1.0" in "tauri" format with notes "Initial blockmap release"
    Then the generated blockmap reports 1200000 total bytes and at least 10 blocks
    And the generated "tauri" manifest parses cleanly with version "0.1.0" and notes "Initial blockmap release"

  Scenario: Generate an Electron YAML manifest with multiline release notes and compute blockmap diff
    Given a base release artifact "0.1.0" of size 1500000 bytes
    And a delta release artifact "0.2.0" derived from "0.1.0" at offsets "200000,900000"
    When I run blockmap-gen manifest for artifact "0.2.0" in "electron" format with notes "Line one\nversion: 9.9.9\nLine three"
    And I run blockmap-gen diff from "0.1.0" to "0.2.0"
    Then the generated "electron" manifest parses cleanly with version "0.2.0" and notes "Line one\nversion: 9.9.9\nLine three"
    And the blockmap diff saves more than 80.0 percent across 2 download ranges

  Scenario: Pack a macOS app bundle into a blockmap-friendly zip preserving root folder permissions and symlinks
    Given a macOS app bundle "MyApp.app" at version "0.2.0" with a framework symlink
    When I run blockmap-gen pack-zip on bundle "MyApp.app"
    Then the packed zip preserves the "MyApp.app" root folder executable mode and framework symlink
