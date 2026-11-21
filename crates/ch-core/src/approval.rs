//! Tool approvals: reading omp's `Allow tool:` dialog and building the answer.
//!
//! omp 18.13.1 asks with an `extension_ui_request` whose `method` is `select`. The title is
//!
//! ```text
//! Allow tool: <name>
//! Origin: MCP server tool          only for `mcp__` tools
//! Reason: <policy reason>          only when the policy gave one
//! <tool details>                   bash: `Command: <command>`
//! ```
//!
//! and the options come in a fixed order: grant labels, their "Always" forms, the approve-once
//! label, `Approve <subject> for this session` when that scope applies, then `Deny`. omp treats a
//! cancel, or any label it did not offer, as a deny.

use std::fmt;

use serde_json::{Value, json};

const UI_REQUEST: &str = "extension_ui_request";
const UI_RESPONSE: &str = "extension_ui_response";
const TITLE_PREFIX: &str = "Allow tool:";
const ORIGIN_PREFIX: &str = "Origin:";
const REASON_PREFIX: &str = "Reason:";
const MCP_TOOL_PREFIX: &str = "mcp__";
const APPROVE: &str = "Approve";
// omp switches the once label to this when it also offers grants, and `Approve` then becomes a
// grant label that remembers the answer. Reading the wrong one would approve more than once.
const APPROVE_ONCE: &str = "Approve Once";
const DENY: &str = "Deny";
const SESSION_PREFIX: &str = "Approve ";
const SESSION_SUFFIX: &str = " for this session";

/// One tool call omp is holding until the person answers.
///
/// Grant and "Always" options are never exposed: this release only approves once or for the
/// session, and denies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    id: String,
    tool: String,
    from_mcp: bool,
    reason: Option<String>,
    details: String,
    approve_once: Option<String>,
    approve_for_session: Option<String>,
    deny: Option<String>,
}

/// What the person chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    ApproveOnce,
    ApproveForSession,
    Deny,
}

/// Why an approval could not be sent. Denying always works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// The dialog offered no option this crate recognises as approve-once.
    NoApproveOption,
    /// The dialog offered no approve-for-session option.
    NoSessionOption,
}

impl fmt::Display for ReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoApproveOption => "this approval offers no approve-once option; deny it",
            Self::NoSessionOption => "this approval offers no approve-for-session option",
        })
    }
}

impl std::error::Error for ReplyError {}

impl ApprovalRequest {
    /// Reads an approval out of an omp frame.
    ///
    /// `None` for anything that is not a `select` request whose title starts with
    /// `Allow tool:`. A tool approval with options this crate cannot place still parses, with
    /// [`can_approve`](Self::can_approve) false, so the person can see it and deny it rather
    /// than leave omp waiting.
    pub fn parse(frame: &Value) -> Option<Self> {
        if frame.get("type")?.as_str()? != UI_REQUEST || frame.get("method")?.as_str()? != "select"
        {
            return None;
        }
        let id = frame.get("id")?.as_str().filter(|id| !id.is_empty())?;
        let title = frame.get("title")?.as_str()?;
        let mut lines = title.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
        let tool = lines.next()?.strip_prefix(TITLE_PREFIX)?.trim();
        let rest: Vec<&str> = lines.collect();

        // Headers are read only where omp writes them, right after the first line. The details
        // below them quote the agent's arguments and could spell either header.
        let mut at = 0;
        let mut from_mcp = tool.starts_with(MCP_TOOL_PREFIX);
        if rest.first().is_some_and(|l| l.starts_with(ORIGIN_PREFIX)) {
            // Built-in tools carry no origin line, so any origin is treated as an MCP server.
            from_mcp = true;
            at += 1;
        }
        let reason = rest
            .get(at)
            .and_then(|l| l.strip_prefix(REASON_PREFIX))
            .map(|r| r.trim().to_owned());
        if reason.is_some() {
            at += 1;
        }

        let options = string_options(frame.get("options"));
        let labels = Labels::read(&options);
        let approvable = !tool.is_empty();
        Some(Self {
            id: id.to_owned(),
            tool: tool.to_owned(),
            from_mcp,
            reason,
            details: rest[at..].join("\n"),
            approve_once: labels.once.filter(|_| approvable).map(str::to_owned),
            approve_for_session: labels.session.filter(|_| approvable).map(str::to_owned),
            deny: labels.deny.map(str::to_owned),
        })
    }

