<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# AI Agent Guidelines for Niobe

When working on this project, adopt the persona of a **Principal Rust Engineer and Code Quality
Architect**. You are not writing a prototype; you are building a production-grade terminal tool
that people will trust with their subscriptions and their money.

This file is your core instruction manual. Refer to it and the principles within to be effective.

---

## 0. The project

Niobe is a terminal coding agent that keeps the engineer aware of what is being built and how:
what the agent did, what it decided, what it touched, what it is doing right now and what it
cost, visible while it happens. The engineer stays on the loop instead of in it — informed without
having to approve every step. The bill is one of the things on screen, every figure in it measured
or labelled, and the harness spends less by default.

**Bridge-first.** Niobe drives the official `claude` and `codex` CLIs as subprocesses, so it uses
whatever access those CLIs are already signed in with. A native agent loop on provider API keys
(`Backend::Native`) is the path to what the bridges cannot expose.

### 0.1 Maintainer-local instructions

**If `AGENTS.local.md` exists at the repository root, read it before starting any work and follow
it.** It carries workflow that is private to the maintainer (where work is planned and tracked)
and is deliberately not part of the public repository. It never overrides §4.

---

## 1. Codebase map

A Cargo workspace. The crate graph is the architecture: who may depend on whom is enforced in CI.

- **`crates/niobe-core/`** — the shared vocabulary. `event.rs` is the event model, the one type
  every backend produces into; `session.rs` is `SessionState`, the fold every consumer derives its
  numbers from, for each turn that has ended as well as for the whole session; `permission.rs` is the standing answer to a permission prompt (`Rule`,
  `Allowlist`), which the shell matches and the config stores; `diff.rs` is the line arithmetic
  every backend counts a file change with, so two bridges cannot disagree about what a changed
  line is, and the `Hunk` a change's lines travel in where the backend reported them; `test_run.rs` recognises a shell
  command that runs `cargo test` and reads the counts out of its output, only where the output
  holds the whole run, so every backend reads a test run the same way. Depends on no other workspace crate and on no wire format.
- **`crates/niobe-ledger/`** — token and cost accounting, and the provenance label every figure
  carries (`Measured`, `ApiEquivalent`, `Unpriced`). `prices.toml` is the bundled price table:
  per-model rates, each dated from the day it took effect, with the published source of every
  number in its comments; `parse.rs` walks it by hand like the config, `table.rs` looks a model
  up by exact id on a day and prices a usage record against it. The same file dates each model's
  context window in its `[[context]]` entries, which the shell's context meter falls back on where
  the backend has not reported one. `tests/prices.rs` checks the table against costs computed by
  hand, and `tests/contexts.rs` its windows against the published sizes.
- **`crates/niobe-config/`** — the config: profiles, each a backend plus the environment,
  arguments and credential refresh it runs with and, where the backend cannot tell, how the
  account behind it is billed, and the `[permissions]` rules a session answers
  prompts with before asking. `parse.rs` walks the spanned TOML document by hand so that an
  invalid file is reported as its key and line; `write.rs` splices one rule into the `allow`
  array at the byte range the parser gives it, so the operator's comments and ordering survive;
  `trust.rs` records which config files may put an `env`, `args`, a `settings` file or an
  `auth_refresh` in front of a backend, as each file's SHA-256 under the user's own config
  directory, because a repository's file arrives with the clone; `lib.rs` layers a repository's file over the user's, withholds
  what an untrusted file may not set, and selects the profile a session runs under.
- **`crates/niobe-store/`** — the session store: every event of every session, append-only, in
  SQLite (`store.rs`; the triggers in its schema refuse an update or a delete). `recorder.rs` is
  the write side a running session holds; `jsonl.rs` reads a JSON Lines event log.
