//! Pre-release audit: what omp puts in the headers of a model request. It needs a real omp, so it
//! is ignored in the normal test run. Before each release:
//!
//! ```text
//! CH_OMP=<path to omp> cargo test -p ch-runner --test omp_headers -- --ignored --nocapture --test-threads=1
//! ```
//!
//! omp gets a fake key, a temporary home and a model provider at a listener on 127.0.0.1. Any
//! other connection goes to a local proxy that refuses it and is printed. The listener keeps
//! header names, the values of the few headers in `SHOW_VALUE` and the names of the body's
//! top-level fields, then compares them with `testdata/omp/headers-*.txt`. Values of keys and of
//! headers that link requests are never kept. `CH_BLESS=1` rewrites the files after a review.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const FAKE_KEY: &str = "ch-header-audit-fake-key";
const MARKER: &str = "ch header audit marker";
const WAIT: Duration = Duration::from_secs(90);
/// Requests omp sends around the prompt, such as a model list, can land just after it.
const GRACE: Duration = Duration::from_secs(3);

/// Headers whose values say what omp is, not who runs it.
const SHOW_VALUE: &[&str] = &[
    "accept",
    "accept-encoding",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "anthropic-version",
    "content-type",
    "user-agent",
    "x-app",
];

/// Header and body field names that suggest an id tying requests to a person, machine or session.
const LINKING_WORDS: &[&str] = &[
    "session",
    "user",
    "machine",
    "device",
    "account",
    "org",
    "client",
    "request-id",
    "install",
    "cache_key",
];

#[derive(Clone, Copy)]
enum Provider {
    Anthropic,
    OpenAi,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }

    fn model(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic/claude-sonnet-4-5",
            Self::OpenAi => "openai/gpt-5",
        }
    }
}

#[test]
#[ignore = "needs omp; run before each release"]
fn anthropic_requests_match_the_fixture() {
    audit(Provider::Anthropic);
}

#[test]
#[ignore = "needs omp; run before each release"]
fn openai_requests_match_the_fixture() {
    audit(Provider::OpenAi);
}

fn listed_for(provider: Provider) -> Vec<String> {
    ch_core::STARTUP_HOSTS
        .iter()
        .filter(|(_, only)| only.is_none_or(|name| name == provider.name()))
        .map(|(host, _)| format!("{host}:443"))
        .collect()
}

#[test]
fn a_proxy_target_is_reduced_to_its_host() {
    assert_eq!(host_of("api.kilo.ai:443"), "api.kilo.ai");
    assert_eq!(host_of("http://zenmux.ai/v1/models"), "zenmux.ai");
    assert_eq!(
        host_of("https://hyper.charm.land:8443/x"),
        "hyper.charm.land"
    );
}

#[test]
fn the_listed_startup_hosts_pass_the_host_check() {
    for provider in [Provider::Anthropic, Provider::OpenAi] {
        check_hosts(provider, &listed_for(provider));
    }
}

#[test]
#[should_panic(expected = "does not list: [\"telemetry.example.com\"]")]
fn an_unlisted_host_fails_the_host_check() {
    let mut proxied = listed_for(Provider::OpenAi);
    proxied.push("telemetry.example.com:443".to_owned());
    check_hosts(Provider::OpenAi, &proxied);
}

#[test]
#[should_panic(expected = "skipped ones it lists: [\"api.anthropic.com\"]")]
fn a_listed_host_omp_skipped_fails_the_host_check() {
    check_hosts(Provider::Anthropic, &listed_for(Provider::OpenAi));
}

#[test]
fn a_header_carrying_the_home_folder_is_a_finding() {
    let (kind, home) = local_names()
        .into_iter()
        .find(|(kind, _)| *kind == "home folder path")
        .expect("HOME or USERPROFILE is set");
    let seen = Seen {
        method: "POST".to_owned(),
        path: "/v1/messages".to_owned(),
        headers: vec![("x-cwd".to_owned(), format!("{home}/project"))],
        body: Vec::new(),
    };
    let found = findings(&[seen]);
    let want = format!("POST /v1/messages: header x-cwd carries the {kind}");
    assert!(found.contains(&want), "{found:?}");
}

struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn is_prompt(&self) -> bool {
        String::from_utf8_lossy(&self.body).contains(MARKER)
    }
}

enum Event {
    Model(Seen),
    Proxy(String),
}

fn audit(provider: Provider) {
    let omp = std::env::var_os("CH_OMP").map_or_else(|| PathBuf::from("omp"), PathBuf::from);
    let version = omp_version(&omp);
    let root = scratch_root();
    let (home, agent, cwd) = (
        root.path().join("home"),
        root.path().join("agent"),
        root.path().join("cwd"),
    );
    for dir in [&home, &agent, &cwd] {
        std::fs::create_dir_all(dir).expect("create audit folders");
    }

    let (tx, rx) = mpsc::channel();
    let base = format!("http://{}", serve(listen(), tx.clone(), handle_model));
    let proxy = format!("http://{}", serve(listen(), tx, handle_proxy));
    let overlay = agent.join("host.yml");
    std::fs::write(&overlay, ch_protocol::host_config_overlay(&[])).expect("write host.yml");

    let mut command = Command::new(&omp);
    command
        .args([
            "--mode",
            "rpc",
            "--approval-mode",
            "write",
            "--model",
            provider.model(),
        ])
        .arg("--config")
        .arg(&overlay)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    keep_only_safe_env(&mut command, root.path());
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost");
    for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
        command.env(name, &proxy);
    }
    match provider {
        Provider::Anthropic => {
            command
                .env("ANTHROPIC_BASE_URL", &base)
                .env("ANTHROPIC_API_KEY", FAKE_KEY);
        }
        Provider::OpenAi => {
            let models = format!("providers:\n  openai:\n    baseUrl: {base}/v1\n");
            std::fs::write(agent.join("models.yml"), models).expect("write models.yml");
            command.env("OPENAI_API_KEY", FAKE_KEY);
        }
    }

    let mut child = KillOnDrop(
        command
            .spawn()
            .unwrap_or_else(|e| panic!("start {}: {e}", omp.display())),
    );
    let output = drain(&mut child.0);
    let mut stdin = child.0.stdin.take().expect("omp stdin");
    let frame = serde_json::json!({ "id": "audit", "type": "prompt", "message": MARKER });
    writeln!(stdin, "{frame}").expect("send the prompt");

    let (requests, proxied) = collect(&rx);
    drop(child);
    drop(stdin);
    for host in &proxied {
        println!("[{}] refused a connection to {host}", provider.name());
    }
    if !requests.iter().any(Seen::is_prompt) {
        let tail = output
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_default();
        panic!("omp never sent the prompt to the listener. Its output:\n{tail}");
    }

    let report = render(&version, provider, &requests);
    let found = findings(&requests);
    for line in &found {
        println!("[{}] FINDING: {line}", provider.name());
    }
    compare(provider, &report);
    check_hosts(provider, &proxied);
    assert!(
        found.is_empty(),
        "omp's {} requests carry who or where you are:\n{}",
        provider.name(),
        found.join("\n")
    );
    assert_eq!(
        version,
        ch_core::OMP_VERSION_SEEN,
        "omp is {version} but PRIVACY.md was checked against {}. Review the outbound list, then \
         update OMP_VERSION_SEEN in crates/ch-core/src/outbound.rs.",
        ch_core::OMP_VERSION_SEEN
    );
}

fn omp_version(omp: &Path) -> String {
    let out = Command::new(omp)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "run {} --version: {e}. Set CH_OMP to omp's path.",
                omp.display()
            )
        });
    let raw = String::from_utf8_lossy(&out.stdout);
    ch_protocol::parse_version(&raw).unwrap_or_else(|| panic!("no version in {raw:?}"))
}

