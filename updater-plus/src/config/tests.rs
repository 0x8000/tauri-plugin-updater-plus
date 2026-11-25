use std::collections::HashMap;

use serde_json::json;

use super::{Config, WindowsInstallMode};
use crate::Error;

#[test]
fn resolves_default_when_neither_updater_key_is_present() {
    let mut plugins = HashMap::new();
    plugins.insert("deep-link".to_string(), json!({}));
    plugins.insert("updater".to_string(), serde_json::Value::Null);
    plugins.insert("updater-plus".to_string(), serde_json::Value::Null);

    let config = Config::from_tauri_plugins(&plugins).expect("default config");
    assert_eq!(config, Config::default());
}

#[test]
fn reads_single_plugins_updater_section_for_tauri_cli_compatibility() {
    let mut plugins = HashMap::new();
    plugins.insert(
        "updater".to_string(),
        json!({
            "pubkey": "dGVzdC1wdWJrZXk=",
            "differential": true,
            "endpoints": ["https://example.com/latest.json"],
            "windows": {
                "installMode": "quiet"
            }
        }),
    );

    let config = Config::from_tauri_plugins(&plugins).expect("updater config");
    assert_eq!(config.pubkey.as_deref(), Some("dGVzdC1wdWJrZXk="));
    assert!(config.differential);
    assert_eq!(config.endpoints.len(), 1);
    assert_eq!(
        config.endpoints[0].as_str(),
        "https://example.com/latest.json"
    );
    assert_eq!(
        config.windows.expect("windows").install_mode,
        WindowsInstallMode::Quiet
    );
}

#[test]
fn reads_legacy_plugins_updater_plus_section_directly() {
    let mut plugins = HashMap::new();
    plugins.insert(
        "updater-plus".to_string(),
        json!({
            "pubkey": "bGVnYWN5LWtleQ==",
            "differential": false,
            "endpoints": ["https://example.com/v2/latest.json"]
        }),
    );

    let config = Config::from_tauri_plugins(&plugins).expect("updater-plus config");
    assert_eq!(config.pubkey.as_deref(), Some("bGVnYWN5LWtleQ=="));
    assert!(!config.differential);
    assert_eq!(
        config.endpoints[0].as_str(),
        "https://example.com/v2/latest.json"
    );
}

#[test]
fn overlays_updater_plus_fields_and_nested_windows_onto_updater_base() {
    let mut plugins = HashMap::new();
    plugins.insert(
        "updater".to_string(),
        json!({
            "pubkey": "YmFzZS1rZXk=",
            "endpoints": ["https://base.example.com/latest.json"],
            "windows": {
                "installMode": "basicUi",
                "installerArgs": ["/BASE"]
            }
        }),
    );
    plugins.insert(
        "updater-plus".to_string(),
        json!({
            "differential": false,
            "endpoints": ["https://override.example.com/latest.json"],
            "windows": {
                "publisherName": "CN=Acme, O=Acme, C=US",
                "allowCatalogSignature": true
            }
        }),
    );

    let config = Config::from_tauri_plugins(&plugins).expect("merged config");
    assert_eq!(config.pubkey.as_deref(), Some("YmFzZS1rZXk="));
    assert!(!config.differential);
    assert_eq!(
        config.endpoints[0].as_str(),
        "https://override.example.com/latest.json"
    );
    let windows = config.windows.expect("windows");
    assert_eq!(windows.install_mode, WindowsInstallMode::BasicUi);
    assert_eq!(windows.installer_args, vec!["/BASE"]);
    assert_eq!(windows.publisher_name, vec!["CN=Acme, O=Acme, C=US"]);
    assert!(windows.allow_catalog_signature);
}

#[test]
fn rejects_non_object_or_malformed_plugin_config() {
    let mut non_object = HashMap::new();
    non_object.insert("updater".to_string(), json!("not-an-object"));
    assert!(matches!(
        Config::from_tauri_plugins(&non_object),
        Err(Error::InvalidConfig(msg)) if msg.contains("plugins.updater")
    ));

    let mut bad_url = HashMap::new();
    bad_url.insert(
        "updater".to_string(),
        json!({
            "endpoints": ["not a valid url"]
        }),
    );
    assert!(matches!(
        Config::from_tauri_plugins(&bad_url),
        Err(Error::InvalidConfig(msg)) if msg.contains("failed to parse updater config")
    ));
}
