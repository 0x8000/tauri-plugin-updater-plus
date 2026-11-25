use super::{ByteRangeParser, ByteRangePart};
use crate::Error;

#[test]
fn parses_content_type_boundary_and_content_range() {
    assert_eq!(
        ByteRangeParser::extract_boundary("multipart/byteranges; boundary=3d6b6a416f9b5"),
        Some("3d6b6a416f9b5".into())
    );
    assert_eq!(
        ByteRangeParser::extract_boundary(r#"multipart/byteranges; boundary="quoted-boundary""#),
        Some("quoted-boundary".into())
    );
    assert_eq!(
        ByteRangeParser::extract_boundary("application/octet-stream"),
        None
    );

    assert_eq!(
        ByteRangeParser::parse_content_range("bytes 10-25/100").expect("valid range"),
        (10, 25)
    );
    assert_eq!(
        ByteRangeParser::parse_content_range("bytes 0-0/*").expect("wildcard total"),
        (0, 0)
    );
    assert!(matches!(
        ByteRangeParser::parse_content_range("bytes 25-10/100"),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("end precedes start")
    ));
    assert!(matches!(
        ByteRangeParser::parse_content_range("items 0-5/10"),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("invalid Content-Range")
    ));
}

#[test]
fn parses_multipart_byteranges_including_embedded_boundary_tokens_and_lf_framing() {
    let mut body = Vec::new();
    body.extend_from_slice(
        b"--BND\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-9/100\r\n\r\n",
    );
    // Contains the literal boundary sequence inside the binary slice to prove length-guided slicing.
    body.extend_from_slice(b"ab--BND\r\nx");
    body.extend_from_slice(b"\r\n--BND\nContent-Range: bytes 50-53/100\n\n");
    body.extend_from_slice(b"\x00\x01\r\n");
    body.extend_from_slice(b"\n--BND--\n");

    let parts = ByteRangeParser::parse_multipart(&body, "BND").expect("parse multipart");
    assert_eq!(
        parts,
        vec![
            ByteRangePart {
                start: 0,
                end: 9,
                data: b"ab--BND\r\nx".to_vec(),
            },
            ByteRangePart {
                start: 50,
                end: 53,
                data: b"\x00\x01\r\n".to_vec(),
            },
        ]
    );
}

#[test]
fn rejects_malformed_truncated_or_overflowing_multipart_payloads() {
    assert!(matches!(
        ByteRangeParser::parse_multipart(b"no boundary here", "BND"),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("missing opening boundary")
    ));
    assert!(matches!(
        ByteRangeParser::parse_multipart(b"--BND--\r\n", "BND"),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("no parts")
    ));
    assert!(matches!(
        ByteRangeParser::parse_multipart(
            b"--BND\r\nContent-Type: application/octet-stream\r\n\r\nabc\r\n--BND--\r\n",
            "BND"
        ),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("missing Content-Range")
    ));
    assert!(matches!(
        ByteRangeParser::parse_multipart(
            b"--BND\r\nContent-Range: bytes 0-9/100\r\n\r\nshort",
            "BND"
        ),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("truncated")
    ));
    assert!(matches!(
        ByteRangeParser::parse_multipart(
            b"--BND\r\nContent-Range: bytes 0-2/100\r\n\r\nabc",
            "BND"
        ),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("missing closing boundary")
    ));
    let overflow_body = format!(
        "--BND\r\nContent-Range: bytes 0-{}/100\r\n\r\nabc\r\n--BND--\r\n",
        u64::MAX
    );
    assert!(matches!(
        ByteRangeParser::parse_multipart(overflow_body.as_bytes(), "BND"),
        Err(Error::DifferentialTransfer(ref msg)) if msg.contains("overflows")
    ));
}