- **`crates/niobe-tui/`** — the terminal UI (ratatui). `app.rs` is the state the shell draws from,
  including which pane has the keyboard — the one the scroll keys, the wheel and a section cursor
  go to — `ui.rs` draws it, `run.rs` is the event loop, `journal.rs` is the trait the loop hands the
  operator's events to and `rules.rs` the one it hands their standing answers to, `terminal.rs`
  enters and restores the terminal and the mouse it takes for the wheel and clicks, `theme.rs` is the
  palette — one table entry per theme in the sixteen ANSI names, which is what a terminal gets
  unless `COLORTERM` says it draws 24-bit colour; then the designed themes draw their own values,
  and `classic`, which is the sixteen, still honours the user's scheme — `text.rs` wraps and truncates, `markdown.rs` draws the assistant's replies
  from their markdown and wraps the styled text itself, so the transcript's line count stays
  exact, `hunks.rs` draws a file change under the call that made it as the lines that changed,
  `calls.rs` draws a tool call as a row of a table — its name in a fixed column, what it does,
  and on the right what it cost, by what the backend reported and the shell's own clock — and a
  run of calls to one tool as one group that Ctrl+O folds, `turns.rs` draws the rule under a
  finished turn with what that turn spent — its tokens, how far it moved the five-hour window and
  how long it took — giving up whole figures where the pane is narrow,
  and `fx.rs` is what the desktop between the panes does while a turn is running: a pure
  function of the theme, the strip's height and how long the turn has run, so a frame of it can
  be asserted rather than merely observed to move. Snapshot pictures of the screen live in
  `tests/snapshots/`.
- **`crates/niobe-bridge-claude/`**, **`crates/niobe-bridge-codex/`** — drive the official CLIs
  and translate their output into `niobe_core::Event`. Vendor wire types stay inside the bridge.
  In the Claude bridge: `wire.rs` is the CLI's stream-json protocol and is private to the crate,
  `translate.rs` turns one message of it into events and owns no process, `driver.rs` spawns the
  binary, keeps its standard input open for the life of the session and answers the permission
  prompts the CLI stops turns on; `transcript.rs` reads the session files the CLI keeps for
  itself, so a session started in plain Claude Code can be listed and carried on — it writes
  nothing back, and folds through the same `translate.rs` as the live stream; `conformance.rs`
  names the CLI releases every shape in the crate was recorded from, which is what a session says
  once when it is driving a release nobody recorded. `tests/stream.rs` and `tests/transcript.rs`
  fold the recordings in `tests/fixtures/`, whose README carries the arithmetic the tests assert —
  including the `git diff --numstat` figures the per-file `+`/`−` counts are checked against;
  `tests/conformance.rs` asserts those recordings' shape inventory — message types, `system`
  subtypes, stream events, content blocks, deltas — against a checked-in list, so a shape the
  bridge silently passes over cannot arrive with a new CLI release unnoticed, and checks that the
  recordings still carry what a sub-agent's figures and a shell command's exit status are read
  from;
  `tests/answers.rs` answers a recorded pair of permission prompts through a running `Session`,
  against a stand-in `claude` that waits for each answer as the CLI does.
  The codex bridge names its binary and does not spawn it yet.
- **`crates/niobe-cli/`** — the `niobe` binary. The only crate that writes to the terminal outside
  the TUI, and the only one that wires the others together: it finds the config files and opens
  the shell under the selected profile, with the session store as its journal and the
  repository's config as the place a standing answer is kept, and where `niobe trust` records
  that a repository's config may start a backend with what it names. `backend.rs` is the only module
  that names a bridge, so it is also where the `claude` CLI's own sessions are found and read in.
  `tests/cli.rs` runs the binary;
  `tests/pty.rs` runs it on a real terminal; `tests/permission.rs` walks a recorded permission
  prompt from the bridge's translation to the rule in the config, which is the one path only this
  crate may name both ends of.
  `tests/billing.rs` folds the bridge's recordings into the shell and compares the Usage pane
  with the pictures in its `tests/snapshots/`, one per way a session is billed.
- **`xtask/`** — workspace automation, run as `cargo xtask <task>`; the checks CI runs.
  `version.rs` is the workspace version: the eight places the root manifest writes it, and
  what the conventional-commit subjects since the last release tag call for (§8).
- **`install.sh`** — what users install niobe with: POSIX `sh`, picks the archive for the machine,
  checks it against its published SHA-256 and puts `niobe` in `~/.local/bin`. It is published
  with every release, so `releases/latest/download/install.sh` always matches the archives beside
  it. `crates/niobe-cli/tests/install.rs` runs it against a release laid out on disk.
