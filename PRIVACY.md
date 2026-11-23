# What leaves your machine

C-Harness runs a coding agent on your computer or on a host you pick. This page lists
everything that leaves your machine because of it: where it goes, what is sent, when, why, and
whether you can turn it off. The same list is in the app under Settings, What leaves this
machine.

<!-- outbound:start -->
C-Harness itself sends no telemetry, crash reports or update checks, and it has no HTTP client of its own. What does leave your machine goes through omp (the coding agent C-Harness runs), git, curl or ssh, and it is all listed here.

### Model requests

- **Goes to:** Your model provider
- **What:** Your prompts, the files and command output the agent reads, the session so far and your model key. Requests also carry a random id that ties one session's requests together.
- **When:** Every turn of every session.
- **Why:** The model does the work. omp sends the requests.
- **Turning it off:** No switch. A session needs a model. You pick the provider in Settings, Model.
- **How we know:** Seen in a test run: the omp header audit in crates/ch-runner/tests/omp_headers.rs, and crates/ch-app/src/launch.rs

### Skill descriptions

- **Goes to:** Your model provider
- **What:** The name and description of every skill omp finds in a .claude/skills folder in your project folder or any folder above it, such as your home folder.
- **When:** When omp starts, one request per skill.
- **Why:** omp asks the model to shorten skill descriptions before it uses them.
- **Turning it off:** No switch. C-Harness has no setting for it. In a test run, a project outside the home folder sent none.
- **How we know:** Seen in a test run: omp's debug log from a test run

### Model catalogs

- **Goes to:** Model catalog hosts
- **What:** Connections to catalog.stencil.so (omp's model list), api.anthropic.com, api.commandcode.ai, api.kilo.ai, api.venice.ai, coding-intl.dashscope.aliyuncs.com, hyper.charm.land and zenmux.ai. What they receive was not checked: the test refused the connections.
- **When:** When omp starts.
- **Why:** omp refreshes the list of models it knows.
- **Turning it off:** No switch. omp has no setting for it that we found.
- **How we know:** Seen in a test run: a proxy log from a test run

### MCP servers

- **Goes to:** MCP servers you set up
- **What:** Whatever each server sends. In a test run omp also contacted registry.npmjs.org while starting one.
- **When:** When omp starts, if your Claude Code or omp settings list MCP servers.
- **Why:** omp loads MCP servers from your settings and the project's.
- **Turning it off:** You can turn it off. Remove the servers from those settings. C-Harness has no switch of its own.
- **How we know:** Seen in a test run: omp's output and a proxy log from a test run

### Web search

- **Goes to:** Web search providers
- **What:** Search queries the agent writes.
- **When:** When the agent uses omp's web search tool.
- **Why:** The agent looks things up.
- **Turning it off:** You can turn it off. omp setting web_search.enabled, on by default. C-Harness leaves it alone.
- **How we know:** Not verified: omp config list

### omp Auto QA

- **Goes to:** omp's issue reports at qa.omp.sh
- **What:** Reports about tool failures omp runs into.
- **When:** When omp's Auto QA sees a tool problem. omp says it asks for consent before the first report.
- **Why:** omp's authors collect tool bugs.
- **Turning it off:** You can turn it off. omp setting dev.autoqa, on by default. C-Harness does not turn it off yet.
- **How we know:** Not verified: omp config list

### OpenTelemetry export

- **Goes to:** An OpenTelemetry collector
- **What:** omp's traces and metrics.
- **When:** Only if OTEL_* exporter variables are set where omp runs.
- **Why:** omp can export to a collector you run yourself.
- **Turning it off:** You can turn it off. Unset those variables, or set omp's telemetry.otlpExportEnabled to false.
- **How we know:** Not verified: omp config list

### Commands the agent runs

- **Goes to:** Wherever the command connects
- **What:** Whatever that command sends, for example curl, git push or a package install.
- **When:** When you approve the command, or without asking if your approval mode allows it.
- **Why:** The agent works by running commands in your project.
- **Turning it off:** Only when you choose. Deny the command. C-Harness marks network commands such as curl, wget, ssh and git push as high risk.
- **How we know:** Verified in code: crates/ch-core/src/risk.rs

### Your Verify command

- **Goes to:** Wherever your Verify command connects
- **What:** Whatever that command sends.
- **When:** When you press Verify.
- **Why:** You set the command in Settings, Checks.
- **Turning it off:** Only when you choose. Leave Verify command empty.
- **How we know:** Verified in code: crates/ch-runner/src/verify.rs

### Model test in first run

- **Goes to:** Your model provider
- **What:** One short prompt, "Reply with the single word ok.", with your model key. omp runs it with tools off, no session saved and its spending cap at $0.05. omp starts for it, so the rows above that happen when omp starts apply too. It runs in the system temp folder, which on Windows is inside your home folder, so skill descriptions found there can go with it.
- **When:** Only when you press Test the model in first run.
- **Why:** It checks that the key and the model answer before your first session.
- **Turning it off:** Only when you choose. Don't press it. The other first-run checks only ask omp for its version.
- **How we know:** Verified in code: crates/ch-runner/src/omp.rs (test_model), crates/ch-runner/src/preflight.rs and crates/ch-app/src/first_run.rs

### Sessions on an SSH host

- **Goes to:** Your SSH host
- **What:** The whole session: prompts, approvals, agent output, your model key when the session starts or you test the model, and your GitHub token when you land.
- **When:** Only when Settings, Where sessions run is set to SSH host.
- **Why:** The agent runs on that machine.
- **Turning it off:** Only when you choose. Choose This computer.
- **How we know:** Verified in code: crates/ch-runtime/src/ssh.rs and crates/ch-protocol/src/runner.rs

### Landing on GitHub

- **Goes to:** GitHub
- **What:** The session branch and its commits with your author name and email, your GitHub token, and a pull request. Its title comes from your first prompt; its body names the session and the checks that ran.
- **When:** Only when you press Land.
- **Why:** Land pushes the branch to github.com and opens the pull request through api.github.com.
- **Turning it off:** Only when you choose. Don't press Land. Without a GitHub token, Land stays off.
- **How we know:** Verified in code: crates/ch-runner/src/land.rs, crates/ch-runner/src/pr.rs and crates/ch-app/src/view_model.rs

### omp update check

- **Goes to:** omp's update server
- **What:** Nothing.
- **When:** Never. C-Harness turns omp's startup update check off for every session.
- **Why:** Updating omp stays your decision.
- **Turning it off:** Turned off. Nothing to do.
- **How we know:** Verified in code: crates/ch-protocol/src/probe.rs

Rows seen in a test run were checked against omp 18.13.1.
<!-- outbound:end -->

## How this list is kept

The list lives in `crates/ch-core/src/outbound.rs`. The app page and this file both come from
it, and a test fails when they differ. After changing the list, refresh this file with:

    CH_BLESS=1 cargo test -p ch-core --test privacy

Each row says how we know. "Verified in code" means we read it in C-Harness's own code. "Seen
in a test run" means we ran omp against a local listener with a fake key and watched what it
sent; that check is repeated before each release (see [CONTRIBUTING.md](CONTRIBUTING.md)).
"Not verified" means the row comes from omp's settings and nobody has watched it on the wire.

## Something missing?

If you see C-Harness or omp send something that is not on this list, please open an issue.
For a security problem, follow [SECURITY.md](SECURITY.md) instead.
