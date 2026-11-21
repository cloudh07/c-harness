use std::fmt;

use crate::SecretName;

const HIDDEN: &str = "<hidden>";

/// Why a credential could not be stored, read or deleted. No variant holds a secret value.
#[derive(Debug)]
pub enum VaultError {
    /// The name is not `github` or `model/<provider>`. The rejected text is not kept, because a
    /// key pasted into the wrong field would otherwise end up in the message.
    InvalidName,
    /// The value to store is empty or only whitespace.
    EmptySecret { name: SecretName },
    /// This machine has no usable credential store. The keyring crate decides this once per
    /// process, so it lasts until the app restarts.
    Unavailable { detail: String },
    /// The credential store is locked or refused access.
    Locked { name: SecretName, detail: String },
    /// The stored value is not readable text.
    Unreadable { name: SecretName },
    /// The store has a size limit and the value or a name is over it.
    TooLong {
        name: SecretName,
        attribute: String,
        limit: u32,
    },
    /// More than one stored credential matches the name.
    Ambiguous { name: SecretName },
    /// Any other failure the credential store reported.
    Platform { name: SecretName, detail: String },
}

impl VaultError {
    pub(crate) fn from_keyring(name: &SecretName, err: keyring::Error) -> Self {
        let name = name.clone();
        match err {
            keyring::Error::NoDefaultStore => Self::Unavailable {
                detail: store_status_detail(),
            },
            keyring::Error::NoStorageAccess(source) => Self::Locked {
                name,
                detail: source.to_string(),
            },
            // These two carry the raw stored bytes, which is the secret itself.
            keyring::Error::BadEncoding(_) | keyring::Error::BadDataFormat(..) => {
                Self::Unreadable { name }
            }
            keyring::Error::TooLong(attribute, limit) => Self::TooLong {
                name,
                attribute,
                limit,
            },
            keyring::Error::Ambiguous(_) => Self::Ambiguous { name },
            err @ (keyring::Error::PlatformFailure(_)
            | keyring::Error::NoEntry
            | keyring::Error::BadStoreFormat(_)
            | keyring::Error::Invalid(..)
            | keyring::Error::NotSupportedByStore(_)) => Self::Platform {
                name,
                detail: err.to_string(),
            },
            // The enum is non-exhaustive, and a new variant might carry the value.
            _ => Self::Platform {
                name,
                detail: "the credential store reported an error this version does not know"
                    .to_owned(),
            },
        }
    }

    /// Replaces `value` wherever a platform message repeated it.
    pub(crate) fn redact(self, value: &str) -> Self {
        let scrub = |detail: String| {
            if value.is_empty() {
                detail
            } else {
                detail.replace(value, HIDDEN)
            }
        };
        match self {
            Self::Unavailable { detail } => Self::Unavailable {
                detail: scrub(detail),
            },
            Self::Locked { name, detail } => Self::Locked {
                name,
                detail: scrub(detail),
            },
            Self::Platform { name, detail } => Self::Platform {
                name,
                detail: scrub(detail),
            },
            Self::TooLong {
                name,
                attribute,
                limit,
            } => Self::TooLong {
                name,
                attribute: scrub(attribute),
                limit,
            },
            other => other,
        }
    }
}

// Store selection runs once per process; on failure its error explains why, for example no
// Secret Service on a Linux desktop.
fn store_status_detail() -> String {
    match keyring::Entry::store_status() {
        Err(err) => err.to_string(),
        Ok(()) => "no credential store was selected".to_owned(),
    }
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => f.write_str(
                "credential name is not allowed: use `github` or `model/<provider>`, where the \
                 provider is 1 to 40 lowercase letters, digits or '-'",
            ),
            Self::EmptySecret { name } => {
                write!(
                    f,
                    "the value for {name} is empty; paste the key or token and save again"
                )
            }
            Self::Unavailable { detail } => write!(
                f,
                "no OS credential store is available ({detail}). On Linux, install and unlock a \
                 Secret Service provider such as GNOME Keyring or KWallet, then restart C-Harness"
            ),
            Self::Locked { name, detail } => write!(
                f,
                "the OS credential store refused access to {name} ({detail}). Unlock it, allow \
                 C-Harness if the system asks, and try again"
            ),
            Self::Unreadable { name } => write!(
                f,
                "the stored value for {name} is not readable text; save the key or token again"
            ),
            Self::TooLong {
                name,
                attribute,
                limit,
            } => write!(
                f,
                "the OS credential store limits {attribute} to {limit} characters, which {name} \
                 is over; check that the right value was pasted"
            ),
            Self::Ambiguous { name } => write!(
                f,
                "the OS credential store has more than one entry for {name}; delete the extra \
                 entries for the c-harness service in your keychain app and try again"
            ),
            Self::Platform { name, detail } => write!(
                f,
                "the OS credential store failed on {name} ({detail}); try again, and check the \
                 keychain app if it keeps failing"
            ),
        }
    }
}

impl std::error::Error for VaultError {}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "test-secret-not-real";

    fn assert_hidden(err: &VaultError) {
        assert!(!err.to_string().contains(FAKE), "{err}");
        assert!(!format!("{err:?}").contains(FAKE));
    }

    #[test]
    fn stored_bytes_never_reach_the_error() {
        let name = SecretName::github();
        let bad_encoding = keyring::Error::BadEncoding(FAKE.as_bytes().to_vec());
        let err = VaultError::from_keyring(&name, bad_encoding);
        assert!(matches!(err, VaultError::Unreadable { .. }));
        assert_hidden(&err);

        let bad_format = keyring::Error::BadDataFormat(FAKE.as_bytes().to_vec(), FAKE.into());
        let err = VaultError::from_keyring(&name, bad_format);
        assert!(matches!(err, VaultError::Unreadable { .. }));
        assert_hidden(&err);
    }

    #[test]
    fn keyring_errors_map_to_actions() {
        let name = SecretName::model("openai").unwrap();
        let locked =
            VaultError::from_keyring(&name, keyring::Error::NoStorageAccess("locked".into()));
        assert!(locked.to_string().contains("Unlock it"));
        let too_long =
            VaultError::from_keyring(&name, keyring::Error::TooLong("password".into(), 2560));
        assert!(too_long.to_string().contains("2560"));
        assert!(matches!(
            VaultError::from_keyring(&name, keyring::Error::Ambiguous(Vec::new())),
            VaultError::Ambiguous { .. }
        ));
    }

    #[test]
    fn redact_removes_a_repeated_value() {
        let err = VaultError::Platform {
            name: SecretName::github(),
            detail: format!("store rejected {FAKE} twice: {FAKE}"),
        }
        .redact(FAKE);
        assert_hidden(&err);
        assert!(
            err.to_string()
                .contains("store rejected <hidden> twice: <hidden>")
        );
    }
}