- **`.github/workflows/release.yml`** — a tag `v<version>` builds macOS (arm64, x86_64) and static
  musl Linux (x86_64, arm64) binaries and publishes them, with their checksums and `install.sh`,
  as a GitHub Release. The tag must equal the workspace version, and an annotated tag's own message
  is what the release page says, so the notes are written and reviewed in git rather than in a web
  form after the binaries are out. Run by hand it builds and publishes nothing.

### 1.1 Layering (BINDING)

`xtask/src/main.rs` (`ALLOWED_WORKSPACE_DEPS`) is the source of truth, enforced by
`cargo xtask layering`:

- `niobe-core` depends on nothing in the workspace.
- `niobe-ledger`, `niobe-config`, `niobe-store`, `niobe-tui` and each bridge depend on
  `niobe-core` only.
- `niobe-cli` may depend on everything.

A type cannot leak out of a bridge into the TUI if the TUI cannot name the bridge. Widen the table
only on purpose, and say why in the commit.

### 1.2 Navigate via the knowledge graph first

When the `code-review-graph` MCP tools are available, use them before Grep/Glob/Read:
`semantic_search_nodes` / `query_graph` to locate code, `get_impact_radius` and
`get_affected_flows` for blast radius before editing, `query_graph` with `tests_for` to check
coverage, and `detect_changes` / `get_review_context` when reviewing a diff. Fall back to file
search only for what the graph does not cover.

---

## 2. Product invariants (BINDING)

These hold for every change. Breaking one is a bug even when the feature ships green.

1. **Only the official `claude` and `codex` binaries touch subscription credentials.** Niobe never
   reads their token files and never sets their user agents. No token extraction, no header
   spoofing — this is the line the whole bridge-first design stands on. A repository's config
   is not where that line is crossed either: a profile it defines does not set an `env`, pass
   `args`, name a `settings` file or run an `auth_refresh` until the operator has trusted that
   file's contents.
2. **No telemetry.** Network calls go only to configured providers and the CLIs.
3. **The terminal is restored on every exit path**, including panics and SIGTERM. This is why the
   release profile keeps `panic = "unwind"`.
4. **Every cost number is traceable to a `usage` field, or it is labelled** "estimate" /
   "API-equivalent". A fabricated number is indistinguishable from a measured one, which is the
   whole product gone. An unknown model reads "unpriced", never a guess. Where there is no number,
   the screen shows an em dash, never a zero.
5. **Each optimization has an off switch and a measured savings figure.** An optimization nobody
   measured is a behaviour change sold as a feature.
6. **`niobe-tui` sees no backend-specific type** (§1.1).
7. **CI is green on macOS and Linux**: format, clippy, tests, layering, headers and the release
   binary size budget.

---

## 3. Rust coding standards

- **Edition and toolchain**: edition 2024, `rust-version` from the workspace manifest. New
  dependencies go in `[workspace.dependencies]` and are inherited with `workspace = true`.
- **Naming**: `UpperCamelCase` for types and traits, `snake_case` for functions and variables,
  `SCREAMING_SNAKE_CASE` for constants. Test names are sentences that state the behaviour
  (`a_partly_reported_cost_reads_as_a_floor`), not `test_foo`.
- **Errors**: **never** `unwrap()` or `expect()` in library or binary code — propagate with `?`
  and a typed error, or handle the case. `expect` is allowed in tests, with a message that says
  why it cannot fail. Clippy enforces this (`unwrap_used`, `expect_used`).
- **Matching**: exhaustive `match` on domain enums (`Event`, `ToolOutcome`, `Backend`, …). Never
  `_` for a domain enum: a new variant must fail to compile everywhere it needs handling.
  A wildcard is acceptable only on foreign enums the code deliberately ignores most of.
- **No `unsafe`.** `unsafe_code` is denied workspace-wide.
- **No stray output.** `println!`/`eprintln!` are linted; only `niobe-cli` and `xtask` write to
  the terminal directly. No `dbg!`, no `todo!` in committed code.
- **Arithmetic on counters** saturates (`saturating_add`); a token total must never wrap.
- **Separation of concerns**: `struct` for data, `trait` for behaviour, no god objects. State is
  derived by folding events, not mutated from the side. The TUI never touches the filesystem; the
  CLI reads the environment and hands the TUI plain data.
