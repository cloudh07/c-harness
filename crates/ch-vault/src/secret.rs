use std::fmt;
use std::str::FromStr;

use crate::VaultError;

const GITHUB: &str = "github";
const MODEL_PREFIX: &str = "model/";
const MAX_PROVIDER_LEN: usize = 40;

/// Which credential an entry holds: `github`, or `model/<provider>` where the provider is 1 to
/// 40 of `[a-z0-9-]`. It is also the keychain account name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretName(String);

impl SecretName {
    pub fn parse(name: &str) -> Result<Self, VaultError> {
        let valid = name == GITHUB || name.strip_prefix(MODEL_PREFIX).is_some_and(valid_provider);
        if valid {
            Ok(Self(name.to_owned()))
        } else {
            Err(VaultError::InvalidName)
        }
    }

    /// The name of the API key for a model provider, such as `model/openai`.
    pub fn model(provider: &str) -> Result<Self, VaultError> {
        if valid_provider(provider) {
            Ok(Self(format!("{MODEL_PREFIX}{provider}")))
        } else {
            Err(VaultError::InvalidName)
        }
    }

    /// The name of the GitHub token.
    pub fn github() -> Self {
        Self(GITHUB.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The provider of a `model/<provider>` name.
    pub fn provider(&self) -> Option<&str> {
        self.0.strip_prefix(MODEL_PREFIX)
    }
}

fn valid_provider(provider: &str) -> bool {
    (1..=MAX_PROVIDER_LEN).contains(&provider.len())
        && provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl FromStr for SecretName {
    type Err = VaultError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name)
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SecretName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A credential value. It has no `Display` and its `Debug` hides the value, so it cannot end
/// up in a log or an error message by accident; [`Secret::expose`] is the only way to read it.
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "test-secret-not-real";

    #[test]
    fn names_are_github_or_a_model_provider() {
        for ok in ["github", "model/openai", "model/a", "model/open-router-2"] {
            assert_eq!(SecretName::parse(ok).unwrap().as_str(), ok);
        }
        let long = format!("model/{}", "a".repeat(41));
        for bad in [
            "",
            "GitHub",
            "github/x",
            " github",
            "model",
            "model/",
            "model/OpenAI",
            "model/open_ai",
            "model/a/b",
            "model/a b",
            "model/é",
            long.as_str(),
            "other/openai",
        ] {
            assert!(SecretName::parse(bad).is_err(), "{bad:?}");
        }
        assert!(SecretName::parse(&format!("model/{}", "a".repeat(40))).is_ok());
    }

    #[test]
    fn constructors_match_parse() {
        let openai = SecretName::model("openai").unwrap();
        assert_eq!(openai, "model/openai".parse().unwrap());
        assert_eq!(openai.provider(), Some("openai"));
        assert_eq!(openai.to_string(), "model/openai");
        assert_eq!(SecretName::github(), SecretName::parse("github").unwrap());
        assert_eq!(SecretName::github().provider(), None);
        assert!(SecretName::model("Open/AI").is_err());
    }

    #[test]
    fn a_rejected_name_is_not_echoed() {
        // Someone may paste the key into the name field by mistake.
        let err = SecretName::parse(FAKE).unwrap_err();
        assert!(!err.to_string().contains(FAKE));
        assert!(!format!("{err:?}").contains(FAKE));
    }

    #[test]
    fn debug_hides_the_value() {
        let secret = Secret::new(FAKE);
        assert_eq!(format!("{secret:?}"), "Secret(<hidden>)");
        assert_eq!(format!("{:?}", Some(&secret)), "Some(Secret(<hidden>))");
        assert_eq!(secret.expose(), FAKE);
    }
}