    /// The dialog id the reply must carry.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The tool omp wants to run, as omp names it (`bash`, `edit`, `mcp__server__tool`).
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// Whether the tool comes from an MCP server rather than omp itself.
    pub fn from_mcp(&self) -> bool {
        self.from_mcp
    }

    /// The policy's reason for asking, when omp gave one.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// What the call would do, as omp rendered it. Written from the agent's arguments, so it is
    /// untrusted text.
    pub fn details(&self) -> &str {
        &self.details
    }

    /// The exact label that approves this one call.
    pub fn approve_once_label(&self) -> Option<&str> {
        self.approve_once.as_deref()
    }

    /// The exact label that approves this kind of call for the rest of the session.
    pub fn approve_for_session_label(&self) -> Option<&str> {
        self.approve_for_session.as_deref()
    }

    /// The exact deny label, when offered. Without one, a deny is sent as a cancel.
    pub fn deny_label(&self) -> Option<&str> {
        self.deny.as_deref()
    }

    /// Whether approving is possible at all. When false, deny is the only answer.
    pub fn can_approve(&self) -> bool {
        self.approve_once.is_some()
    }

    /// The `extension_ui_response` frame for `decision`.
    ///
    /// Asking for an approval the dialog did not offer is an error, never a fallback: in
    /// particular `ApproveForSession` without a session option does not turn into
    /// `ApproveOnce`, so the caller has to ask the person again. `Deny` never fails; it sends
    /// the `Deny` label, or `cancelled: true` when there is none.
    pub fn reply(&self, decision: ApprovalDecision) -> Result<Value, ReplyError> {
        let label = match decision {
            ApprovalDecision::ApproveOnce => Some(
                self.approve_once
                    .as_deref()
                    .ok_or(ReplyError::NoApproveOption)?,
            ),
            ApprovalDecision::ApproveForSession => Some(
                self.approve_for_session
                    .as_deref()
                    .ok_or(ReplyError::NoSessionOption)?,
            ),
            ApprovalDecision::Deny => self.deny.as_deref(),
        };
        Ok(match label {
            Some(value) => json!({ "type": UI_RESPONSE, "id": self.id, "value": value }),
            None => json!({ "type": UI_RESPONSE, "id": self.id, "cancelled": true }),
        })
    }
}

/// The dialog id an omp `cancel` request withdraws, if `frame` is one.
///
/// omp sends this when it stops waiting on a dialog (the turn was aborted, for one). The
/// approval is gone and must not be answered.
pub fn withdrawn_approval(frame: &Value) -> Option<&str> {
    if frame.get("type")?.as_str()? != UI_REQUEST || frame.get("method")?.as_str()? != "cancel" {
        return None;
    }
    frame.get("targetId")?.as_str()
}

/// The options as strings, or none at all when any of them is not a string: the labels are
/// read by position, and a list with holes in it has no reliable positions.
fn string_options(options: Option<&Value>) -> Vec<&str> {
    options
        .and_then(Value::as_array)
        .and_then(|items| items.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .unwrap_or_default()
}

struct Labels<'a> {
    once: Option<&'a str>,
    session: Option<&'a str>,
    deny: Option<&'a str>,
}