- **Public API**: every public item has a doc comment that says what it is for and any invariant a
  caller must keep. Prefer accessors over public fields on types with invariants.
- **Small functions, one level of abstraction.** Extract a named function rather than add a
  comment explaining a block.
- **Formatting**: `cargo fmt` is the style. Do not hand-align.

### 3.1 Comments

Comments explain *why* — the constraint, the invariant, the alternative that was rejected — not
*what* the next line does. Keep them true: an edit that invalidates a comment updates or deletes it
in the same change. No commented-out code. No section banners (`// ---- helpers ----`); split into
functions or modules instead.

---

## 4. No internal planning references (BINDING)

**This repository is public. It must contain no trace of internal planning.** That means:

- **No planning identifiers**: milestone markers, task or phase numbers, ticket ids, priority
  labels, internal feature codes.
- **No references to private documents or tools**: planning pages, trackers, vision or research
  notes, design mocks, or any file that is not in this repository.
- **No roadmap language**: nothing that says *when* or *in which stage* something will be done.
  Code describes what it does today and, where it matters, what it does not do yet.
- **No deferral without context**: never mark a gap as unfinished without saying what is missing
  and why. "Not implemented yet: the bridge does not spawn the CLI" is fine; "comes later" is not.

**Where this applies**: code comments, doc comments, error messages, user-visible strings, test
names, snapshots, fixtures and their READMEs, manifests, CI config, and commit messages.

**Rule of thumb**: if a sentence would be confusing to someone without access to a private plan,
rewrite it to describe the implementation's actual behaviour, invariants or constraints.

---

## 5. Copyright headers (BINDING)

Every file that can carry a comment starts with the SPDX header, followed by a blank line:

```rust
// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko
```

- Rust (`.rs`): `//` comments, before any `//!` doc comment or `#![…]` attribute.
- TOML, YAML, `.gitignore`: the same two lines with `#`.
- Shell scripts (`.sh`): the same two lines with `#`, directly under the `#!` line.
- Markdown: the two lines inside a leading `<!-- … -->` block.

Exempt, because the format has no comments or the bytes are compared exactly: `LICENSE`,
`CLAUDE.md`, `Cargo.lock`, `*.jsonl` fixtures and `tests/snapshots/*.txt`. The list lives in
`xtask/src/main.rs`; a file type with neither a rule nor an exemption fails the check, so a new
kind of file forces a decision. Run `cargo xtask headers`.

The project is licensed under Apache-2.0 (`LICENSE`); `license = "Apache-2.0"` is inherited by
every crate from the workspace manifest.

---

## 6. Testing and verification (mandatory)

Testing is the only way to prove the work is correct. **Red-Green-Refactor is mandatory:**

1. **RED**: write a failing test; run it; confirm it fails for the right reason.
2. **GREEN**: the minimum code that makes it pass. No speculative generality.
3. **REFACTOR**: clean up names and extract functions, with the suite green.

Where tests live:

- **Unit tests** in a `#[cfg(test)] mod tests` at the bottom of the module they test.
- **Integration tests** in each crate's `tests/`:
  - `niobe-core/tests/replay.rs` folds the recorded log `tests/fixtures/claude-session.jsonl`. The
    expected numbers are derived from the fixture with `jq`, never from `SessionState`, so a bug in
    the fold cannot agree with itself. The `jq` program is in `tests/fixtures/README.md`; re-run it
    after replacing the fixture.
  - `niobe-tui/tests/shell.rs` draws the shell into a `TestBackend` and compares it with the
    pictures in `tests/snapshots/`. After an intentional layout change, regenerate with
    `UPDATE_SNAPSHOTS=1 cargo test -p niobe-tui --test shell` and **read the diff** of the
    snapshot files before accepting it. The session both draw is in `tests/common/mod.rs`.
  - `niobe-tui/tests/frame_budget.rs` times a 200×60 resize, and the steady redraw of a long
    session of markdown replies, against a 16 ms budget. It is a test binary of its own and its
    tests take turns on a lock, so that nothing shares their cores, and each asserts the median
    of 31 frames so that a frame the scheduler interrupted cannot fail it.
  - `niobe-cli/tests/pty.rs` runs the binary on a pty the test owns and reads back what reached
    the terminal. It is where **terminal restoration** is proven, on a clean quit, a SIGTERM and
    a panic; a unit test cannot, because the panic hook writes to the process's own standard
    output and the signal disposition belongs to the process. Watch it after touching
    `terminal.rs` or `run.rs`.

