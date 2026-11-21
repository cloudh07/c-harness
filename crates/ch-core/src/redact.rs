//! Credential masking for text that is about to be written to a session log.
//!
//! Narrow on purpose. This is not a secret scanner: a broad heuristic that eats plausible-looking
//! tokens would also strip the build output a reader needs, and a log that shows `[redacted]`
//! where the compiler printed a type name is worse than useless. Two rules only:
//!
//! 1. Tokens with a known provider prefix (`sk-`, `ghp_`, `AKIA`, `xoxb-`, ...). These exist to be
//!    recognizable, which is exactly what makes them safe to match on.
//! 2. Assignments whose name says the value is a credential (`API_KEY=`, `token:`, `password =`).
//!    The name survives so a reader can still see that a key was configured at all.
//!
//! Plus `Bearer <token>`, URL userinfo and credential-named URL parameters, webhook tokens in URL
//! paths, and passwords passed as command-line flags, which are the shapes credentials take when
//! they are not written as an assignment.
//!
//! No regex crate: this is a byte scan over lines, and the dependency would be the largest thing
//! in this file.

/// What replaces a matched secret. A fixed string, so two redacted logs that printed different
/// keys still diff as the same text.
pub const MASK: &str = "[redacted]";

/// Provider token prefixes worth matching. Each has a length floor, because `sk-` also starts
/// ordinary kebab-case identifiers (`sk-init`), and a short match is not a key.
const PREFIXES: &[(&str, usize)] = &[
    ("sk-", 20),
    ("sk_live_", 16),
    ("sk_test_", 16),
    ("rk_live_", 16),
    ("pk_live_", 16),
    ("ghp_", 24),
    ("gho_", 24),
    ("ghu_", 24),
    ("ghs_", 24),
    ("ghr_", 24),
    ("github_pat_", 24),
    ("glpat-", 16),
    ("xoxb-", 20),
    ("xoxp-", 20),
    ("xapp-", 20),
    ("AKIA", 20),
    ("ASIA", 20),
    ("AIza", 24),
    ("ya29.", 20),
    ("hf_", 24),
    ("npm_", 24),
    ("dop_v1_", 24),
    ("shpat_", 24),
];

/// Name segments that make a value a credential, matched whole. Never as a substring: that
/// masked `Author: Jane Doe` in a git log, `total_tokens: 1234` in usage output, and a compiler
/// diagnostic whose path happened to be `src/commands/secrets.rs:44`. A log that hides the error
/// is worse than one that shows a key.
const SECRET_SEGMENTS: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "apikey",
    "credential",
    "credentials",
    "authorization",
];

/// Two-segment names that are credentials as a phrase, where neither half is one alone: `key`
/// matches `keyboard_layout` and `api` matches nothing worth hiding.
const SECRET_PHRASES: &[&str] = &[
    "api_key",
    "access_key",
    "private_key",
    "secret_key",
    "auth_token",
    "refresh_token",
    "session_token",
    "client_secret",
];

/// Whether `token` looks like a provider credential rather than ordinary output.
fn is_provider_token(token: &str) -> bool {
    PREFIXES.iter().any(|(prefix, min_len)| {
        token.len() >= *min_len
            && token.starts_with(prefix)
            // The tail has to be credential-shaped: one long run without spaces, made of the
            // characters tokens use. A sentence starting "sk-" does not qualify.
            && token[prefix.len()..].chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '=')
            })
    })
}

