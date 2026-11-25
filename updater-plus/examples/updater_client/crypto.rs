use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

fn decode_hex(hex: &str, expected_len: usize) -> Result<Vec<u8>, String> {
    let trimmed = hex.trim();
    if trimmed.len() != expected_len * 2 {
        return Err(format!(
            "expected {} hex chars, got {}",
            expected_len * 2,
            trimmed.len()
        ));
    }
    (0..expected_len)
        .map(|idx| {
            u8::from_str_radix(&trimmed[idx * 2..idx * 2 + 2], 16)
                .map_err(|err| format!("invalid hex byte at {idx}: {err}"))
        })
        .collect()
}

pub(crate) fn run_keygen(seed_hex: &str) {
    let seed = decode_hex(seed_hex, 32).expect("valid 32-byte seed hex");
    let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("ed25519 keypair");
    let key_id = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    let mut pub_bin = Vec::with_capacity(42);
    pub_bin.extend_from_slice(b"Ed");
    pub_bin.extend_from_slice(&key_id);
    pub_bin.extend_from_slice(key_pair.public_key().as_ref());
    let pubkey_text = format!(
        "untrusted comment: minisign public key\n{}",
        STANDARD.encode(&pub_bin)
    );
    println!(
        "{}",
        serde_json::json!({ "pubkey": STANDARD.encode(pubkey_text) })
    );
}

pub(crate) fn run_sign(seed_hex: &str, blake2b_hex: &str, trusted_comment: &str) {
    let seed = decode_hex(seed_hex, 32).expect("valid 32-byte seed hex");
    let digest = decode_hex(blake2b_hex, 64).expect("valid 64-byte blake2b hex");
    let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("ed25519 keypair");
    let key_id = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

    let sig_bytes = key_pair.sign(&digest);
    let mut sig_bin = Vec::with_capacity(74);
    sig_bin.extend_from_slice(b"ED");
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
    println!(
        "{}",
        serde_json::json!({ "signature": STANDARD.encode(sig_text) })
    );
}
