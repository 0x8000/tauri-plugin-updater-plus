use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;

use aws_lc_rs::digest;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use lazy_regex::regex_captures;
use minisign_verify::{PublicKey, Signature, StreamVerifier};
use semver::Version;

use crate::{Error, Result};

#[cfg(test)]
mod tests;

const STREAM_BUFFER_BYTES: usize = 256 * 1024;

/// Expected integrity and authenticity parameters for a downloaded update artifact.
#[derive(Debug, Clone, Default)]
pub struct VerifyExpectation {
    /// Optional base64-encoded SHA-512 checksum.
    pub sha512: Option<String>,
    /// Optional base64-encoded minisign signature.
    pub signature: Option<String>,
    /// Optional base64-encoded minisign public key.
    pub pubkey: Option<String>,
    /// Version announced by the release manifest.
    pub announced_version: String,
    /// Require the minisign trusted comment to carry a matching version.
    pub require_signed_version: bool,
}

/// Verifies reconstructed update artifacts via SHA-512 and optional minisign signatures.
pub struct ArtifactVerifier;

impl ArtifactVerifier {
    /// Computes the base64-encoded SHA-512 digest of a file on disk.
    pub fn sha512_file(path: &Path) -> Result<String> {
        let reader = BufReader::with_capacity(STREAM_BUFFER_BYTES, File::open(path)?);
        let mut hasher = digest::Context::new(&digest::SHA512);
        Self::stream_reader(reader, Some(&mut hasher), None)?;
        Ok(STANDARD.encode(hasher.finish().as_ref()))
    }

    /// Computes the base64-encoded SHA-512 digest of an in-memory byte slice.
    pub fn sha512_bytes(data: &[u8]) -> String {
        STANDARD.encode(digest::digest(&digest::SHA512, data).as_ref())
    }

    /// Validates a file on disk against all configured checksum and signature expectations.
    pub fn verify_file(path: &Path, expectation: &VerifyExpectation) -> Result<()> {
        if expectation.sha512.is_none() && expectation.pubkey.is_none() {
            return Ok(());
        }
        let decoded = Self::decode_expectation_minisign(expectation)?;
        let reader = BufReader::with_capacity(STREAM_BUFFER_BYTES, File::open(path)?);
        Self::verify_decoded_reader(reader, decoded.as_ref(), expectation)
    }

    /// Validates a buffered byte stream against all configured checksum and signature expectations.
    pub fn verify_reader<R: BufRead>(reader: R, expectation: &VerifyExpectation) -> Result<()> {
        if expectation.sha512.is_none() && expectation.pubkey.is_none() {
            return Ok(());
        }
        let decoded = Self::decode_expectation_minisign(expectation)?;
        Self::verify_decoded_reader(reader, decoded.as_ref(), expectation)
    }

    fn verify_decoded_reader<R: BufRead>(
        reader: R,
        minisign: Option<&(PublicKey, Signature)>,
        expectation: &VerifyExpectation,
    ) -> Result<()> {
        fn verify_legacy_reader<R: BufRead>(
            mut reader: R,
            minisign: (&PublicKey, &Signature),
            expectation: &VerifyExpectation,
        ) -> Result<()> {
            let (public_key, signature) = minisign;
            tracing::warn!(
                "minisign signature uses legacy non-prehashed mode; falling back to buffered verification"
            );
            let mut data = Vec::new();
            reader.read_to_end(&mut data)?;
            if let Some(expected) = expectation.sha512.as_deref() {
                ArtifactVerifier::check_sha512(expected, ArtifactVerifier::sha512_bytes(&data))?;
            }
            public_key.verify(&data, signature, true).map_err(|error| {
                Error::Signature(format!("minisign verification failed: {error}"))
            })?;
            ArtifactVerifier::verify_signed_version(
                signature.trusted_comment(),
                &expectation.announced_version,
                expectation.require_signed_version,
            )
        }

        let mut stream_verifier = if let Some((public_key, signature)) = minisign {
            match public_key.verify_stream(signature) {
                Ok(verifier) => Some((signature, verifier)),
                Err(minisign_verify::Error::UnsupportedLegacyMode) => {
                    return verify_legacy_reader(reader, (public_key, signature), expectation);
                }
                Err(error) => {
                    return Err(Error::Signature(format!(
                        "minisign verification failed: {error}"
                    )));
                }
            }
        } else {
            None
        };

        let mut sha512 = expectation
            .sha512
            .as_deref()
            .map(|expected| (expected, digest::Context::new(&digest::SHA512)));
        Self::stream_reader(
            reader,
            sha512.as_mut().map(|(_, hasher)| hasher),
            stream_verifier.as_mut().map(|(_, verifier)| verifier),
        )?;

        if let Some((expected, hasher)) = sha512 {
            Self::check_sha512(expected, STANDARD.encode(hasher.finish().as_ref()))?;
        }

        if let Some((signature, mut verifier)) = stream_verifier {
            verifier.finalize().map_err(|error| {
                Error::Signature(format!("minisign verification failed: {error}"))
            })?;
            Self::verify_signed_version(
                signature.trusted_comment(),
                &expectation.announced_version,
                expectation.require_signed_version,
            )?;
        }

        Ok(())
    }

