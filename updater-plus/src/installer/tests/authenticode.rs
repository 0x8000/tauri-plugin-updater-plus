use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::Error;
use crate::config::WindowsConfig;
use crate::installer::{AuthenticodePolicy, WindowsAuthenticodeVerifier};

#[test]
fn resolves_absolute_powershell_path_and_rejects_missing_or_relative_system_root() {
    assert_eq!(
        WindowsAuthenticodeVerifier::windows_powershell_path(Some(r"C:\Windows"))
            .expect("valid SystemRoot"),
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
    );
    assert_eq!(
        WindowsAuthenticodeVerifier::windows_powershell_path(Some("D:/Windows/"))
            .expect("forward slash SystemRoot normalized"),
        r"D:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
    );

    assert_eq!(
        WindowsAuthenticodeVerifier::windows_system32_path(Some(r"C:\Windows"), "msiexec.exe")
            .expect("valid msiexec path"),
        r"C:\Windows\System32\msiexec.exe"
    );

    assert!(matches!(
        WindowsAuthenticodeVerifier::windows_powershell_path(None),
        Err(Error::Signature(ref msg)) if msg.contains("SystemRoot environment variable is not set")
    ));
    for invalid in ["", "C:", r"\Windows", "Windows"] {
        assert!(matches!(
            WindowsAuthenticodeVerifier::windows_powershell_path(Some(invalid)),
            Err(Error::Signature(ref msg)) if msg.contains("not an absolute path")
        ));
    }
}