/// omp looks for `.claude/skills` in every folder above its working folder and sends what it
/// finds to the model, so the scratch folder must sit outside the real home.
fn scratch_root() -> tempfile::TempDir {
    let base = std::env::var_os("CH_AUDIT_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    std::fs::create_dir_all(&base).expect("create the audit folder");
    let root = tempfile::Builder::new()
        .prefix("omp-headers-")
        .tempdir_in(&base)
        .expect("make a scratch folder");
    for name in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(name).filter(|h| !h.is_empty()) {
            assert!(
                !root.path().starts_with(&home),
                "{} is inside your home folder; set CH_AUDIT_DIR to a folder outside it",
                root.path().display()
            );
        }
    }
    root
}

/// Kills omp when the audit ends, a failed assert included.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Variables omp may see from the real environment, upper-cased. Windows spells some of them in
/// mixed case, such as `Path`. The user and computer names stay so `findings` has something to
/// look for.
const KEEP_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "LANG",
    "TERM",
    "USERNAME",
    "USER",
    "LOGNAME",
    "COMPUTERNAME",
    "HOSTNAME",
];

/// Starts omp from an empty environment, so no real key, proxy, config folder or telemetry
/// setting reaches it, whatever it is called. Folders omp may write to point into `scratch`.
fn keep_only_safe_env(command: &mut Command, scratch: &Path) {
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if KEEP_ENV.contains(&upper.as_str()) || upper.starts_with("LC_") {
            command.env(name, value);
        }
    }
    let folders = [
        (&["TEMP", "TMP", "TMPDIR"][..], "tmp"),
        (&["APPDATA"], "appdata"),
        (&["LOCALAPPDATA"], "localappdata"),
        (&["XDG_CONFIG_HOME"], "xdg-config"),
        (&["XDG_CACHE_HOME"], "xdg-cache"),
        (&["XDG_DATA_HOME"], "xdg-data"),
        (&["XDG_STATE_HOME"], "xdg-state"),
    ];
    for (names, folder) in folders {
        let dir = scratch.join(folder);
        std::fs::create_dir_all(&dir).expect("create a scratch folder");
        for name in names {
            command.env(name, &dir);
        }
    }
}

/// Reads omp's output so it never blocks on a full pipe. The text comes back once omp exits,
/// with the fake key masked.
fn drain(child: &mut Child) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let mut stdout = child.stdout.take().expect("omp stdout");
    let mut stderr = child.stderr.take().expect("omp stderr");
    thread::spawn(move || {
        let err = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text.push_str(&err.join().unwrap_or_default());
        let start = text.len().saturating_sub(4000);
        let start = (start..text.len())
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        let _ = tx.send(text[start..].replace(FAKE_KEY, "<fake key>"));
    });
    rx
}

fn collect(rx: &Receiver<Event>) -> (Vec<Seen>, Vec<String>) {
    let deadline = Instant::now() + WAIT;
    let mut stop = deadline;
    let (mut requests, mut proxied) = (Vec::new(), Vec::new());
    while let Some(left) = stop.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Model(seen)) => {
                if seen.is_prompt() && stop == deadline {
                    stop = Instant::now() + GRACE;
                }
                requests.push(seen);
            }
            Ok(Event::Proxy(host)) => proxied.push(host),
            Err(_) => break,
        }
    }
    proxied.sort();
    proxied.dedup();
    (requests, proxied)
}

