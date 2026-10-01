// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Standard base64 with padding, which is how the CLI's protocol carries the
//! bytes of an image.
//!
//! Written here rather than taken from a crate: it is one encoder of a dozen
//! lines, and every crate in the tree is one the release has to carry and the
//! dependency check has to allow.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in standard base64, padded, with no line breaks: the protocol
/// takes the string whole.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(niobe_core::image::encoded_len(bytes.len()));
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let sextet = |shift: u32| char::from(ALPHABET[((n >> shift) & 0x3f) as usize]);
        out.push(sextet(18));
        out.push(sextet(12));
        out.push(if chunk.len() > 1 { sextet(6) } else { '=' });
        out.push(if chunk.len() > 2 { sextet(0) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_4648_test_vectors_encode_as_the_rfc_gives_them() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(encode(plain.as_bytes()), encoded, "{plain:?}");
        }
    }

    #[test]
    fn every_byte_value_uses_the_standard_alphabet() {
        let every: Vec<u8> = (0..=255).collect();
        let encoded = encode(&every);
        assert_eq!(encoded.len(), niobe_core::image::encoded_len(every.len()));
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
        assert!(encoded.ends_with("/P3+/w=="), "{encoded}");
    }
}