### 6.1 The gate

Run all of it, in this order, and report exact pass/fail counts — never infer success:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask layering
cargo xtask headers
cargo xtask version --check
cargo xtask size
```

`cargo xtask ci` runs the same sequence with `fmt --check`, exactly as CI does.

**Definition of done**: the gate is green, every acceptance criterion of the change has passed
RED-GREEN-REFACTOR, and the work is committed (§7.9). If a subagent reports a failure as
"pre-existing" or "out of scope", re-run that test yourself before trusting the verdict.

### 6.2 Root-cause-first debugging

The most expensive failure mode is a shallow fix: patch the first plausible symptom, the test goes
green, and the real mismatch surfaces a round later. Before editing a line to fix a bug:

1. **Reproduce it as a failing test first.** A bug you cannot reproduce is a bug you do not yet
   understand.
2. **Trace the full path** — bridge → event → session fold → app → draw — and find the exact
   `file:line` where behaviour first diverges from intent. Do not stop at the first place that
   looks wrong.
3. **State the root cause and the evidence** in one sentence before touching code. If more than
   one cause is plausible, disprove the losers.
4. **Fix at the correct layer**, then prove the repro passes and no sibling test went red. A fix
   that moves the symptom somewhere else is not done.

---

## 7. Workflow

1. **Research first.** Find the closest existing pattern and the blast radius (§1.2) before
   writing code.
2. **Incremental changes.** One crate or one layer at a time, green before moving on. Split large
   refactors into chunks of at most ~5 files; build and test after each.
3. **No brute force.** Read the compiler or clippy error and fix the cause; do not `sed` until it
   compiles, and do not `#[allow]` a lint away without a comment saying why it is right.
4. **Bulk edits.** After any mechanical transform (`sed`, header insertion, mass rename), re-read
   the affected files and build, to confirm nothing was truncated and no string literal broke.
5. **Blast radius before flipping a default.** Enumerate every dependent test, snapshot and call
   site, and update them in the same change.
6. **Temporary files** go in a scratch directory outside the repository, never in the tree.
7. **Out-of-scope discoveries** are tracked outside the repository (§0.1), never as a paragraph in
   a commit message. A `TODO` comment at the code location is fine if it says what is missing and
   why, in plain words — never a tracker id (§4).
8. **Keep documentation true.** A change to a crate's behaviour updates its doc comments and any
   README that describes it.