#[test]
fn encodes_powershell_command_as_utf16le_using_env_var_path_and_hardened_flags() {
    let args = WindowsAuthenticodeVerifier::build_powershell_args();
    assert_eq!(
        args.len(),
        6,
        "unexpected extra PowerShell arguments: {args:?}"
    );
    assert_eq!(
        &args[..5],
        [
            "-NoProfile",
            "-NonInteractive",
            "-InputFormat",
            "None",
            "-EncodedCommand",
        ]
    );
    let raw = STANDARD.decode(&args[5]).expect("base64 utf-16le");
    assert_eq!(raw.len() % 2, 0);
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let script = String::from_utf16(&units).expect("valid utf-16");
    let env_var = WindowsAuthenticodeVerifier::file_env_var();
    assert!(!env_var.is_empty());
    assert!(script.contains("$ProgressPreference = 'SilentlyContinue'"));
    assert!(script.contains("$env:PSModulePath = \"\""));
    assert!(script.contains(r#"Import-Module "$PSHOME\Modules\Microsoft.PowerShell.Security""#));
    assert!(script.contains(r#"Import-Module "$PSHOME\Modules\Microsoft.PowerShell.Utility""#));
    assert!(script.contains(&format!(
        "Get-AuthenticodeSignature -LiteralPath $env:{env_var} -ErrorAction Stop | ConvertTo-Json -Compress"
    )));
    assert!(script.contains("exit 1"));
}

#[test]
fn parses_rfc2253_dn_and_matches_publisher_names_strictly() {
    let parsed = WindowsAuthenticodeVerifier::parse_dn(
        r#"CN="Acme, Inc.", O=M\C3\BCller GmbH; OU=R\&D + L=M\FCnchen, C=DE"#,
    );
    assert_eq!(parsed.get("CN").map(String::as_str), Some("Acme, Inc."));
    assert_eq!(parsed.get("O").map(String::as_str), Some("Müller GmbH"));
    assert_eq!(parsed.get("OU").map(String::as_str), Some("R&D"));
    assert_eq!(parsed.get("L").map(String::as_str), Some("München"));
    assert_eq!(parsed.get("C").map(String::as_str), Some("DE"));

    let escaped_quote =
        WindowsAuthenticodeVerifier::parse_dn(r#"CN="Acme \"Pro\", Inc.", O="A\\B""#);
    assert_eq!(
        escaped_quote.get("CN").map(String::as_str),
        Some(r#"Acme "Pro", Inc."#)
    );
    assert_eq!(escaped_quote.get("O").map(String::as_str), Some(r"A\B"));

    assert!(WindowsAuthenticodeVerifier::is_valid_publisher_name_dn(
        "CN=Acme, O=Acme, C=US"
    ));
    for invalid_dn in ["", "Acme", "CN=, O=Acme", "=Acme"] {
        assert!(
            !WindowsAuthenticodeVerifier::is_valid_publisher_name_dn(invalid_dn),
            "expected invalid DN: {invalid_dn:?}"
        );
    }

    let subject = "CN=Acme Studio, O=Acme Studio, L=San Francisco, S=CA, C=US";
    assert!(WindowsAuthenticodeVerifier::match_publisher_name(
        subject,
        &[
            "CN=Other Co, C=US".into(),
            "O=Acme Studio, CN=Acme Studio".into(),
        ],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        subject,
        &["Acme Studio".into()],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        subject,
        &["CN=Acme Studio, C=GB".into()],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        subject,
        &["CN=Acme Studio, OU=Release".into()],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        subject,
        &["CN=Acme Studio, O=".into()],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        subject,
        &[]
    ));

    // Multi-valued RDN attributes (multiple OU= / DC=) must preserve every component in order
    // instead of letting later attributes overwrite earlier ones.
    let multi_rdn_subject =
        "CN=Acme Updater, OU=Engineering, OU=ReleaseSigning, DC=corp, DC=acme, DC=com";
    assert!(WindowsAuthenticodeVerifier::match_publisher_name(
        multi_rdn_subject,
        &["CN=Acme Updater, OU=Engineering, OU=ReleaseSigning, DC=corp, DC=acme, DC=com".into(),],
    ));
    assert!(WindowsAuthenticodeVerifier::match_publisher_name(
        multi_rdn_subject,
        &["CN=Acme Updater, OU=ReleaseSigning, DC=acme, DC=com".into()],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        "CN=Acme Updater, OU=UntrustedContractor, OU=ReleaseSigning, DC=attacker, DC=com",
        &["CN=Acme Updater, OU=Engineering, OU=ReleaseSigning, DC=acme, DC=com".into()],
    ));
    assert!(!WindowsAuthenticodeVerifier::match_publisher_name(
        multi_rdn_subject,
        &["CN=Acme Updater, DC=com, DC=acme".into()],
    ));
}

#[test]
fn normalizes_windows_paths_for_literal_path_comparison() {
    assert_eq!(
        WindowsAuthenticodeVerifier::normalize_windows_path(
            r"c:/Users/./Alice//AppData/Local/Temp/nested/../setup.exe"
        ),
        r"C:\Users\Alice\AppData\Local\Temp\setup.exe"
    );
    assert_eq!(
        WindowsAuthenticodeVerifier::normalize_windows_path(
            r"\\fileserver\releases\v2\..\v3\Acme.exe"
        ),
        r"\\fileserver\releases\v3\Acme.exe"
    );
    assert_eq!(
        WindowsAuthenticodeVerifier::normalize_windows_path(r"c:\..\..\setup.exe"),
        r"C:\setup.exe"
    );
    assert_eq!(
        WindowsAuthenticodeVerifier::normalize_windows_path(r"c:relative\app.exe"),
        r"C:relative\app.exe"
    );
    assert_eq!(
        WindowsAuthenticodeVerifier::normalize_windows_path(r"\rooted\..\app.exe"),
        r"\app.exe"
    );
}

#[test]
fn evaluates_authenticode_json_with_fail_closed_guards() {
    let target = r"C:\Temp\Acme-0.2.0-setup.exe";
    let publishers = vec!["CN=Acme Studio, O=Acme Studio, C=US".to_string()];
    let embedded_policy = AuthenticodePolicy {
        publisher_names: &publishers,
        allow_catalog_signature: false,
    };

    let valid_json = r#"{
        "Status": 0,
        "Path": "c:\\Temp\\Acme-0.2.0-setup.exe",
        "SignatureType": 1,
        "SignerCertificate": {
            "Subject": "CN=Acme Studio, O=Acme Studio, L=Austin, C=US"
        }
    }"#;
    WindowsAuthenticodeVerifier::evaluate_signature_json(valid_json, target, &embedded_policy)
        .expect("valid embedded signature matching DN passes");

    let catalog_json = r#"{
        "Status": 0,
        "Path": "C:\\Temp\\Acme-0.2.0-setup.exe",
        "SignatureType": 2,
        "SignerCertificate": {
            "Subject": "CN=Acme Studio, O=Acme Studio, C=US"
        }
    }"#;
    let catalog_err = WindowsAuthenticodeVerifier::evaluate_signature_json(
        catalog_json,
        target,
        &embedded_policy,
    )
    .expect_err("catalog signature must be rejected by default");
    assert!(matches!(
        catalog_err,
        Error::Signature(ref msg) if msg.contains("catalog signature")
    ));

    let catalog_policy = AuthenticodePolicy {
        publisher_names: &publishers,
        allow_catalog_signature: true,
    };
    WindowsAuthenticodeVerifier::evaluate_signature_json(catalog_json, target, &catalog_policy)
        .expect("catalog signature allowed when opted in and signer matches");

    let untrusted_catalog_json = r#"{
        "Status": 0,
        "Path": "C:\\Temp\\Acme-0.2.0-setup.exe",
        "SignatureType": 2,
        "SignerCertificate": { "Subject": "CN=Untrusted Signer, C=US" }
    }"#;
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json(
            untrusted_catalog_json,
            target,
            &catalog_policy
        ),
        Err(Error::Signature(ref msg)) if msg.contains("does not match configured publisherName")
    ));

    let wrong_path_json = r#"{
        "Status": 0,
        "Path": "C:\\Windows\\System32\\cmd.exe",
        "SignatureType": 1,
        "SignerCertificate": {
            "Subject": "CN=Acme Studio, O=Acme Studio, C=US"
        }
    }"#;
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json(
            wrong_path_json,
            target,
            &embedded_policy
        ),
        Err(Error::Signature(ref msg)) if msg.contains("LiteralPath mismatch")
    ));

    let unsigned_json = r#"{"Status": 2, "Path": "C:\\Temp\\Acme-0.2.0-setup.exe"}"#;
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json(
            unsigned_json,
            target,
            &embedded_policy
        ),
        Err(Error::Signature(ref msg)) if msg.contains("signature is not valid") && msg.contains("Status=2")
    ));

    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json("[]", target, &embedded_policy),
        Err(Error::Signature(ref msg)) if msg.contains("not a JSON object")
    ));
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json(
            r#"{"Status": 0}"#,
            target,
            &embedded_policy
        ),
        Err(Error::Signature(ref msg)) if msg.contains("did not report the verified file Path")
    ));
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json(
            r#"{"Status": 0, "Path": "C:\\Temp\\Acme-0.2.0-setup.exe", "SignatureType": 1, "SignerCertificate": {}}"#,
            target,
            &embedded_policy
        ),
        Err(Error::Signature(ref msg)) if msg.contains("without SignerCertificate.Subject")
    ));

    let empty_policy = AuthenticodePolicy {
        publisher_names: &[],
        allow_catalog_signature: false,
    };
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json(valid_json, target, &empty_policy),
        Err(Error::Signature(ref msg)) if msg.contains("at least one publisherName")
    ));
    assert!(matches!(
        WindowsAuthenticodeVerifier::evaluate_signature_json("not-json", target, &embedded_policy),
        Err(Error::Signature(ref msg)) if msg.contains("cannot parse")
    ));
}