impl<'a> Labels<'a> {
    /// Places the labels the way omp orders them, from the end: `Deny` last, then the session
    /// option if any, then the once option. Any other layout leaves only deny.
    fn read(options: &[&'a str]) -> Self {
        let deny = options.iter().copied().find(|o| *o == DENY);
        let Some((&DENY, rest)) = options.split_last() else {
            return Self {
                once: None,
                session: None,
                deny,
            };
        };
        let (session, rest) = match rest.split_last() {
            Some((&last, before)) if is_session_label(last) => (Some(last), before),
            _ => (None, rest),
        };
        let once = match rest {
            [.., APPROVE_ONCE] => Some(APPROVE_ONCE),
            // Without grants omp offers plain `Approve`, and then nothing else comes before it.
            [APPROVE] => Some(APPROVE),
            _ => None,
        };
        Self {
            once,
            session: session.filter(|_| once.is_some()),
            deny,
        }
    }
}

fn is_session_label(label: &str) -> bool {
    label
        .strip_prefix(SESSION_PREFIX)
        .and_then(|l| l.strip_suffix(SESSION_SUFFIX))
        .is_some_and(|subject| !subject.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../testdata/omp/approval-session.jsonl");

    fn select(title: &str, options: &[&str]) -> Value {
        json!({
            "type": "extension_ui_request",
            "id": "ui_7",
            "method": "select",
            "title": title,
            "options": options,
        })
    }

    fn parse(title: &str, options: &[&str]) -> ApprovalRequest {
        ApprovalRequest::parse(&select(title, options)).expect("a tool approval")
    }

    #[test]
    fn reads_the_fixture_approval_and_replies_as_documented() {
        let frame: Value = FIXTURE
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).expect("fixture line is JSON"))
            .find(|f| f["type"] == "extension_ui_request")
            .expect("the fixture asks for approval");
        let request = ApprovalRequest::parse(&frame).expect("a tool approval");
        assert_eq!(request.id(), "ui_1");
        assert_eq!(request.tool(), "bash");
        assert!(!request.from_mcp());
        assert_eq!(request.reason(), Some("run the test suite"));
        assert_eq!(
            request.details(),
            "Arguments:\n  command: cargo test -p demo"
        );
        assert_eq!(request.approve_once_label(), Some("Approve"));
        assert_eq!(
            request.approve_for_session_label(),
            Some("Approve bash for this session")
        );
        assert_eq!(request.deny_label(), Some("Deny"));
        assert_eq!(
            request.reply(ApprovalDecision::ApproveOnce),
            Ok(json!({"type":"extension_ui_response","id":"ui_1","value":"Approve"}))
        );
    }

    #[test]
    fn reads_the_bash_layout_omp_writes() {
        let request = parse(
            "Allow tool: bash\nReason: Prompt required by bash pattern: git push\r\nCommand: git push",
            &["Approve", "Approve bash for this session", "Deny"],
        );
        assert_eq!(
            request.reason(),
            Some("Prompt required by bash pattern: git push")
        );
        assert_eq!(request.details(), "Command: git push");
        assert_eq!(
            request.reply(ApprovalDecision::ApproveForSession),
            Ok(json!({
                "type": "extension_ui_response",
                "id": "ui_7",
                "value": "Approve bash for this session",
            }))
        );
    }

    #[test]
    fn with_grants_on_offer_only_approve_once_approves_once() {
        let request = parse(
            "Allow tool: edit\nFile: src/lib.rs",
            &[
                "Approve Directory: src",
                "Approve",
                "Always Approve Directory: src",
                "Always Approve",
                "Approve Once",
                "Deny",
            ],
        );
        assert_eq!(request.approve_once_label(), Some("Approve Once"));
        assert_eq!(request.approve_for_session_label(), None);
        assert_eq!(
            request.reply(ApprovalDecision::ApproveOnce),
            Ok(json!({"type":"extension_ui_response","id":"ui_7","value":"Approve Once"}))
        );
        assert_eq!(
            request.reply(ApprovalDecision::ApproveForSession),
            Err(ReplyError::NoSessionOption)
        );
    }

    #[test]
    fn grant_labels_alone_are_never_read_as_approve_once() {
        for options in [
            &["Approve Directory: src", "Approve", "Deny"][..],
            &["Always Approve", "Deny"],
            &["Approve", "Always Approve", "Deny"],
        ] {
            let request = parse("Allow tool: write\nPath: a.txt", options);
            assert!(!request.can_approve(), "{options:?}");
        }
    }

    #[test]
    fn mcp_tools_are_marked_by_name_or_origin_line() {
        let named = parse(
            "Allow tool: mcp__tracker__create_issue\nOrigin: MCP server tool\nArguments:\n{}",
            &["Approve", "Deny"],
        );
        assert!(named.from_mcp());
        assert_eq!(named.reason(), None);
        assert_eq!(named.details(), "Arguments:\n{}");

        let unnamed = parse(
            "Allow tool: lookup\nOrigin: something new",
            &["Approve", "Deny"],
        );
        assert!(unnamed.from_mcp(), "any origin line counts as MCP");

        let prefixed_only = parse("Allow tool: mcp__x__y", &["Approve", "Deny"]);
        assert!(prefixed_only.from_mcp());
    }