/// The host in a proxy request's target: `host:443` from a CONNECT, or `http://host/path`.
fn host_of(target: &str) -> &str {
    let rest = target.split_once("://").map_or(target, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or(rest);
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
}

/// Fails when omp connected to a host `ch_core::STARTUP_HOSTS` does not list for this provider,
/// or never connected to one it does, so PRIVACY.md cannot fall behind omp.
fn check_hosts(provider: Provider, proxied: &[String]) {
    let seen: BTreeSet<&str> = proxied.iter().map(|target| host_of(target)).collect();
    let want: BTreeSet<&str> = ch_core::STARTUP_HOSTS
        .iter()
        .filter(|(_, only)| only.is_none_or(|name| name == provider.name()))
        .map(|(host, _)| *host)
        .collect();
    let unlisted: Vec<&str> = seen.difference(&want).copied().collect();
    let missing: Vec<&str> = want.difference(&seen).copied().collect();
    assert!(
        unlisted.is_empty() && missing.is_empty(),
        "omp's {} run connected to hosts STARTUP_HOSTS does not list: {unlisted:?}, and skipped \
         ones it lists: {missing:?}. Update STARTUP_HOSTS and the model-catalogs row in \
         crates/ch-core/src/outbound.rs, then PRIVACY.md.",
        provider.name()
    );
}

fn listen() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1")
}

fn serve(
    listener: TcpListener,
    tx: Sender<Event>,
    handle: fn(TcpStream, &Sender<Event>),
) -> String {
    let addr = listener.local_addr().expect("listener address").to_string();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            thread::spawn(move || handle(stream, &tx));
        }
    });
    addr
}

/// Answers nothing to the prompt, so omp never gets an error to retry or log, and refuses
/// everything else with a status omp does not dump to disk.
fn handle_model(stream: TcpStream, tx: &Sender<Event>) {
    let mut reader = BufReader::new(stream);
    let Some(seen) = read_request(&mut reader) else {
        return;
    };
    let prompt = seen.is_prompt();
    let status = if seen.method == "GET" {
        "404 Not Found"
    } else {
        "401 Unauthorized"
    };
    if tx.send(Event::Model(seen)).is_err() || prompt {
        let mut sink = Vec::new();
        let _ = reader.read_to_end(&mut sink);
        return;
    }
    let body =
        r#"{"type":"error","error":{"type":"authentication_error","message":"header audit"}}"#;
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = reader.get_mut().write_all(reply.as_bytes());
}

fn handle_proxy(stream: TcpStream, tx: &Sender<Event>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_ok() {
        let target = line.split_whitespace().nth(1).unwrap_or("?").to_string();
        let _ = tx.send(Event::Proxy(target));
    }
    let _ = reader
        .get_mut()
        .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n");
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Seen> {
    let mut line = String::new();
    reader.read_line(&mut line).ok().filter(|n| *n > 0)?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.split('?').next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok().filter(|n| *n > 0)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let body = if header("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        read_chunked(reader)?
    } else {
        let len = header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        reader.read_exact(&mut body).ok()?;
        body
    };
    Some(Seen {
        method,
        path,
        headers,
        body,
    })
}

fn read_chunked(reader: &mut BufReader<TcpStream>) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        reader.read_line(&mut size).ok()?;
        let size = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
        let mut chunk = vec![0; size + 2];
        reader.read_exact(&mut chunk).ok()?;
        if size == 0 {
            return Some(body);
        }
        body.extend_from_slice(&chunk[..size]);
    }
}

fn links(name: &str) -> bool {
    LINKING_WORDS.iter().any(|w| name.contains(w))
}

/// One block per kind of request, first one seen wins.
fn render(version: &str, provider: Provider, requests: &[Seen]) -> String {
    let mut blocks = BTreeMap::new();
    for seen in requests {
        let prompt = if seen.is_prompt() {
            " (the prompt)"
        } else {
            ""
        };
        let label = format!("{} {}{prompt}", seen.method, seen.path);
        blocks.entry(label).or_insert_with(|| block(seen));
    }
    let mut out = vec![
        format!("omp {version}"),
        format!("model {}", provider.model()),
    ];
    for (label, lines) in blocks {
        out.push(String::new());
        out.push(label);
        out.extend(lines);
    }
    out.push(String::new());
    out.join("\n")
}