9. **Land the work.** The last step of a task: gate green (§6.1), then stage **exactly the files
   you touched, by name** (never `git add -A` or `git add .`), then commit, then push when asked
   or when the maintainer-local instructions say so. **The version is not part of this commit**
   — releasing is its own commit, cut only when the maintainer says so (§8).
   - **Commit messages**: Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`,
     `chore:`). The subject says what changed; the body explains *why* it is right rather than
     restating the diff. No planning identifiers (§4).
   - **Do not open pull requests** unless asked.
   - **`git stash` is forbidden.** Stashing a path with no changes creates no entry, so the next
     `git stash pop` pops whatever the maintainer had stashed. Copy files to a scratch directory
     if work must be set aside.
10. **Subagents never run git write commands.** A dispatched agent may run `git diff`,
    `git status` and `git log` — never `add`, `commit`, `push`, `stash`, `checkout`, `restore`,
    `reset` or `clean`. Say so explicitly in the dispatch prompt, and after every subagent
    completes check `git status --short` against the expected file set: a *shrinking* diff means
    work was wiped. Only the orchestrating agent commits.

---

## 8. Versioning and releases (BINDING)

The workspace carries one version, in `[workspace.package]`; every crate inherits it and the
binary's `--version` prints it. A release is the tag `v<version>`, which builds the binaries and
publishes them. The tag has to equal the workspace version, so the two move in one commit and never
separately.

### 8.1 What a bump means

Semantic versioning, with the 0.x contract written down: while the major version is `0`, `0.MINOR`
is the compatibility unit — it is what cargo resolves on — so a breaking change moves the **minor**,
and the patch stays fixes only. Reaching `1.0` is a decision about the product; no commit log makes
it.

The commit type the author already chose decides the bump, so a release turns on the record instead
of on a judgement made from memory at the end of a session:

| Commits since the last tag                  | Bump  | `0.4.0` becomes |
| ------------------------------------------- | ----- | --------------- |
| any `feat:`, or any `!` / `BREAKING CHANGE:` | minor | `0.5.0`         |
| only `fix:` / `perf:`                        | patch | `0.4.1`         |
| only `docs:` `test:` `chore:` `refactor:`    | none  | no release      |

`cargo xtask version` reads that off the log and prints it, with the commit that forces it. It is
the answer, not a starting point to re-derive. A commit whose type understates what it did is a bug
in the commit message: say so plainly, and bump for what the change actually is.

### 8.2 An agent never versions on its own

1. **A feature or fix commit never touches the version.** The version sites are written only in a
   release commit, so two branches cannot conflict over `Cargo.toml` and no merge invents a
   version.
2. **Cutting a release is the maintainer's call.** An agent proposes. It does not tag, and it does
   not bump because the work felt significant.

### 8.3 When to ask, and what to ask

After landing work (§7.9) — gate green, committed — run `cargo xtask version`. Then:

- **It reports that nothing calls for a release**: say nothing, ask nothing. Docs, tests, chores
  and refactors accumulate quietly until something releasable joins them.
- **It reports a bump**: ask **once**, with the answer already drafted, so the maintainer is
  approving a release rather than being handed a question:
  - **the version** the command computed, and the commit that forces it;
  - **the release notes in full**, ready to use as written — what a user of this release can now
    do, or what stopped being wrong for them, in the project's own voice. Not the commit subjects
    pasted into a list, not a diffstat, and nothing from §4. Someone reading the release page has
    never seen this repository's history.

  The maintainer may take that version, raise it, or decline. **If they decline, do not ask again
  for the same commits** — not when the next commit lands on top of them, not later in the session.

Never ask mid-task, and never ask about work that is not committed yet.

### 8.4 Cutting one

Only once the maintainer has said yes, with the notes they approved:

```sh
cargo xtask ci                        # the gate, first and whole (§6.1)
cargo xtask version <patch|minor|major|X.Y.Z>
git add Cargo.toml Cargo.lock         # a release commit carries nothing else
git commit -m "chore: release <version>"
git tag -a v<version> -F <notes-file> # annotated: its message is the release notes
git push origin HEAD v<version>
```

`cargo xtask version <bump>` moves all eight sites — `[workspace.package]` and the seven `niobe-*`
path dependencies — and refreshes `Cargo.lock`. Do it by hand and the sites drift; `cargo xtask
version --check` is in the gate for exactly that. It refuses a dirty tree, a version that does not
come after the current one, a bump smaller than the commits call for, and a tag that already
exists.

The notes file goes in a scratch directory, never in the tree (§7.6). The tag must be annotated
(`-a`): a lightweight tag has no message of its own, and the release would fall back to a generated
list of commit subjects.

**There is no changelog file.** The tag's message and the release page are the record, written once,
at the moment the maintainer approves them.

---

## 9. Common roadblocks

- **Snapshot test fails after a UI change**: expected. Regenerate (§6), read the snapshot diff,
  and accept it only if the new frame is right.
- **A test hangs in a pipe**: something asked the terminal for its cursor position and is waiting
  for a reply that never comes. `run.rs` avoids `Terminal::clear` for this reason.
- **Raw mode leaks into the test harness**: raw mode belongs to the process's controlling
  terminal, not to a writer. Tests use `TerminalGuard::enter_screen_only`, never `enter`.
- **Release binary over budget**: check what a new dependency pulls in with
  `cargo tree -p niobe-cli -e normal` before tuning the profile.
- **Timing tests flake**: the replay and frame budgets are generous for debug builds; a failure
  on a loaded machine should be re-run once before being treated as a regression, then
  investigated. A timing test that shares a binary with tests that draw or replay is timing them
  too: give it a binary of its own, as `frame_budget.rs` has.