/// Whether an assignment with this name carries a credential.
///
/// The name has to be a bare identifier. A path, a URL, or a sentence on the left of a colon is
/// not an assignment name: `src/commands/secrets.rs:44` is a diagnostic location, and treating it
/// as one turns a whole compiler error into a mask.
pub(crate) fn is_secret_name(name: &str) -> bool {
    let bare = name
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | '{' | '}' | '[' | ']'));
    if bare.is_empty()
        || bare.len() > 64
        || !bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return false;
    }
    // Segment the name the way both conventions write it: `AWS_SECRET_KEY`, `auth-token`, and
    // `authToken` all become the same segment list.
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    // `prev_lower` reads the original character, not the lowercased copy in `current`: testing
    // the copy makes every capital a boundary, so `OPENAI_API_KEY` would segment into single
    // letters and match nothing.
    let mut prev_lower = false;
    for c in bare.chars() {
        if matches!(c, '_' | '-') {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            prev_lower = false;
        } else if c.is_ascii_uppercase() && prev_lower {
            segments.push(std::mem::take(&mut current));
            current.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else {
            current.push(c.to_ascii_lowercase());
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    if segments
        .iter()
        .any(|seg| SECRET_SEGMENTS.contains(&seg.as_str()))
    {
        return true;
    }
    segments
        .windows(2)
        .any(|pair| SECRET_PHRASES.contains(&format!("{}_{}", pair[0], pair[1]).as_str()))
}

/// Redacts one line, keeping everything that is not a credential byte-identical.
///
/// A matched name is authoritative: `PASSWORD=123456` and `token=abc` are secrets at any length
/// and shape. Only the weaker prefix rule has a shape gate, the per-prefix floor in `PREFIXES`.
fn redact_line(line: &str) -> String {
    // A quoted line (`> `, as in a quoted reply) is redacted as its content: `> PASSWORD` is not
    // a bare name, so without this the quote marker would switch the assignment rule off.
    let quote = quote_prefix(line);
    if !quote.is_empty() {
        return format!("{quote}{}", redact_line(&line[quote.len()..]));
    }
    // Assignment first: `NAME=value` and `NAME: value`. Only the first separator splits, because
    // a value may legitimately contain more of them (a JWT carries dots, a URL carries colons).
    if let Some(cut) = line.find(['=', ':']) {
        let (name, rest) = line.split_at(cut);
        let sep = &rest[..1];
        let value = &rest[1..];
        if is_secret_name(name) && !value.trim().is_empty() {
            // Leading whitespace of the value survives so indentation and `KEY: value` spacing
            // read the same as before, and a trailing separator keeps a JSON or env line
            // parseable.
            let lead: String = value.chars().take_while(|c| c.is_whitespace()).collect();
            let trailing = match value.trim_end().chars().next_back() {
                Some(c @ (',' | ';')) => c.to_string(),
                _ => String::new(),
            };
            return format!("{name}{sep}{lead}{MASK}{trailing}");
        }
    }
    // Then the token shapes, word by word. Splitting on whitespace only: punctuation around a
    // token (quotes, brackets, trailing comma) is trimmed for the test and restored around the
    // mask, so `"ghp_..."` stays quoted.
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut after_bearer = false;
    // The command whose flags carry a credential on this line, once its name has gone by
    // (`curl -u`, `mysql -p`), and whether the word before was curl's separate `-u`/`--user`.
    let mut command = None;
    let mut after_user_flag = false;
    while !rest.is_empty() {
        let space = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(space);
        let core = word_core(word);
        let flagged = if after_bearer && is_bearer_token(core) {
            Some(MASK.to_string())
        } else if after_user_flag {
            mask_after_colon(core)
        } else {
            redact_flag(core, command)
        };
        match flagged {
            Some(masked) => out.push_str(&word.replacen(core, &masked, 1)),
            None => out.push_str(&redact_word(word)),
        }
        after_bearer = core.eq_ignore_ascii_case("bearer");
        after_user_flag = command == Some(FlagCommand::Curl) && matches!(core, "-u" | "--user");
        command = command_named(core).or(command);
        let gap = tail
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(tail.len());
        out.push_str(&tail[..gap]);
        rest = &tail[gap..];
    }
    out
}

/// A command whose own flags carry a credential in plain text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FlagCommand {
    /// `curl -u user:pass`, `curl --user user:pass`, `-uuser:pass`, `--user=user:pass`.
    Curl,
    /// `mysql -pSECRET` (a bare `-p` asks at the prompt and holds nothing).
    Mysql,
}

/// The command this word starts, by its file name: `curl`, `/usr/bin/curl`, `curl.exe`.
fn command_named(core: &str) -> Option<FlagCommand> {
    let base = core
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(core)
        .to_ascii_lowercase();
    match base.strip_suffix(".exe").unwrap_or(&base) {
        "curl" => Some(FlagCommand::Curl),
        "mysql" | "mysqldump" | "mysqladmin" | "mariadb" | "mariadb-dump" => {
            Some(FlagCommand::Mysql)
        }
        _ => None,
    }
}

/// Environment variables the database clients read a password from, by their exact names.
const PASSWORD_ENV: &[&str] = &["PGPASSWORD", "MYSQL_PWD"];

/// A flag or a shell assignment that carries a credential in this one word, with only the secret
/// part masked: the flag, the variable name and a `user:` stay readable.
///
/// - `--password=SECRET` (any `--<name>=` whose name is a credential name, as in rule 2, and does
///   not name a place such as `--password-file=`);
/// - `PGPASSWORD=SECRET`, `MYSQL_PWD=SECRET`, also as `PowerShell`'s `$env:PGPASSWORD=...`;
/// - after `curl`: `-uuser:pass` and `--user=user:pass`;
/// - after `mysql`: `-pSECRET`.
fn redact_flag(core: &str, command: Option<FlagCommand>) -> Option<String> {
    if let Some((name, value)) = core.split_once('=') {
        if value.is_empty() {
            return None;
        }
        if command == Some(FlagCommand::Curl) && name == "--user" {
            return mask_after_colon(value).map(|v| format!("{name}={v}"));
        }
        let env = name.strip_prefix("$env:").unwrap_or(name);
        let flag = name
            .strip_prefix("--")
            .is_some_and(|n| !n.starts_with('-') && is_secret_name(n) && !names_a_place(n));
        return (flag || PASSWORD_ENV.contains(&env)).then(|| format!("{name}={MASK}"));
    }
    match command {
        Some(FlagCommand::Curl) => core
            .strip_prefix("-u")
            .filter(|v| !v.is_empty())
            .and_then(mask_after_colon)
            .map(|v| format!("-u{v}")),
        Some(FlagCommand::Mysql) => core
            .strip_prefix("-p")
            .filter(|v| !v.is_empty())
            .map(|_| format!("-p{MASK}")),
        None => None,
    }
}

/// `--password-file=PATH`, `--token-path=PATH`: the flag names where the secret is, and the path
/// is not the secret.
fn names_a_place(flag: &str) -> bool {
    let last = flag
        .rsplit(['-', '_'])
        .next()
        .unwrap_or(flag)
        .to_ascii_lowercase();
    matches!(last.as_str(), "file" | "path" | "dir" | "env")
}

/// `user:pass` with the part after the first colon masked; `None` without a colon or a password
/// (`curl -u user` asks for the password, so it holds none).
fn mask_after_colon(value: &str) -> Option<String> {
    let (user, pass) = value.split_once(':')?;
    (!pass.is_empty()).then(|| format!("{user}:{MASK}"))
}

/// The leading quote markers of a line, at any depth: `>` each, optionally followed by one space.
fn quote_prefix(line: &str) -> &str {
    let mut rest = line;
    while let Some(after) = rest.strip_prefix('>') {
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    &line[..line.len() - rest.len()]
}

/// A word without the punctuation it may be wrapped in, which is output, not secret.
fn word_core(word: &str) -> &str {
    word.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '<' | '>'
        )
    })
}