fn block(seen: &Seen) -> Vec<String> {
    let mut names: Vec<&(String, String)> = seen.headers.iter().collect();
    names.sort();
    names.dedup_by(|a, b| a.0 == b.0);
    let mut out: Vec<String> = names
        .into_iter()
        .map(|(name, value)| {
            if SHOW_VALUE.contains(&name.as_str()) {
                format!("  {name}: {value}")
            } else if links(name) {
                format!("  {name}  [links requests; value not recorded]")
            } else {
                format!("  {name}")
            }
        })
        .collect();
    let fields = body_fields(&seen.body);
    if !fields.is_empty() {
        out.push(format!("  body: {}", fields.join(", ")));
    }
    out
}

/// Top-level field names, with the names inside `metadata`. Never values.
fn body_fields(body: &[u8]) -> Vec<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice(body) else {
        return Vec::new();
    };
    let mut fields: Vec<String> = map
        .iter()
        .map(|(key, value)| match (key.as_str(), value) {
            ("metadata", serde_json::Value::Object(inner)) => {
                let inner: Vec<String> = inner
                    .keys()
                    .map(|k| {
                        if links(k) {
                            format!("{k} [links requests]")
                        } else {
                            k.clone()
                        }
                    })
                    .collect();
                format!("metadata{{{}}}", inner.join(", "))
            }
            (key, _) if links(key) => format!("{key} [links requests]"),
            (key, _) => key.to_string(),
        })
        .collect();
    fields.sort();
    fields
}

/// Header values that carry who or where: the user name, the computer name or a local path.
/// Prints which header and what kind, never the value.
fn findings(requests: &[Seen]) -> Vec<String> {
    let mut out = Vec::new();
    for (kind, needle) in local_names() {
        for seen in requests {
            for (name, value) in &seen.headers {
                if contains_word(value, &needle) {
                    out.push(format!(
                        "{} {}: header {name} carries the {kind}",
                        seen.method, seen.path
                    ));
                }
            }
            if seen.is_prompt() && contains_word(&String::from_utf8_lossy(&seen.body), &needle) {
                out.push(format!(
                    "{} {}: the request body mentions the {kind}",
                    seen.method, seen.path
                ));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn local_names() -> Vec<(&'static str, String)> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| v.len() >= 2);
    let mut names = Vec::new();
    if let Some(user) = var("USERNAME").or_else(|| var("USER")) {
        names.push(("user name", user));
    }
    if let Some(host) = var("COMPUTERNAME").or_else(|| var("HOSTNAME")) {
        names.push(("computer name", host));
    }
    if let Some(home) = var("USERPROFILE").or_else(|| var("HOME")) {
        names.push(("home folder path", home.replace('\\', "/")));
        names.push(("home folder path", home.replace('/', "\\")));
    }
    names
}

/// `needle` with no letter or digit right before or after it, so a user called "PC" does not
/// match "RPC".
fn contains_word(hay: &str, needle: &str) -> bool {
    let hay = hay.replace("\\\\", "\\");
    hay.match_indices(needle).any(|(at, _)| {
        let before = hay[..at].chars().next_back();
        let after = hay[at + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn compare(provider: Provider, report: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/omp")
        .join(format!("headers-{}.txt", provider.name()));
    let want = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if want == report {
        return;
    }
    if std::env::var_os("CH_BLESS").is_some() {
        std::fs::write(&path, report).expect("write the fixture");
        println!("[{}] rewrote {}", provider.name(), path.display());
        return;
    }
    let gone: Vec<&str> = want
        .lines()
        .filter(|l| !report.lines().any(|r| r == *l))
        .collect();
    let new: Vec<&str> = report
        .lines()
        .filter(|l| !want.lines().any(|w| w == *l))
        .collect();
    panic!(
        "omp's {} requests changed.\nOnly in {}:\n{}\nOnly in this run:\n{}\n\nThis run:\n{report}\n\
         Review the change and PRIVACY.md, then run again with CH_BLESS=1 to accept it.",
        provider.name(),
        path.display(),
        gone.join("\n"),
        new.join("\n")
    );
}
