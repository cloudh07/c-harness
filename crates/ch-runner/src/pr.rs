//! Opening a GitHub pull request after the push.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use serde_json::{Value, json};

use crate::proc_tree;
use crate::tail::cut_tail;

const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "c-harness";
const MAX_TIME_SECS: u32 = 60;
const STDERR_TAIL_BYTES: usize = 2048;
const HTTP_CREATED: &str = "201";

/// What to open: `head` is the branch just pushed to `repo`.
pub struct PrRequest<'a> {
    pub repo: &'a str,
    pub head: &'a str,
    pub base: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub token: Option<&'a str>,
}

impl fmt::Debug for PrRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrRequest")
            .field("repo", &self.repo)
            .field("head", &self.head)
            .field("base", &self.base)
            .field("title", &self.title)
            .field("token", &self.token.map(|_| crate::scrub::HIDDEN))
            .finish_non_exhaustive()
    }
}

pub trait PrOpener: Send {
    /// Whether [`PrOpener::open`] needs `PrRequest::token`; checked before anything is pushed.
    fn requires_token(&self) -> bool {
        true
    }

    /// Opens the pull request and returns its web URL. `scratch` is a private directory
    /// outside the working tree that is removed afterwards.
    fn open(&self, request: &PrRequest<'_>, scratch: &Path) -> Result<String, String>;
}

/// Calls the GitHub REST API through the system `curl`, with the token on curl's stdin.
#[derive(Debug, Default)]
pub struct CurlPrOpener;

impl PrOpener for CurlPrOpener {
    fn open(&self, request: &PrRequest<'_>, scratch: &Path) -> Result<String, String> {
        let token = request
            .token
            .ok_or("opening a pull request needs a token")?;
        let body_path = scratch.join("pr-request.json");
        let response_path = scratch.join("pr-response.json");
        write_private(&body_path, request_body(request).as_bytes())
            .map_err(|e| format!("could not write the pull request body: {e}"))?;
        let config = curl_config(request.repo, token, &body_path, &response_path)?;

        let mut child = proc_tree::command("curl")
            // `-q` must come first: it stops curl reading a `.curlrc` that could send the
            // request, token included, somewhere else.
            .args(["-q", "-sS", "-K", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run curl: {e}"))?;
        let written = child
            .stdin
            .take()
            .map(|mut stdin| stdin.write_all(config.as_bytes()));
        let output = child
            .wait_with_output()
            .map_err(|e| format!("curl did not finish: {e}"))?;
        if let Some(Err(e)) = written {
            return Err(format!("could not pass the request to curl: {e}"));
        }
        if !output.status.success() {
            let (tail, _) = cut_tail(&output.stderr, STDERR_TAIL_BYTES);
            return Err(format!(
                "curl could not reach GitHub: {}",
                tail.trim().trim_start_matches("curl: ")
            ));
        }
        let status = String::from_utf8_lossy(&output.stdout);
        let response = fs::read(&response_path).unwrap_or_default();
        parse_response(status.trim(), &response)
    }
}

fn request_body(request: &PrRequest<'_>) -> String {
    json!({
        "title": request.title,
        "head": request.head,
        "base": request.base,
        "body": request.body,
    })
    .to_string()
}

/// A curl config file (the `-K` format) for the request.
pub fn curl_config(
    repo: &str,
    token: &str,
    body: &Path,
    response: &Path,
) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{repo}/pulls");
    let body = format!("@{}", path_text(body)?);
    let response = path_text(response)?;
    let lines = [
        format!("url = {}", quote_curl(&url)?),
        format!(
            "header = {}",
            quote_curl(&format!("Authorization: Bearer {token}"))?
        ),
        format!(
            "header = {}",
            quote_curl("Accept: application/vnd.github+json")?
        ),
        format!(
            "header = {}",
            quote_curl(&format!("X-GitHub-Api-Version: {API_VERSION}"))?
        ),
        format!(
            "header = {}",
            quote_curl(&format!("User-Agent: {USER_AGENT}"))?
        ),
        format!("header = {}", quote_curl("Content-Type: application/json")?),
        format!("data-binary = {}", quote_curl(&body)?),
        format!("output = {}", quote_curl(response)?),
        format!("write-out = {}", quote_curl("%{http_code}")?),
        format!("proto = {}", quote_curl("=https")?),
        format!("max-time = {MAX_TIME_SECS}"),
        String::new(),
    ];
    Ok(lines.join("\n"))
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("the temp path {} is not valid UTF-8", path.display()))
}

/// A double-quoted curl config value. curl reads `\\` and `\"` as escapes inside quotes; a line
/// break cannot be written safely, so it is refused rather than escaped.
pub fn quote_curl(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("a curl option value holds a control character".to_owned());
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if matches!(c, '\\' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}

/// The pull request URL from GitHub's answer, or why there is none.
pub fn parse_response(status: &str, body: &[u8]) -> Result<String, String> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    if status == HTTP_CREATED {
        return json
            .as_ref()
            .and_then(|v| v.get("html_url"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "GitHub created the pull request but sent no html_url".to_owned());
    }
    let message = json
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str);
    let detail = json
        .as_ref()
        .and_then(|v| v.get("errors"))
        .and_then(|errors| errors.get(0))
        .and_then(|first| first.get("message"))
        .and_then(Value::as_str);
    let mut text = format!("GitHub returned {status}");
    for part in [message, detail].into_iter().flatten() {
        text.push_str(": ");
        text.push_str(part);
    }
    Err(text)
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_backslashes_and_quotes() {
        assert_eq!(quote_curl(r#"a\b"c"#).unwrap(), r#""a\\b\"c""#);
        assert_eq!(
            quote_curl(r"C:\Users\x\body.json").unwrap(),
            r#""C:\\Users\\x\\body.json""#
        );
    }

    #[test]
    fn refuses_line_breaks_in_values() {
        assert!(quote_curl("a\nurl = \"https://evil.example\"").is_err());
        assert!(quote_curl("a\rb").is_err());
        assert!(quote_curl("a\0b").is_err());
    }

    #[test]
    fn config_carries_the_token_only_in_a_quoted_header() {
        let config = curl_config(
            "octo/repo",
            "test-token-not-real",
            Path::new("/private/body.json"),
            Path::new("/private/out.json"),
        )
        .unwrap();
        let lines: Vec<&str> = config.lines().collect();
        assert_eq!(
            lines[0],
            r#"url = "https://api.github.com/repos/octo/repo/pulls""#
        );
        assert_eq!(
            lines[1],
            r#"header = "Authorization: Bearer test-token-not-real""#
        );
        assert!(lines.contains(&r#"data-binary = "@/private/body.json""#));
        assert!(lines.contains(&r#"write-out = "%{http_code}""#));
        assert!(lines.contains(&r#"proto = "=https""#));
        assert_eq!(config.matches("test-token-not-real").count(), 1);
    }

    #[test]
    fn reads_the_url_of_a_created_pull_request() {
        let body = br#"{"html_url":"https://github.com/octo/repo/pull/7","number":7}"#;
        assert_eq!(
            parse_response("201", body),
            Ok("https://github.com/octo/repo/pull/7".to_owned())
        );
        assert!(parse_response("201", b"not json").is_err());
    }

    #[test]
    fn reports_githubs_reason_for_a_refusal() {
        let body = br#"{"message":"Validation Failed","errors":[{"resource":"PullRequest","code":"custom","message":"A pull request already exists for octo:ch/s1."}]}"#;
        assert_eq!(
            parse_response("422", body),
            Err(
                "GitHub returned 422: Validation Failed: A pull request already exists for octo:ch/s1."
                    .to_owned()
            )
        );
        assert_eq!(
            parse_response("000", b""),
            Err("GitHub returned 000".to_owned())
        );
    }

    #[test]
    fn debug_hides_the_token() {
        let request = PrRequest {
            repo: "octo/repo",
            head: "ch/s1",
            base: "main",
            title: "t",
            body: "b",
            token: Some("test-token-not-real"),
        };
        let text = format!("{request:?}");
        assert!(!text.contains("test-token-not-real"));
        assert!(text.contains("<hidden>"));
    }
}