/// The word after `Bearer` when it is shaped like a credential: long, RFC 6750 `b64token`
/// characters (plus `.`, which a JWT uses), and a digit or mixed case. The last condition is what
/// leaves `Bearer authorization-header-required` in an error message readable.
fn is_bearer_token(core: &str) -> bool {
    core.len() >= 16
        && core.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '+' | '/' | '=')
        })
        && (core.chars().any(|c| c.is_ascii_digit())
            || (core.chars().any(|c| c.is_ascii_uppercase())
                && core.chars().any(|c| c.is_ascii_lowercase())))
}

/// Redacts one whitespace-delimited word, preserving the punctuation wrapped around it.
fn redact_word(word: &str) -> String {
    let core = word_core(word);
    if core.is_empty() {
        return word.to_string();
    }
    let replacement = if is_provider_token(core) {
        Some(MASK.to_string())
    } else if let Some(masked) = redact_url_path_token(core) {
        Some(masked)
    } else {
        let userinfo = redact_url_userinfo(core);
        redact_url_params(userinfo.as_deref().unwrap_or(core)).or(userinfo)
    };
    match replacement {
        Some(masked) => word.replacen(core, &masked, 1),
        None => word.to_string(),
    }
}

/// `scheme://user:password@host` keeps the user and loses the password: who connected is
/// diagnosable, the credential is not.
fn redact_url_userinfo(word: &str) -> Option<String> {
    let scheme_end = word.find("://")? + 3;
    let at = word[scheme_end..].find('@')? + scheme_end;
    let userinfo = &word[scheme_end..at];
    let colon = userinfo.find(':')?;
    if userinfo[colon + 1..].is_empty() {
        return None;
    }
    Some(format!(
        "{}{}{MASK}{}",
        &word[..scheme_end],
        &userinfo[..=colon],
        &word[at..]
    ))
}

