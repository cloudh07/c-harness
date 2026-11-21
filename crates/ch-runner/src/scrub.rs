//! Masking secrets in text that leaves the runner.

use crate::base64;

pub const HIDDEN: &str = "<hidden>";

/// Values shorter than this are left alone in omp's stderr: masking every `1` or `true` an
/// environment variable holds would wreck the text without protecting anything.
const MIN_ENV_SECRET_LEN: usize = 8;

pub struct Scrubber {
    secrets: Vec<String>,
}

impl Scrubber {
    pub fn new<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut secrets: Vec<String> = secrets
            .into_iter()
            .map(Into::into)
            .filter(|s| !s.is_empty())
            .collect();
        // Longest first, so a secret that contains another is masked whole.
        secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        secrets.dedup();
        Self { secrets }
    }

    /// Everything a push with `token` could echo back: the token and the header built from it.
    pub fn for_token(token: Option<&str>) -> Self {
        let Some(token) = token else {
            return Self::new(Vec::<String>::new());
        };
        let credential = base64::encode(format!("x-access-token:{token}").as_bytes());
        Self::new([token.to_owned(), credential])
    }

    /// The values of omp's own environment that are long enough to be secrets.
    pub fn for_env<'a>(values: impl IntoIterator<Item = &'a String>) -> Self {
        Self::new(
            values
                .into_iter()
                .filter(|v| v.len() >= MIN_ENV_SECRET_LEN)
                .cloned(),
        )
    }

    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), HIDDEN);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_every_occurrence() {
        let scrubber = Scrubber::new(["test-token-not-real"]);
        assert_eq!(
            scrubber.scrub("a test-token-not-real b test-token-not-real"),
            "a <hidden> b <hidden>"
        );
    }

    #[test]
    fn masks_the_longer_secret_whole() {
        let scrubber = Scrubber::new(["abc", "abcdef"]);
        assert_eq!(scrubber.scrub("xabcdefx"), "x<hidden>x");
    }

    #[test]
    fn ignores_empty_secrets() {
        let scrubber = Scrubber::new(["", "secret-value"]);
        assert_eq!(scrubber.scrub("plain"), "plain");
    }

    #[test]
    fn token_scrubber_masks_the_encoded_credential() {
        let scrubber = Scrubber::for_token(Some("test-token-not-real"));
        let header = "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46dGVzdC10b2tlbi1ub3QtcmVhbA==";
        assert_eq!(scrubber.scrub(header), "AUTHORIZATION: basic <hidden>");
        assert_eq!(
            scrubber.scrub("fatal: bad token test-token-not-real"),
            "fatal: bad token <hidden>"
        );
    }

    #[test]
    fn env_scrubber_skips_short_values() {
        let values = ["1".to_owned(), "test-model-key-not-real".to_owned()];
        let scrubber = Scrubber::for_env(&values);
        assert_eq!(
            scrubber.scrub("debug=1 key=test-model-key-not-real"),
            "debug=1 key=<hidden>"
        );
    }

    #[test]
    fn no_token_scrubs_nothing() {
        let scrubber = Scrubber::for_token(None);
        assert_eq!(scrubber.scrub("anything"), "anything");
    }
}
