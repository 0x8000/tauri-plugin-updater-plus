use crate::Error;
use crate::installer::{MacosCodeSignVerifier, MacosSigningRequirement};

#[test]
fn parses_developer_id_designated_requirement_from_codesign_output() {
    let stdout = concat!(
        "designated => identifier \"com.example.app\" and anchor apple generic ",
        "and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ ",
        "and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ ",
        "and certificate leaf[subject.OU] = ABCDE12345\n"
    );
    let stderr = "Executable=/Applications/MyApp.app/Contents/MacOS/my-app\n";

    let req = MacosCodeSignVerifier::parse_designated_requirement(stdout, stderr)
        .expect("designated requirement parses");
    assert_eq!(
        req,
        MacosSigningRequirement::Designated(
            concat!(
                "identifier \"com.example.app\" and anchor apple generic ",
                "and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ ",
                "and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ ",
                "and certificate leaf[subject.OU] = ABCDE12345"
            )
            .to_string()
        )
    );

    // Some macOS tools combine stdout/stderr into stderr; parser must accept CRLF and stderr-only output.
    let from_stderr = MacosCodeSignVerifier::parse_designated_requirement(
        "",
        "Executable=/Applications/MyApp.app/Contents/MacOS/my-app\r\ndesignated => identifier \"com.example.app\" and certificate leaf[subject.OU] = TEAM99\r\n",
    )
    .expect("stderr designated requirement parses");
    assert_eq!(
        from_stderr,
        MacosSigningRequirement::Designated(
            "identifier \"com.example.app\" and certificate leaf[subject.OU] = TEAM99".to_string()
        )
    );

    // Compound requirement with cdhash stays Designated rather than bare AdHoc.
    let compound = MacosCodeSignVerifier::parse_designated_requirement(
        "designated => identifier \"com.example.app\" and cdhash H\"0123456789abcdef\"\n",
        "",
    )
    .expect("compound requirement parses");
    assert_eq!(
        compound,
        MacosSigningRequirement::Designated(
            "identifier \"com.example.app\" and cdhash H\"0123456789abcdef\"".to_string()
        )
    );
}

#[test]
fn classifies_unsigned_adhoc_and_malformed_codesign_outputs() {
    let unsigned = MacosCodeSignVerifier::parse_designated_requirement(
        "",
        "/Applications/MyApp.app: code object is not signed at all\n",
    )
    .expect("unsigned bundle classified");
    assert_eq!(unsigned, MacosSigningRequirement::Unsigned);

    for adhoc_line in [
        "designated => cdhash H\"0123456789abcdef0123456789abcdef01234567\"\n",
        "designated => cdhash H\"ABCDEF0123456789\"\r\n",
    ] {
        let adhoc = MacosCodeSignVerifier::parse_designated_requirement(
            adhoc_line,
            "Executable=/Applications/MyApp.app/Contents/MacOS/my-app\n",
        )
        .expect("ad-hoc cdhash classified");
        assert_eq!(adhoc, MacosSigningRequirement::AdHoc);
    }

    for (stdout, stderr) in [
        ("", ""),
        ("", "/Applications/MyApp.app: No such file or directory\n"),
        ("designated =>   \n", ""),
        (
            "designated =>   \n",
            "Executable=/Applications/MyApp.app/Contents/MacOS/my-app\n",
        ),
        (
            "designated =>\n",
            "Executable=/Applications/MyApp.app/Contents/MacOS/my-app\n",
        ),
        ("prefix designated => identifier \"com.example.app\"\n", ""),
    ] {
        let err = MacosCodeSignVerifier::parse_designated_requirement(stdout, stderr)
            .expect_err("malformed or blank designated requirement must fail");
        assert!(
            matches!(err, Error::Signature(ref msg) if msg.contains("failed to extract macOS codesign")),
            "unexpected error for ({stdout:?}, {stderr:?}): {err:?}"
        );
    }
}