/// Webhook and bot URLs whose path holds the token, with only that part masked: the host, the
/// route and the ids before the token stay readable.
///
/// - Slack: `hooks.slack.com/services/<team>/<bot>/<token>`;
/// - Discord: `discord.com/api[/v<n>]/webhooks/<id>/<token>` (also `discordapp.com`);
/// - Telegram: `api.telegram.org[/file]/bot<id>:<token>/...`.
fn redact_url_path_token(word: &str) -> Option<String> {
    // ASCII lowercasing keeps every byte where it was, so an index found here is one in `word`.
    let lower = word.to_ascii_lowercase();
    let after = |marker: &str| lower.find(marker).map(|i| i + marker.len());
    if let Some(start) = after("hooks.slack.com/services/") {
        return mask_path_segment(word, start, 2);
    }
    if let Some(mut start) = after("discord.com/api/").or_else(|| after("discordapp.com/api/")) {
        let version = &lower[start..];
        if let Some(v) = version.strip_prefix('v') {
            let digits = v.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && v[digits..].starts_with('/') {
                start += 1 + digits + 1;
            }
        }
        return match lower[start..].strip_prefix("webhooks/") {
            Some(_) => mask_path_segment(word, start + "webhooks/".len(), 1),
            None => None,
        };
    }
    for marker in ["api.telegram.org/bot", "api.telegram.org/file/bot"] {
        if let Some(start) = after(marker) {
            let end = start
                + word[start..]
                    .find(['/', '?', '#'])
                    .unwrap_or(word.len() - start);
            let (id, token) = word[start..end].split_once(':')?;
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) || token.is_empty() {
                return None;
            }
            return Some(format!("{}{id}:{MASK}{}", &word[..start], &word[end..]));
        }
    }
    None
}

/// `word` with path segment number `n` (from 0) after byte `start` masked, when it is there and
/// not empty. A segment ends at `/`, `?` or `#`.
fn mask_path_segment(word: &str, start: usize, n: usize) -> Option<String> {
    let mut from = start;
    for _ in 0..n {
        let rest = &word[from..];
        let slash = rest.find('/')?;
        if rest[..slash].is_empty() || rest[..slash].contains(['?', '#']) {
            return None;
        }
        from += slash + 1;
    }
    let len = word[from..]
        .find(['/', '?', '#'])
        .unwrap_or(word.len() - from);
    (len > 0).then(|| format!("{}{MASK}{}", &word[..from], &word[from + len..]))
}

