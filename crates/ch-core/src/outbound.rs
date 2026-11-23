//! What leaves this machine, where it goes and when. One list feeds the Settings page and
//! `PRIVACY.md`, so the two cannot drift apart.

use std::fmt::Write as _;

/// The omp version the observed rows were seen with.
pub const OMP_VERSION_SEEN: &str = "18.13.1";

/// Opens the page and the markdown copy.
pub const OUTBOUND_SUMMARY: &str = "C-Harness itself sends no telemetry, crash reports or update \
checks, and it has no HTTP client of its own. What does leave your machine goes through omp (the \
coding agent C-Harness runs), git, curl or ssh, and it is all listed here.";

/// How sure the list is about one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// Read in C-Harness code; `source` names the files.
    InCode,
    /// Seen in a test run of omp [`OMP_VERSION_SEEN`] with a fake key and a local listener.
    Observed,
    /// Taken from omp's own settings, never seen on the wire.
    NotVerified,
}

impl Evidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::InCode => "Verified in code",
            Self::Observed => "Seen in a test run",
            Self::NotVerified => "Not verified",
        }
    }
}

/// Who decides whether a row happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Happens only after you do something.
    YourChoice,
    /// On until you change a setting.
    Setting,
    /// Happens whenever a session runs.
    NoSwitch,
    /// C-Harness turns it off.
    Off,
}

impl Control {
    /// Group order on the page. [`OUTBOUND`] keeps the same order, so `PRIVACY.md` does too.
    pub const ORDER: [Control; 4] = [Self::NoSwitch, Self::Setting, Self::YourChoice, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::YourChoice => "Only when you choose",
            Self::Setting => "You can turn it off",
            Self::NoSwitch => "No switch",
            Self::Off => "Turned off",
        }
    }
}

/// One thing that leaves this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outbound {
    pub id: &'static str,
    pub title: &'static str,
    /// Who receives it, by role.
    pub destination: &'static str,
    pub what: &'static str,
    pub when: &'static str,
    pub why: &'static str,
    /// Read after [`Control::label`].
    pub turn_off: &'static str,
    pub control: Control,
    pub evidence: Evidence,
    /// Where the claim was checked.
    pub source: &'static str,
}

