//! Whether a line of text holds a credential by its shape alone.
//!
//! Two rules: a PEM private key header, and a token with a known provider prefix (the same list
//! [`crate::redact_secrets`] masks) that holds a digit. The assignment-name rule is left out on
//! purpose: UI source declares `password: string` in every sign-in form. The digit keeps
//! kebab-case class names that happen to start with a prefix (`sk-loading-spinner-wrapper`) out;
//! provider keys are random.
//!
//! A floor, not a scanner: a key with no known prefix, or a PGP key block, passes.

use ch_protocol::runner::SecretKind;

use crate::redact::is_provider_token;

/// What ends every PEM private key header and footer, split so this file holds no such header.
const PRIVATE_KEY: &str = concat!("PRIVATE", " KEY-----");

/// The kind of credential `text` holds, or `None`. A private key wins over a token on one line.
#[must_use]
pub fn secret_kind(text: &str) -> Option<SecretKind> {
    if text.contains(PRIVATE_KEY) {
        return Some(SecretKind::PrivateKey);
    }
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .any(|word| word.bytes().any(|b| b.is_ascii_digit()) && is_provider_token(word))
        .then_some(SecretKind::ProviderToken)
}

/// Whether `text` holds a credential by its shape alone. See [`secret_kind`].
#[must_use]
pub fn holds_provider_secret(text: &str) -> bool {
    secret_kind(text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_key_or_private_key_is_held_back_and_a_form_is_not() {
        for held in [
            concat!(
                "const k = \"gh",
                "p_abcdefghijklmnopqrstuvwxyz0123456789\";"
            ),
            concat!("apiKey='sk", "-proj-abc123def456ghi789jkl012'"),
            concat!("AKIA", "ZZZZ0000ZZZZ0000"),
            concat!("-----BEGIN OPENSSH PRIV", "ATE KEY-----"),
        ] {
            assert!(holds_provider_secret(held), "{held}");
        }
        for plain in [
            "password: string;",
            "const [token, setToken] = useState('');",
            "<div className=\"sk-loading-spinner-wrapper\" />",
            "export function SignIn() { return <form /> }",
        ] {
            assert!(!holds_provider_secret(plain), "{plain}");
        }
    }

    #[test]
    fn a_provider_token_is_named_as_one() {
        for line in [
            concat!("token = \"gh", "p_abcdefghijklmnopqrstuvwxyz0123456789\""),
            concat!("    \"hf", "_abcdefghijklmnopqrstuvwx12\","),
            concat!("AWS_ACCESS_KEY_ID=AK", "IAZZZZ0000ZZZZ0000"),
        ] {
            assert_eq!(secret_kind(line), Some(SecretKind::ProviderToken), "{line}");
        }
    }

    #[test]
    fn a_pem_header_or_footer_is_a_private_key_even_beside_a_token() {
        for line in [
            concat!("-----BEGIN RSA PRIV", "ATE KEY-----"),
            concat!("-----END PRIV", "ATE KEY-----"),
            concat!(
                "-----BEGIN PRIV",
                "ATE KEY----- gh",
                "p_abcdefghijklmnopqrstuvwxyz0123456789"
            ),
        ] {
            assert_eq!(secret_kind(line), Some(SecretKind::PrivateKey), "{line}");
        }
    }

    #[test]
    fn a_public_key_a_prefix_alone_or_a_class_name_is_not_a_secret() {
        for line in [
            "-----BEGIN PUBLIC KEY-----",
            "sk-",
            "use the ghp_ prefix for classic tokens",
            "<div className=\"sk-loading-spinner-wrapper\" />",
            "",
        ] {
            assert_eq!(secret_kind(line), None, "{line}");
        }
    }
}