/// URL parameter names that carry a credential although no `SECRET_SEGMENTS` word is in them:
/// an OAuth authorization code, the signed-request parameters of presigned links, session ids,
/// and SSO tickets and assertions.
const URL_SECRET_PARAMS: &[&str] = &[
    "code",
    "auth_code",
    "sig",
    "signature",
    "x-amz-signature",
    "x-amz-credential",
    "x-goog-signature",
    "x-goog-credential",
    "jwt",
    "otp",
    "auth",
    "pwd",
    "pass",
    "sid",
    "session",
    "sessionid",
    "jsessionid",
    "phpsessid",
    "ticket",
    "samlresponse",
];

/// `name=value` pairs in a URL's query and fragment keep their names and lose the values a
/// credential name marks: an OAuth callback address or a presigned link would otherwise put the
/// token in the log. Other pairs stay byte-identical.
fn redact_url_params(word: &str) -> Option<String> {
    if !word.contains("://") {
        return None;
    }
    let start = word.find(['?', '#'])? + 1;
    let tail = &word[start..];
    let mut out = String::from(&word[..start]);
    let mut changed = false;
    let mut pair_start = 0;
    // A `#` ends the query's last pair and starts the fragment's first one.
    for (i, sep) in tail
        .match_indices(['&', '#'])
        .map(|(i, s)| (i, Some(s)))
        .chain([(tail.len(), None)])
    {
        let pair = &tail[pair_start..i];
        match pair.split_once('=') {
            Some((name, value)) if !value.is_empty() && is_url_secret_name(name) => {
                out.push_str(name);
                out.push('=');
                out.push_str(MASK);
                changed = true;
            }
            _ => out.push_str(pair),
        }
        out.push_str(sep.unwrap_or_default());
        pair_start = i + 1;
    }
    changed.then_some(out)
}

/// Whether a URL parameter name marks a credential. A percent-encoded name (`%63ode`) is decoded
/// first: the browser decodes it the same way before the server reads it.
fn is_url_secret_name(name: &str) -> bool {
    let decoded = percent_decoded(name);
    let name = decoded.as_str();
    is_secret_name(name)
        || URL_SECRET_PARAMS
            .iter()
            .any(|p| name.eq_ignore_ascii_case(p))
}