pub const OUTBOUND: &[Outbound] = &[
    Outbound {
        id: "model-provider",
        title: "Model requests",
        destination: "Your model provider",
        what: "Your prompts, the files and command output the agent reads, the session so far and \
               your model key. Requests also carry a random id that ties one session's requests \
               together.",
        when: "Every turn of every session.",
        why: "The model does the work. omp sends the requests.",
        turn_off: "A session needs a model. You pick the provider in Settings, Model.",
        control: Control::NoSwitch,
        evidence: Evidence::Observed,
        source: "the omp header audit in crates/ch-runner/tests/omp_headers.rs, and \
                 crates/ch-app/src/launch.rs",
    },
    Outbound {
        id: "skill-descriptions",
        title: "Skill descriptions",
        destination: "Your model provider",
        what: "The name and description of every skill omp finds in a .claude/skills folder in \
               your project folder or any folder above it, such as your home folder.",
        when: "When omp starts, one request per skill.",
        why: "omp asks the model to shorten skill descriptions before it uses them.",
        turn_off: "C-Harness has no setting for it. In a test run, a project outside the home \
                   folder sent none.",
        control: Control::NoSwitch,
        evidence: Evidence::Observed,
        source: "omp's debug log from a test run",
    },
    Outbound {
        id: "model-catalogs",
        title: "Model catalogs",
        destination: "Model catalog hosts",
        what: "Connections to catalog.stencil.so (omp's model list), api.anthropic.com, \
               api.commandcode.ai, api.kilo.ai, api.venice.ai, \
               coding-intl.dashscope.aliyuncs.com, hyper.charm.land and zenmux.ai. What they \
               receive was not checked: the test refused the connections.",
        when: "When omp starts.",
        why: "omp refreshes the list of models it knows.",
        turn_off: "omp has no setting for it that we found.",
        control: Control::NoSwitch,
        evidence: Evidence::Observed,
        source: "a proxy log from a test run",
    },
    Outbound {
        id: "mcp-servers",
        title: "MCP servers",
        destination: "MCP servers you set up",
        what: "Whatever each server sends. In a test run omp also contacted registry.npmjs.org \
               while starting one.",
        when: "When omp starts, if your Claude Code or omp settings list MCP servers.",
        why: "omp loads MCP servers from your settings and the project's.",
        turn_off: "Remove the servers from those settings. C-Harness has no switch of its own.",
        control: Control::Setting,
        evidence: Evidence::Observed,
        source: "omp's output and a proxy log from a test run",
    },
    Outbound {
        id: "web-search",
        title: "Web search",
        destination: "Web search providers",
        what: "Search queries the agent writes.",
        when: "When the agent uses omp's web search tool.",
        why: "The agent looks things up.",
        turn_off: "omp setting web_search.enabled, on by default. C-Harness leaves it alone.",
        control: Control::Setting,
        evidence: Evidence::NotVerified,
        source: "omp config list",
    },
    Outbound {
        id: "omp-auto-qa",
        title: "omp Auto QA",
        destination: "omp's issue reports at qa.omp.sh",
        what: "Reports about tool failures omp runs into.",
        when: "When omp's Auto QA sees a tool problem. omp says it asks for consent before the \
               first report.",
        why: "omp's authors collect tool bugs.",
        turn_off: "omp setting dev.autoqa, on by default. C-Harness does not turn it off yet.",
        control: Control::Setting,
        evidence: Evidence::NotVerified,
        source: "omp config list",
    },
    Outbound {
        id: "omp-otlp",
        title: "OpenTelemetry export",
        destination: "An OpenTelemetry collector",
        what: "omp's traces and metrics.",
        when: "Only if OTEL_* exporter variables are set where omp runs.",
        why: "omp can export to a collector you run yourself.",
        turn_off: "Unset those variables, or set omp's telemetry.otlpExportEnabled to false.",
        control: Control::Setting,
        evidence: Evidence::NotVerified,
        source: "omp config list",
    },
    Outbound {
        id: "agent-commands",
        title: "Commands the agent runs",
        destination: "Wherever the command connects",
        what: "Whatever that command sends, for example curl, git push or a package install.",
        when: "When you approve the command, or without asking if your approval mode allows it.",
        why: "The agent works by running commands in your project.",
        turn_off: "Deny the command. C-Harness marks ssh, scp, sftp and git push as high risk, \
                   and curl or wget when the same command also runs a script. A plain curl is \
                   medium risk.",
        control: Control::YourChoice,
        evidence: Evidence::InCode,
        source: "crates/ch-core/src/risk.rs",
    },
    Outbound {
        id: "verify-command",
        title: "Your Verify command",
        destination: "Wherever your Verify command connects",
        what: "Whatever that command sends.",
        when: "When you press Verify.",
        why: "You set the command in Settings, Checks.",
        turn_off: "Leave Verify command empty.",
        control: Control::YourChoice,
        evidence: Evidence::InCode,
        source: "crates/ch-runner/src/verify.rs",
    },
    Outbound {
        id: "model-test",
        title: "Model test in first run",
        destination: "Your model provider",
        what: "One short prompt, \"Reply with the single word ok.\", with your model key. omp \
               runs it with tools off, no session saved and its spending cap at $0.05. omp \
               starts for it, so the rows above that happen when omp starts apply too. It runs \
               in the system temp folder, which on Windows is inside your home folder, so skill \
               descriptions found there can go with it.",
        when: "Only when you press Test the model in first run.",
        why: "It checks that the key and the model answer before your first session.",
        turn_off: "Don't press it. The other first-run checks only ask omp for its version.",
        control: Control::YourChoice,
        evidence: Evidence::InCode,
        source: "crates/ch-runner/src/omp.rs (test_model), crates/ch-runner/src/preflight.rs and \
                 crates/ch-app/src/first_run.rs",
    },
    Outbound {
        id: "ssh-host",
        title: "Sessions on an SSH host",
        destination: "Your SSH host",
        what: "The whole session: prompts, approvals, agent output, your model key when the \
               session starts or you test the model, and your GitHub token when you land.",
        when: "Only when Settings, Where sessions run is set to SSH host.",
        why: "The agent runs on that machine.",
        turn_off: "Choose This computer.",
        control: Control::YourChoice,
        evidence: Evidence::InCode,
        source: "crates/ch-runtime/src/ssh.rs and crates/ch-protocol/src/runner.rs",
    },
    Outbound {
        id: "github",
        title: "Landing on GitHub",
        destination: "GitHub",
        what: "The session branch and its commits with your author name and email, your GitHub \
               token, and a pull request. Its title comes from your first prompt; its body names \
               the session and the checks that ran.",
        when: "Only when you press Land.",
        why: "Land pushes the branch to github.com and opens the pull request through \
              api.github.com.",
        turn_off: "Don't press Land. Without a GitHub token, Land stays off.",
        control: Control::YourChoice,
        evidence: Evidence::InCode,
        source: "crates/ch-runner/src/land.rs, crates/ch-runner/src/pr.rs and \
                 crates/ch-app/src/view_model.rs",
    },
    Outbound {
        id: "omp-update-check",
        title: "omp update check",
        destination: "omp's update server",
        what: "Nothing.",
        when: "Never. C-Harness turns omp's startup update check off for every session.",
        why: "Updating omp stays your decision.",
        turn_off: "Nothing to do.",
        control: Control::Off,
        evidence: Evidence::InCode,
        source: "crates/ch-protocol/src/probe.rs",
    },
];