#[test]
fn deserializes_windows_config_publisher_name_and_rejects_invalid_types() {
    let single: WindowsConfig =
        serde_json::from_str(r#"{"publisherName": "CN=Acme, C=US"}"#).expect("single");
    assert_eq!(single.publisher_name, vec!["CN=Acme, C=US"]);
    assert!(!single.allow_catalog_signature);

    let many: WindowsConfig = serde_json::from_str(
        r#"{"publisherName": ["CN=Acme, C=US", "CN=Backup, C=US"], "allowCatalogSignature": true}"#,
    )
    .expect("many");
    assert_eq!(
        many.publisher_name,
        vec!["CN=Acme, C=US", "CN=Backup, C=US"]
    );
    assert!(many.allow_catalog_signature);

    let empty_arr: WindowsConfig =
        serde_json::from_str(r#"{"publisherName": []}"#).expect("empty array");
    assert!(empty_arr.publisher_name.is_empty());

    let null_val: WindowsConfig = serde_json::from_str(r#"{"publisherName": null}"#).expect("null");
    assert!(null_val.publisher_name.is_empty());

    let omitted: WindowsConfig = serde_json::from_str("{}").expect("omitted");
    assert!(omitted.publisher_name.is_empty());

    assert!(
        serde_json::from_str::<WindowsConfig>(r#"{"publisherName": 123}"#)
            .expect_err("integer publisherName must fail")
            .is_data()
    );
    assert!(
        serde_json::from_str::<WindowsConfig>(r#"{"publisherName": [123]}"#)
            .expect_err("integer array element in publisherName must fail")
            .is_data()
    );
}