    fn decode_expectation_minisign(
        expectation: &VerifyExpectation,
    ) -> Result<Option<(PublicKey, Signature)>> {
        fn decode_base64_utf8(encoded: &str, field: &str) -> Result<String> {
            let raw = STANDARD
                .decode(encoded.trim())
                .map_err(|error| Error::Signature(format!("invalid base64 {field}: {error}")))?;
            String::from_utf8(raw)
                .map_err(|error| Error::Signature(format!("non-utf8 {field}: {error}")))
        }

        let Some(pubkey_b64) = expectation.pubkey.as_deref() else {
            return Ok(None);
        };
        let signature_b64 = expectation
            .signature
            .as_deref()
            .ok_or_else(|| Error::Signature("missing signature for configured pubkey".into()))?;
        let pubkey_text = decode_base64_utf8(pubkey_b64, "pubkey")?;
        let sig_text = decode_base64_utf8(signature_b64, "signature")?;
        let public_key = PublicKey::decode(&pubkey_text)
            .map_err(|error| Error::Signature(format!("invalid minisign pubkey: {error}")))?;
        let signature = Signature::decode(&sig_text)
            .map_err(|error| Error::Signature(format!("invalid minisign signature: {error}")))?;
        Ok(Some((public_key, signature)))
    }

    fn stream_reader<R: BufRead>(
        mut reader: R,
        mut sha512: Option<&mut digest::Context>,
        mut minisign: Option<&mut StreamVerifier<'_>>,
    ) -> Result<()> {
        loop {
            let chunk = match reader.fill_buf() {
                Ok([]) => break,
                Ok(chunk) => chunk,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            if let Some(hasher) = sha512.as_deref_mut() {
                hasher.update(chunk);
            }
            if let Some(verifier) = minisign.as_deref_mut() {
                verifier.update(chunk);
            }
            let consumed = chunk.len();
            reader.consume(consumed);
        }
        Ok(())
    }

    fn check_sha512(expected: &str, actual: String) -> Result<()> {
        if actual == expected {
            return Ok(());
        }
        Err(Error::ChecksumMismatch {
            expected: expected.to_string(),
            actual,
        })
    }

    pub(crate) fn extract_signed_version(trusted_comment: &str) -> Option<&str> {
        // Matches Tauri minisign trusted-comment version tokens:
        // - `version:1.2.3` at start of comment
        // - `timestamp:1700000000\tfile:app.tar.gz\tversion:1.2.3`
        // - `timestamp:1700000000\tversion:v1.2.3-beta.1\tfile:app.tar.gz`
        regex_captures!(r"(?:^|\t)version:([^\t]+)", trusted_comment).map(|(_, version)| version)
    }

    pub(crate) fn verify_signed_version(
        trusted_comment: &str,
        announced_version: &str,
        require_signed_version: bool,
    ) -> Result<()> {
        let Some(signed) = Self::extract_signed_version(trusted_comment) else {
            return if require_signed_version {
                Err(Error::MissingSignedVersion)
            } else {
                Ok(())
            };
        };

        let matches = match (
            Version::from_str(signed.trim_start_matches('v')),
            Version::from_str(announced_version.trim_start_matches('v')),
        ) {
            (Ok(signed_ver), Ok(announced_ver)) => signed_ver == announced_ver,
            _ => signed == announced_version,
        };

        if matches {
            Ok(())
        } else {
            Err(Error::SignedVersionMismatch {
                signed: signed.to_string(),
                announced: announced_version.to_string(),
            })
        }
    }
}