/// The markdown copy kept in `PRIVACY.md`.
pub fn outbound_markdown() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{OUTBOUND_SUMMARY}");
    for row in OUTBOUND {
        let _ = write!(
            out,
            "\n### {}\n\n- **Goes to:** {}\n- **What:** {}\n- **When:** {}\n- **Why:** {}\n- \
             **Turning it off:** {}. {}\n- **How we know:** {}: {}\n",
            row.title,
            row.destination,
            row.what,
            row.when,
            row.why,
            row.control.label(),
            row.turn_off,
            row.evidence.label(),
            row.source,
        );
    }
    let _ = write!(
        out,
        "\nRows seen in a test run were checked against omp {OMP_VERSION_SEEN}.\n"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_row_is_filled_in_and_named_once() {
        let mut ids = HashSet::new();
        let mut titles = HashSet::new();
        for row in OUTBOUND {
            assert!(ids.insert(row.id), "{} twice", row.id);
            assert!(titles.insert(row.title), "{} twice", row.title);
            let texts = [
                row.title,
                row.destination,
                row.what,
                row.when,
                row.why,
                row.turn_off,
                row.source,
            ];
            for text in texts {
                assert!(!text.trim().is_empty(), "{} has an empty field", row.id);
                assert!(!text.contains("  "), "{} has a doubled space", row.id);
            }
        }
        assert!(!ids.is_empty(), "the page would be empty");
    }

    #[test]
    fn rows_are_grouped_in_page_order() {
        let rank = |row: &Outbound| {
            Control::ORDER
                .iter()
                .position(|c| *c == row.control)
                .unwrap_or_else(|| panic!("{} has a control the page does not show", row.id))
        };
        for pair in OUTBOUND.windows(2) {
            assert!(
                rank(&pair[0]) <= rank(&pair[1]),
                "{} should come before {}",
                pair[1].id,
                pair[0].id
            );
        }
    }

    #[test]
    fn markdown_has_a_heading_per_row() {
        let markdown = outbound_markdown();
        for row in OUTBOUND {
            assert!(
                markdown.contains(&format!("### {}\n", row.title)),
                "{}",
                row.id
            );
            assert!(markdown.contains(row.what), "{}", row.id);
        }
        assert!(markdown.starts_with(OUTBOUND_SUMMARY));
    }

    #[test]
    fn the_update_check_stays_off() {
        let row = OUTBOUND.iter().find(|r| r.id == "omp-update-check");
        assert_eq!(row.map(|r| r.control), Some(Control::Off));
        assert!(ch_protocol::host_config_overlay(&[]).contains("checkUpdate: false"));
    }
}
