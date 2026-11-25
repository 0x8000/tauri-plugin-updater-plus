use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::TEST_PAYLOAD_SHA512;
use crate::Error;
use crate::verifier::{ArtifactVerifier, VerifyExpectation};

const BLAKE2B_512_TEST: [u8; 64] = [
    0xa7, 0x10, 0x79, 0xd4, 0x28, 0x53, 0xde, 0xa2, 0x6e, 0x45, 0x30, 0x04, 0x33, 0x86, 0x70, 0xa5,
    0x38, 0x14, 0xb7, 0x81, 0x37, 0xff, 0xbe, 0xd0, 0x76, 0x03, 0xa4, 0x1d, 0x76, 0xa4, 0x83, 0xaa,
    0x9b, 0xc3, 0x3b, 0x58, 0x2f, 0x77, 0xd3, 0x0a, 0x65, 0xe6, 0xf2, 0x9a, 0x89, 0x6c, 0x04, 0x11,
    0xf3, 0x83, 0x12, 0xe1, 0xd6, 0x6e, 0x0b, 0xf1, 0x63, 0x86, 0xc8, 0x6a, 0x89, 0xbe, 0xa5, 0x72,
];

fn make_versioned_minisign(prehashed: bool, trusted_comment: &str) -> (String, String) {
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};

    let key_pair = Ed25519KeyPair::from_seed_unchecked(&[0x42; 32]).expect("ed25519 seed");
    let key_id = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];

    let mut pub_bin = Vec::with_capacity(42);
    pub_bin.extend_from_slice(b"Ed");
    pub_bin.extend_from_slice(&key_id);
    pub_bin.extend_from_slice(key_pair.public_key().as_ref());
    let pubkey_text = format!(
        "untrusted comment: minisign public key\n{}",
        STANDARD.encode(&pub_bin)
    );

    let (alg, message): (&[u8; 2], &[u8]) = if prehashed {
        (b"ED", &BLAKE2B_512_TEST)
    } else {
        (b"Ed", b"test")
    };
    let sig_bytes = key_pair.sign(message);
    let mut sig_bin = Vec::with_capacity(74);
    sig_bin.extend_from_slice(alg);
    sig_bin.extend_from_slice(&key_id);
    sig_bin.extend_from_slice(sig_bytes.as_ref());

    let mut global_msg = Vec::with_capacity(64 + trusted_comment.len());
    global_msg.extend_from_slice(sig_bytes.as_ref());
    global_msg.extend_from_slice(trusted_comment.as_bytes());
    let global_sig = key_pair.sign(&global_msg);

    let sig_text = format!(
        "untrusted comment: minisign signature\n{}\ntrusted comment: {trusted_comment}\n{}",
        STANDARD.encode(&sig_bin),
        STANDARD.encode(global_sig.as_ref()),
    );
    (STANDARD.encode(pubkey_text), STANDARD.encode(sig_text))
}

#[test]
fn enforces_signed_version_match_and_rejects_replay_across_prehashed_and_legacy_modes() {
    let file = tempfile::NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), b"test").expect("write");

    for prehashed in [true, false] {
        let (pubkey, sig) =
            make_versioned_minisign(prehashed, "timestamp:1700000000\tfile:test\tversion:1.2.0");

        ArtifactVerifier::verify_file(
            file.path(),
            &VerifyExpectation {
                sha512: Some(TEST_PAYLOAD_SHA512.into()),
                pubkey: Some(pubkey.clone()),
                signature: Some(sig.clone()),
                announced_version: "v1.2.0".into(),
                require_signed_version: true,
            },
        )
        .expect("matching signed version must pass end-to-end verification");

        let replay_err = ArtifactVerifier::verify_file(
            file.path(),
            &VerifyExpectation {
                sha512: Some(TEST_PAYLOAD_SHA512.into()),
                pubkey: Some(pubkey),
                signature: Some(sig),
                announced_version: "1.3.0".into(),
                require_signed_version: true,
            },
        )
        .expect_err("valid signature for older version must fail replay check");
        assert!(matches!(
            replay_err,
            Error::SignedVersionMismatch { ref signed, ref announced }
                if signed == "1.2.0" && announced == "1.3.0"
        ));
    }
}