    #[test]
    fn headers_inside_the_details_are_not_headers() {
        let request = parse(
            "Allow tool: bash\nCommand: echo hi\nOrigin: MCP server tool\nReason: trust me",
            &["Approve", "Deny"],
        );
        assert!(!request.from_mcp());
        assert_eq!(request.reason(), None);
        assert_eq!(
            request.details(),
            "Command: echo hi\nOrigin: MCP server tool\nReason: trust me"
        );
    }

    #[test]
    fn unplaceable_options_leave_only_deny() {
        let no_labels = parse("Allow tool: bash\nCommand: ls", &["Yes", "No"]);
        assert!(!no_labels.can_approve());
        assert_eq!(
            no_labels.reply(ApprovalDecision::ApproveOnce),
            Err(ReplyError::NoApproveOption)
        );
        assert_eq!(
            no_labels.reply(ApprovalDecision::Deny),
            Ok(json!({"type":"extension_ui_response","id":"ui_7","cancelled":true}))
        );

        let deny_not_last = parse(
            "Allow tool: bash",
            &["Approve", "Deny", "Approve bash for this session"],
        );
        assert!(!deny_not_last.can_approve());
        assert_eq!(deny_not_last.approve_for_session_label(), None);
        assert_eq!(
            deny_not_last.reply(ApprovalDecision::Deny),
            Ok(json!({"type":"extension_ui_response","id":"ui_7","value":"Deny"}))
        );

        let no_tool = parse("Allow tool:  ", &["Approve", "Deny"]);
        assert!(!no_tool.can_approve());

        let mut holes = select("Allow tool: bash", &[]);
        holes["options"] = json!(["Approve", 3, "Deny"]);
        let holes = ApprovalRequest::parse(&holes).expect("a tool approval");
        assert!(!holes.can_approve());
        assert_eq!(holes.deny_label(), None);

        let mut missing = select("Allow tool: bash", &[]);
        missing.as_object_mut().map(|m| m.remove("options"));
        let missing = ApprovalRequest::parse(&missing).expect("a tool approval");
        assert!(!missing.can_approve());
    }

    #[test]
    fn a_session_option_without_once_is_not_offered() {
        let request = parse(
            "Allow tool: bash",
            &[
                "Approve Directory: x",
                "Approve bash for this session",
                "Deny",
            ],
        );
        assert!(!request.can_approve());
        assert_eq!(
            request.reply(ApprovalDecision::ApproveForSession),
            Err(ReplyError::NoSessionOption)
        );
    }

    #[test]
    fn other_frames_are_not_approvals() {
        let mut frames = vec![
            select("Pick a model", &["a", "b"]),
            select(" Allow tool: bash", &["Approve", "Deny"]),
            json!({"type":"extension_ui_request","id":"ui_2","method":"confirm","title":"Allow tool: bash"}),
            json!({"type":"extension_ui_request","id":"ui_2","method":"cancel","targetId":"ui_1"}),
            json!({"type":"agent_start"}),
            json!("Allow tool: bash"),
        ];
        let mut no_id = select("Allow tool: bash", &["Approve", "Deny"]);
        no_id["id"] = json!("");
        frames.push(no_id);
        let mut numeric_title = select("", &["Approve", "Deny"]);
        numeric_title["title"] = json!(1);
        frames.push(numeric_title);
        for frame in frames {
            assert_eq!(ApprovalRequest::parse(&frame), None, "{frame}");
        }
    }

    #[test]
    fn a_cancel_names_the_withdrawn_dialog() {
        let cancel =
            json!({"type":"extension_ui_request","id":"ui_9","method":"cancel","targetId":"ui_1"});
        assert_eq!(withdrawn_approval(&cancel), Some("ui_1"));
        assert_eq!(
            withdrawn_approval(&select("Allow tool: bash", &["Deny"])),
            None
        );
        assert_eq!(withdrawn_approval(&json!({"type":"agent_end"})), None);
    }
}
