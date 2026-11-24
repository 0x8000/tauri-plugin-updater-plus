use std::collections::BTreeMap;

use lazy_regex::regex_captures;

/// Parsed RFC 2253 X.509 Distinguished Name used for Windows Authenticode publisher matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DistinguishedName {
    attributes: BTreeMap<String, String>,
}

impl DistinguishedName {
    pub(crate) fn parse(subject: &str) -> Self {
        fn decode_hex_run(hex_run: &str) -> String {
            let mut bytes = Vec::with_capacity(hex_run.len() / 3);
            let mut cursor = hex_run.as_bytes();
            while cursor.len() >= 3 {
                if let Ok(hex) = std::str::from_utf8(&cursor[1..3])
                    && let Ok(byte) = u8::from_str_radix(hex, 16)
                {
                    bytes.push(byte);
                }
                cursor = &cursor[3..];
            }
            let mut decoded = String::with_capacity(bytes.len());
            let mut offset = 0;
            while offset < bytes.len() {
                let first = bytes[offset];
                let width = if first < 0x80 {
                    1
                } else if (0xc2..=0xdf).contains(&first) {
                    2
                } else if (0xe0..=0xef).contains(&first) {
                    3
                } else if (0xf0..=0xf4).contains(&first) {
                    4
                } else {
                    0
                };
                if width > 0
                    && offset + width <= bytes.len()
                    && let Ok(text) = std::str::from_utf8(&bytes[offset..offset + width])
                {
                    decoded.push_str(text);
                    offset += width;
                } else {
                    decoded.push(char::from(first));
                    offset += 1;
                }
            }
            decoded
        }

        let trimmed = subject.trim();
        let chars: Vec<(usize, char)> = trimmed.char_indices().collect();
        let mut attributes = BTreeMap::new();
        let mut quoted = false;
        let mut key: Option<String> = None;
        let mut token = String::new();
        let mut idx = 0usize;

        while idx < chars.len() {
            let (byte_pos, ch) = chars[idx];
            if quoted {
                if ch == '"' {
                    quoted = false;
                } else {
                    token.push(ch);
                }
                idx += 1;
                continue;
            }

            if ch == '"' {
                quoted = true;
                idx += 1;
                continue;
            }
            if ch == '\\' {
                // Matches RFC 2253 escaped hex byte runs:
                // - `\C3\BC` (UTF-8 multi-byte escape)
                // - `\2C` or `\fc` (single-byte escape)
                if let Some((_, hex_run)) =
                    regex_captures!(r"^((?:\\[0-9a-fA-F]{2})+)", &trimmed[byte_pos..])
                {
                    token.push_str(&decode_hex_run(hex_run));
                    idx += hex_run.len();
                } else if idx + 1 < chars.len() {
                    token.push(chars[idx + 1].1);
                    idx += 2;
                } else {
                    idx += 1;
                }
                continue;
            }
            if key.is_none() && ch == '=' {
                key = Some(std::mem::take(&mut token));
                idx += 1;
                continue;
            }
            if matches!(ch, ',' | ';' | '+') {
                if let Some(attr) = key.take() {
                    attributes.insert(attr, std::mem::take(&mut token));
                } else {
                    token.clear();
                }
                idx += 1;
                continue;
            }
            if ch == ' ' {
                if token.is_empty() {
                    idx += 1;
                    continue;
                }
                let mut next = idx;
                while next < chars.len() && chars[next].1 == ' ' {
                    next += 1;
                }
                let before_delimiter = next >= chars.len()
                    || matches!(chars[next].1, ',' | ';')
                    || (key.is_none() && chars[next].1 == '=')
                    || (key.is_some() && chars[next].1 == '+');
                if before_delimiter {
                    idx = next;
                    continue;
                }
            }

            token.push(ch);
            idx += 1;
        }

        if let Some(attr) = key {
            attributes.insert(attr, token);
        }
        Self { attributes }
    }

    pub(crate) fn into_map(self) -> BTreeMap<String, String> {
        self.attributes
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.attributes.is_empty()
            && self
                .attributes
                .iter()
                .all(|(k, v)| !k.is_empty() && !v.is_empty())
    }

    pub(crate) fn matches_candidate(&self, candidate: &Self) -> bool {
        candidate.is_valid()
            && candidate
                .attributes
                .iter()
                .all(|(k, v)| self.attributes.get(k) == Some(v))
    }
}
