Feature: Non-HTTP MQTT transport pull provider push DiffSession and cross-protocol failover
  As an application maintainer deploying over non-HTTP channels such as MQTT or DDS
  I want the update engine to operate purely through UpdateProvider and DiffSession without HTTP
  So that pull-based MQTT providers push-based subscribers and cross-protocol failovers work seamlessly

  Background:
    Given a base release artifact "0.1.0" of size 1800000 bytes
    And a delta release artifact "0.2.0" derived from "0.1.0" at offsets "200000,900000,1450000"
    And an installed AppImage at version "0.1.0" with seeded cache

  Scenario: MQTT Pull provider performs differential update over a real MQTT broker with zero HTTP traffic
    Given a running MQTT release broker "primary" in "normal" mode
    And MQTT broker "primary" publishes artifact "0.2.0" with notes "MQTT pull differential release"
    And the updater uses MQTT pull provider connected to broker "primary"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 75.0 percent of package bytes
    And the transfer emits Started Progress and Finished events
    And MQTT broker "primary" served at least 1 pull range request and 0 full requests
    And the update notes equal "MQTT pull differential release"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: MQTT Push receiver reconstructs via DiffSession with out-of-order overlapping and duplicate QoS-1 chunks
    Given a running MQTT release broker "push" in "out_of_order_and_duplicates" mode
    And MQTT broker "push" publishes artifact "0.2.0" with notes "MQTT push QoS-1 release"
    And the updater uses MQTT push DiffSession connected to broker "push"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 75.0 percent of package bytes
    And the push progress bytes equal the deduplicated missing range bytes
    And MQTT broker "push" pushed at least 4 chunks including 1 duplicate
    And the installed AppImage matches artifact "0.2.0" with mode "0750"

  Scenario: MQTT Push receiver rejects incomplete chunk stream with IncompleteTransfer while preserving cache and AppImage
    Given a running MQTT release broker "incomplete" in "drop_last_chunk" mode
    And MQTT broker "incomplete" publishes artifact "0.2.0" with notes "Truncated MQTT push"
    And the updater uses MQTT push DiffSession connected to broker "incomplete"
    When the application executes "download_and_install"
    Then the update fails with error kind "IncompleteTransfer"
    And the cached base package still matches artifact "0.1.0"
    And the installed AppImage matches artifact "0.1.0" with mode "0750"

  Scenario: Cross-protocol FailoverProvider falls over from an unreachable MQTT broker to a healthy Nginx HTTP mirror
    Given a running MQTT release broker "down" in "unavailable" mode
    And a running Nginx release server "mirror"
    And Nginx server "mirror" publishes artifact "0.2.0" as "latest.json" in "tauri" format
    And the updater uses cross-protocol FailoverProvider from MQTT "down" to "nginx:mirror/latest.json"
    When the application executes "download_and_install"
    Then the update succeeds in "differential" mode to version "0.2.0"
    And the transfer saves more than 75.0 percent of package bytes
    And MQTT broker "down" recorded at least 1 connection attempt
    And Nginx server "mirror" served at least 1 multipart 206 response for "0.2.0"
    And the installed AppImage matches artifact "0.2.0" with mode "0750"
