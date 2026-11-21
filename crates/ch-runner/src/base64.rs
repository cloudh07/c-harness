//! Standard base64 with padding, for the push's `Authorization` header.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let group = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for i in 0..4 {
            if i <= chunk.len() {
                let index = (group >> (18 - 6 * i)) & 0x3f;
                out.push(char::from(ALPHABET[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn matches_rfc_4648_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in vectors {
            assert_eq!(encode(input.as_bytes()), expected, "input {input:?}");
        }
    }

    #[test]
    fn encodes_the_push_credential() {
        assert_eq!(
            encode(b"x-access-token:test-token-not-real"),
            "eC1hY2Nlc3MtdG9rZW46dGVzdC10b2tlbi1ub3QtcmVhbA=="
        );
    }

    #[test]
    fn uses_the_high_end_of_the_alphabet() {
        assert_eq!(encode(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(encode(&[0xff]), "/w==");
    }
}