/// `%XX` escapes decoded (anything that does not form valid UTF-8 stays as written).
fn percent_decoded(raw: &str) -> String {
    if !raw.contains('%') {
        return raw.to_string();
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
                i += 3;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

/// `text` with credentials replaced by [`MASK`].
///
/// Line-oriented so an env dump (`printenv`, a failing CI step echoing its config) is handled one
/// assignment at a time, and so a match can never swallow the rest of the output.
#[must_use]
pub fn redact_secrets(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // `\r\n` survives: the scan works on the line content and the carriage return rides back.
        if let Some(stripped) = line.strip_suffix('\r') {
            out.push_str(&redact_line(stripped));
            out.push('\r');
        } else {
            out.push_str(&redact_line(line));
        }
    }
    out
}

/// Whether `text` carries a credential by its shape alone: a PEM private key, or a token with a
/// known provider prefix (rule 1) that holds a digit.
///
/// The assignment-name rule is left out on purpose: UI source declares `password: string` in
/// every sign-in form. The digit keeps kebab-case class names that happen to start with a prefix
/// (`sk-loading-spinner-wrapper`) out; provider keys are random.
#[must_use]
pub fn holds_provider_secret(text: &str) -> bool {
    text.contains(concat!("PRIVATE", " KEY-----"))
        || text
            .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .any(|word| word.bytes().any(|b| b.is_ascii_digit()) && is_provider_token(word))
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
    fn masks_provider_tokens_and_keeps_their_punctuation() {
        let out = redact_secrets(concat!(
            "auth with \"gh",
            "p_abcdefghijklmnopqrstuvwxyz0123\" done"
        ));
        assert_eq!(
            out, "auth with \"[redacted]\" done",
            "the quotes are output, not secret: {out}"
        );

        let aws = redact_secrets(concat!("id AKIA", "IOSFODNN7EXAMPLE, region us-east-1"));
        assert_eq!(aws, "id [redacted], region us-east-1");
    }

    /// The whole reason this is prefix-based: ordinary build output must survive untouched.
    #[test]
    fn leaves_ordinary_output_alone() {
        for line in [
            "error[E0308]: mismatched types",
            "sk-init ran 12 checks",
            "Compiling ch-core v0.1.0",
            "C:\\Users\\octo\\keyboard.json written",
            "at src/main.rs:42:17",
            "https://github.com/octo/repo.git fetched",
        ] {
            assert_eq!(redact_secrets(line), line, "must not touch: {line}");
        }
    }

    #[test]
    fn masks_a_credential_assignment_but_keeps_the_name() {
        assert_eq!(
            redact_secrets("OPENAI_API_KEY=sk-short"),
            "OPENAI_API_KEY=[redacted]"
        );
        assert_eq!(
            redact_secrets("  authToken: hunter2"),
            "  authToken: [redacted]"
        );
        assert_eq!(
            redact_secrets("\"password\": \"hunter2\","),
            "\"password\": [redacted],"
        );
        // The name says nothing about credentials: the value is data, not a secret.
        assert_eq!(redact_secrets("retries=3"), "retries=3");
        // An empty value is not a secret; masking it would claim a key exists.
        assert_eq!(redact_secrets("API_KEY="), "API_KEY=");
        // A matched name is authoritative: a short or all-digit value is still the credential
        // the name says it is.
        assert_eq!(redact_secrets("PASSWORD=123456"), "PASSWORD=[redacted]");
        assert_eq!(redact_secrets("token=abc"), "token=[redacted]");
    }

    /// Shapes a substring test destroyed: a diagnostic, an author line, a token count.
    #[test]
    fn never_masks_output_that_merely_mentions_a_credential_word() {
        for line in [
            "Author: Jane Doe",
            "total_tokens: 1234",
            "src/commands/secrets.rs:44: error[E0425]: cannot find value `store`",
            "keyboard_layout: us",
            "authors = [\"octo\"]",
            "tokens_used: 91",
        ] {
            assert_eq!(redact_secrets(line), line, "must not touch: {line}");
        }
    }

    #[test]
    fn masks_url_userinfo_and_keeps_the_user() {
        assert_eq!(
            redact_secrets("cloning https://ci-bot:s3cr3t@github.com/acme/app.git"),
            "cloning https://ci-bot:[redacted]@github.com/acme/app.git"
        );
        // No password component: nothing to mask, and the user is not a credential.
        assert_eq!(
            redact_secrets("ssh://git@github.com/acme/app.git"),
            "ssh://git@github.com/acme/app.git"
        );
    }

    /// A credential riding in a URL's query or fragment is masked with the parameter's name
    /// kept; every other parameter stays, because it says where the page is.
    #[test]
    fn masks_url_parameters_named_as_credentials() {
        assert_eq!(
            redact_secrets(
                "navigated to http://127.0.0.1:5173/cb?code=4/0Ab12&state=xyz#access_token=eyJhbGciOi&expires_in=3600"
            ),
            "navigated to http://127.0.0.1:5173/cb?code=[redacted]&state=xyz#access_token=[redacted]&expires_in=3600"
        );
        assert_eq!(
            redact_secrets(
                "open https://bucket.example.com/f.zip?X-Amz-Signature=abc123&X-Amz-Expires=60"
            ),
            "open https://bucket.example.com/f.zip?X-Amz-Signature=[redacted]&X-Amz-Expires=60"
        );
        assert_eq!(
            redact_secrets("https://ci-bot:s3cr3t@example.com/hook?password=x"),
            "https://ci-bot:[redacted]@example.com/hook?password=[redacted]"
        );
        // Signed links, session ids, SAML and a percent-encoded name.
        assert_eq!(
            redact_secrets(
                "https://storage.googleapis.com/b/o?X-Goog-Signature=ab12&X-Goog-Date=20261025"
            ),
            "https://storage.googleapis.com/b/o?X-Goog-Signature=[redacted]&X-Goog-Date=20261025"
        );
        assert_eq!(
            redact_secrets(
                "http://localhost:8080/app?sid=9f8e&jsessionid=AB12&SAMLResponse=PHNhbWw&pwd=x&ticket=ST-1&%63ode=z"
            ),
            "http://localhost:8080/app?sid=[redacted]&jsessionid=[redacted]&SAMLResponse=[redacted]&pwd=[redacted]&ticket=[redacted]&%63ode=[redacted]"
        );
        for line in [
            "http://localhost:3000/items?page=2&sort=name",
            "https://example.com/search?q=token+limits",
            "see https://x.io/a?tokens=5&token=",
            "not a url ?code=1",
        ] {
            assert_eq!(redact_secrets(line), line, "must not touch: {line}");
        }
    }

    /// `Bearer <token>` wherever it sits on a line. Inside a curl header the assignment rule
    /// never fires (the text left of the first colon is `curl -H "Authorization`, not a bare
    /// name), and a JWT carries no provider prefix. The scheme word stays.
    #[test]
    fn masks_a_bearer_token_and_keeps_the_scheme() {
        assert_eq!(
            redact_secrets(concat!(
                r#"curl -H "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9."#,
                r#"eyJzdWIiOiIxMjMifQ.c2lnbmF0dXJl" https://api.example.com/v1"#
            )),
            r#"curl -H "Authorization: Bearer [redacted]" https://api.example.com/v1"#
        );
        assert_eq!(
            redact_secrets("proxy got bearer AbCdEfGh12345678IjKl, retrying"),
            "proxy got bearer [redacted], retrying"
        );
    }

    /// Prose about the scheme is not a credential: the next word must look like a token (long,
    /// token characters only, and a digit or mixed case), or the line is left alone.
    #[test]
    fn leaves_prose_about_bearer_auth_alone() {
        for line in [
            "Bearer authentication is required",
            "error: expected a Bearer token in the Authorization header",
            "Bearer authorization-header-required",
            "Bearer",
        ] {
            assert_eq!(redact_secrets(line), line, "must not touch: {line}");
        }
    }

    #[test]
    fn preserves_line_structure_including_crlf_and_empties() {
        let input = concat!("one\r\nAPI_KEY=sk", "-aaaaaaaaaaaaaaaaaaaaaaaa\r\n\r\ntwo");
        let out = redact_secrets(input);
        assert_eq!(
            out, "one\r\nAPI_KEY=[redacted]\r\n\r\ntwo",
            "line shape must survive: {out:?}"
        );
        assert_eq!(redact_secrets(""), "");
    }

    /// `> PASSWORD` is not a bare name, so the quote marker has to be read past, at any depth,
    /// or quoting would turn the assignment rule off.
    #[test]
    fn masks_an_assignment_inside_a_quote() {
        assert_eq!(
            redact_secrets("> PASSWORD=hunter2"),
            "> PASSWORD=[redacted]"
        );
        assert_eq!(
            redact_secrets("> > api_key: abc"),
            "> > api_key: [redacted]"
        );
        assert_eq!(
            redact_secrets(">TOKEN=abc\n>\n> retries=3"),
            ">TOKEN=[redacted]\n>\n> retries=3"
        );
    }

    /// Webhook and bot tokens ride in the URL path; only the token goes. Made-up values only.
    #[test]
    fn masks_a_token_in_a_webhook_or_bot_url_path() {
        for (line, want) in [
            (
                "curl -X POST https://hooks.slack.com/services/T0FAKE/B0FAKE/notarealtoken -d @msg.json",
                "curl -X POST https://hooks.slack.com/services/T0FAKE/B0FAKE/[redacted] -d @msg.json",
            ),
            (
                "hooks.slack.com/services/T0FAKE/B0FAKE/notreal?x=1",
                "hooks.slack.com/services/T0FAKE/B0FAKE/[redacted]?x=1",
            ),
            (
                "\"https://discord.com/api/webhooks/1234/fake-token_x\"",
                "\"https://discord.com/api/webhooks/1234/[redacted]\"",
            ),
            (
                "https://discordapp.com/api/v10/webhooks/1234/fake/slack",
                "https://discordapp.com/api/v10/webhooks/1234/[redacted]/slack",
            ),
            (
                "GET https://api.telegram.org/bot12345:fake-token/getMe",
                "GET https://api.telegram.org/bot12345:[redacted]/getMe",
            ),
            (
                "https://api.telegram.org/file/bot12345:fake/doc.pdf",
                "https://api.telegram.org/file/bot12345:[redacted]/doc.pdf",
            ),
        ] {
            assert_eq!(redact_secrets(line), want, "{line}");
        }
        for line in [
            "https://hooks.slack.com/services/",
            "https://hooks.slack.com/services/T0FAKE/B0FAKE",
            "https://hooks.slack.com/services/T0FAKE/B0FAKE/",
            "https://discord.com/api/webhooks",
            "https://discord.com/api/webhooks/1234",
            "https://discord.com/api/v10/channels/1234/messages",
            "https://api.telegram.org/bot",
            "https://api.telegram.org/botfather",
            "https://example.com/services/T0FAKE/B0FAKE/page",
            "see docs at api.telegram.org/bots/api",
        ] {
            assert_eq!(redact_secrets(line), line, "must not touch: {line}");
        }
    }

    /// Credentials passed as command-line flags or the clients' own env variables. The command,
    /// the flag, the variable and the user stay readable.
    #[test]
    fn masks_a_password_passed_as_a_flag_or_env_variable() {
        for (line, want) in [
            (
                "curl -u admin:notreal https://example.com",
                "curl -u admin:[redacted] https://example.com",
            ),
            (
                "curl --user admin:notreal https://example.com",
                "curl --user admin:[redacted] https://example.com",
            ),
            (
                "curl -uadmin:notreal https://example.com",
                "curl -uadmin:[redacted] https://example.com",
            ),
            (
                "/usr/bin/curl -s --user=admin:notreal https://x.io",
                "/usr/bin/curl -s --user=admin:[redacted] https://x.io",
            ),
            (
                "curl.exe -u 'admin:notreal' https://x.io",
                "curl.exe -u 'admin:[redacted]' https://x.io",
            ),
            (
                "mysql -uroot -pnotreal app",
                "mysql -uroot -p[redacted] app",
            ),
            (
                "mysqldump -h db -u root -pnotreal app",
                "mysqldump -h db -u root -p[redacted] app",
            ),
            (
                "mysql -u root --password=notreal app",
                "mysql -u root --password=[redacted] app",
            ),
            (
                "PGPASSWORD=notreal psql -h db -U app",
                "PGPASSWORD=[redacted] psql -h db -U app",
            ),
            ("export PGPASSWORD=notreal", "export PGPASSWORD=[redacted]"),
            (
                "MYSQL_PWD=notreal mysql -u root",
                "MYSQL_PWD=[redacted] mysql -u root",
            ),
            (
                "$env:PGPASSWORD=\"notreal\"; psql -h db",
                "$env:PGPASSWORD=[redacted]\"; psql -h db",
            ),
        ] {
            assert_eq!(redact_secrets(line), want, "{line}");
        }
        for line in [
            // No colon: curl asks for the password, so the word holds none.
            "curl -u admin https://example.com",
            "curl --user admin https://example.com",
            "curl -u admin: https://example.com",
            // A bare `-p` asks at the prompt; `-P` is the port.
            "mysql -u root -p app",
            "mysql -u root -P 3306 app",
            // `-u` and `-p` of other commands are not credentials.
            "git push -u origin main:main",
            "sort -u a:b",
            "mkdir -p src/a",
            "psql -h db -U app -p 5432",
            "cargo run -- --max-tokens=100 --tokens=5",
            // The flag names where the secret is kept; the path is not the secret.
            "cargo test --password-file=none",
            "tool --token-path=/run/secrets/x",
            "PGHOST=db psql",
            "mysql --password= app",
        ] {
            assert_eq!(redact_secrets(line), line, "must not touch: {line}");
        }
    }
}
