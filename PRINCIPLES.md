<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Niobe Engineering Principles

This document is the **binding standard** for every change to Niobe. `AGENTS.md` says how to work
and what the code is; this file says what *good* looks like, operationally: every principle has a
*trigger* (when it applies), a *check* (how you verify it) and a Niobe example. The skills under
`.claude/skills/` and the reviewers under `.claude/agents/` grade a change against it, by section.
Where this file and a habit disagree, this file wins; where it and `AGENTS.md` §2 disagree, §2
wins.

---

## 0. Reading guide

- **MUST / NEVER** — non-negotiable. A change violating one is rejected at review.
- **SHOULD** — strong default. Deviating needs one sentence of justification in the commit body.
- **CONSIDER** — judgment call. Write the decision down where it is made, in the code's own terms.

Every section ends with a **Self-check**. The reviewers walk them; so do you, before asking for
review.

---

## 1. Architecture: the crate graph is the architecture

Dependencies point one way, and `cargo xtask layering` fails the build when they do not
(`AGENTS.md` §1.1). What a crate *may* name is what it *may know*.

### 1.1 What crosses a boundary (MUST)

- **`niobe-core` is the vocabulary.** A backend produces `Event`s; every consumer folds them. A
  thing two crates both need to name — a file change, a test run, a kind of work, a proposal, a
  permission rule — lives here, in backend-neutral terms, or the two crates cannot talk.
- **Wire types stay in their bridge.** The CLI's stream-json shapes are `wire.rs`, private to
  `niobe-bridge-claude`. The moment a `serde_json::Value` or a CLI field name reaches the TUI, the
  TUI knows a backend, and the next backend has to lie in that shape to be drawn.
- **The TUI reaches the world through traits.** Everything the event loop needs from outside —
  the journal, the bridge, the working tree, prices, the clipboard, the shell, images, history,
  memory — is a trait in `niobe-tui`, implemented in `niobe-cli`, replaced by a stand-in in tests.
  The TUI never opens a file, spawns a process or reads an environment variable.
- **`niobe-cli` is the only place two ends meet.** It names the bridge *and* the TUI, the config
  *and* the store. A path that can be tested only there (`tests/permission.rs`, `tests/billing.rs`)
  belongs there and nowhere else.

### 1.2 The seam is the design (SHOULD)

- Read a backend's message as what it *means* (an edit, a command, a test run, a cost) in the
  bridge, and hand the meaning across. The TUI draws meaning; it does not parse.
- A reading that depends on a CLI's quirk — the middle-cut shell output, the spilled file, the
  `cost-state` on resume — is the bridge's to hide. If the TUI has to know the quirk, the seam is
  in the wrong place.

### 1.3 Self-check

- [ ] No `niobe-bridge-*` path in `niobe-tui`'s `Cargo.toml`; `cargo xtask layering` is green.
- [ ] Nothing that crosses from a bridge carries a CLI's own field names or JSON.
- [ ] Every new outside need of the loop is a trait in `niobe-tui` with a stand-in in `tests/`.
- [ ] A new shared concept went into `niobe-core`, not into two crates in parallel.

---

## 2. State is a fold

```
  backend bytes ─► bridge ─► Event ─► SessionState (fold) ─► App ─► draw
                                  └─► store (append-only) ─► replay ─► the same fold
```

### 2.1 Derive, never mutate from the side (MUST)

- Every number on screen is a function of the events folded so far. Nothing increments a counter
  because a key was pressed or a tick passed; the tick is itself handed in (`clock.rs`).
- A replayed session folds to the same `SessionState` as the live one did. `niobe replay`,
  `--resume` and the live shell are three readers of one fold, so a figure fixed in one is fixed
  in all three — and a figure that differs between them is a bug in the fold, not in a reader.
- Per-turn figures come from the same fold as whole-session ones (`session.rs`): a turn's rule
  under the transcript and the Usage pane cannot disagree.

### 2.2 The store keeps everything, forever (MUST)

- The session store is append-only, and its triggers refuse an update or a delete. A correction
  is a new event, not an edit. A row that no longer decodes is reported, not dropped.
- An event the journal could not keep is reported to the operator. Silence is the one thing a
  journal may not do.

### 2.3 Self-check

- [ ] The new figure is computed in `session.rs` (or a function it calls), not in `app.rs` or
      `ui.rs`.
- [ ] `niobe-core/tests/replay.rs`'s fixture numbers were re-derived with `jq`, not read off the
      fold.
- [ ] A new event kind is folded, stored, read back and drawn, and the replay tests cover it.
- [ ] No `&mut` state changes outside a fold step or an operator action the journal records.

---

## 3. Figures: measured, labelled or absent

The bill is one of the things on screen, and a fabricated number is indistinguishable from a
measured one (`AGENTS.md` §2.4). This section is what that invariant looks like line by line.

### 3.1 Provenance (MUST)

- Every cost or token figure traces to a `usage` field the backend reported, or carries the label
  the ledger gives it: `Measured`, `ApiEquivalent`, `Unpriced`. A figure the shell valued itself is
  drawn as an estimate.
- An unknown model is `Unpriced` and reads "unpriced". It is **never** priced at a neighbour's rate.
- Where there is no number, the screen shows an em dash. A `0` says "measured, and it was zero"; it
  is not a placeholder.
- A cache write is priced at the lifetime the CLI reported it with; tokens no reported cost covers
  are priced per request, never on their sum (`niobe-cli/tests/cache_lifetime.rs`, `tests/owed.rs`).

### 3.2 Arithmetic (MUST)

- Counters saturate. A token total that could wrap is a token total that will, in a long session.
- Money is summed in whole millionths of a dollar and becomes a float once, at the edge.
- A price is dated from the day it took effect, with its published source in the table's comments.
  A usage record is priced against the price in force on *its* day.

### 3.3 Self-check

- [ ] The new figure's provenance label is set where the figure is made, and drawn.
- [ ] No `unwrap_or(0)`, `unwrap_or_default()` or `.unwrap_or(…)` turns an absent figure into a
      zero.
- [ ] Every `+` on a counter is `saturating_add`.
- [ ] A new price row cites its source and its date; `niobe-ledger/tests/prices.rs` checks it by
      hand.

---

## 4. SOLID, for a terminal tool

### 4.1 Single Responsibility (MUST)

A module answers one question. `work.rs` says what kind of work a call did; `test_run.rs` reads
a test run; `tags.rs` names sub-agents. **Trigger to split**: the module doc needs an "and", or a
function's name does. **Check**: say what the file is for in one sentence without "and".

### 4.2 Open/Closed (SHOULD)

A new backend is a new `niobe-bridge-*` crate plus one arm in `backend.rs`. Nothing in the TUI,
the store or the ledger changes. **Anti-pattern**: `if backend == Backend::Claude` anywhere but the
one module that names bridges.

### 4.3 Liskov Substitution (MUST, for traits)

An implementor honours the trait's contract, and the contract is written on the trait. The
contracts that matter most here are about *time*: a `Journal`, `Watch`, `History`, `Memory` or
`Images` implementation **never blocks the draw path** — it runs on a thread of its own and hands
plain data back. A stand-in in tests must be a legal implementor too.

### 4.4 Interface Segregation (SHOULD)

A function that needs the transcript's width takes the width, not `&App`. A pass that reads
`SessionState` takes `&SessionState`, not `&mut`.

### 4.5 Dependency Inversion (SHOULD)

The loop depends on traits; `niobe-cli` supplies the implementations. **Trigger**: a test that wants
to drive the shell without a file, a process or a terminal cannot, because the dependency is
concrete. Refactor toward the trait.

### 4.6 Self-check

- [ ] One verb per function name; no `foo_and_bar`.
- [ ] No backend identity tested outside `backend.rs`.
- [ ] Nothing new on the draw path waits on a file, a process or a lock.
- [ ] A test constructs the subject without the binary, a terminal or a real CLI.

---

## 5. Clean code

`AGENTS.md` §3 carries the standards; these are the checks.

### 5.1 Functions (MUST)

- **Small.** Default ceiling 40 lines of body, hard ceiling 80. Over 80: split, or say in the commit
  body why not. `cargo clippy --workspace -- -W clippy::too_many_lines` lists the offenders
  against `clippy.toml`'s threshold; it is advisory, since the tree has incumbents.
- **One level of abstraction.** A function that both measures cells and decides layout is two.
- **At most four arguments.** Above: a struct with a name.
- **No flag arguments.** A `bool` that changes behaviour is two functions.
- **Guard clauses over nesting.** Return early.

### 5.2 Naming (MUST)

Functions are verbs, types are nouns, predicates read as predicates (`is_managed`, `has_header`).
Test names are sentences stating the behaviour. No abbreviation unless it is the term of art in
the surrounding code (`tui`, `pty`, `mcp`).

### 5.3 Comments (MUST)

Why, not what: the constraint, the invariant, the alternative rejected. A comment that restates the
line below it is deleted. A comment an edit made false is fixed in the same change. No banners, no
planning references, no `FIXME`/`HACK`/`XXX` — `cargo xtask audit` refuses all three. A `TODO` is
allowed where it says what is missing and why, in plain words.

### 5.4 Errors (MUST)

Library and binary code propagates with `?` and a typed error that names the file, the line and the
key where there is one (`niobe-config/src/error.rs`, `niobe-ledger/src/error.rs`). `unwrap` and
`expect` are clippy errors outside tests; in a test, `expect`'s message says why it cannot fail.

### 5.5 Matching (MUST)

Exhaustive `match` on every domain enum (`Event`, `ToolOutcome`, `Backend`, `Rule`, …). A `_ =>`
is acceptable only on a foreign enum the code deliberately ignores most of (a crossterm key code, a
`serde_json::Value`). A new variant must fail to compile everywhere it needs a decision.

### 5.6 Self-check

- [ ] No function in the diff exceeds 80 lines, or the commit body says why.
- [ ] No new `_ =>` over an enum this workspace defines.
- [ ] No new comment restates code, names a plan, or marks a section.
- [ ] Every public item has a doc comment that says what it is for and what a caller must keep.

---

## 6. Hostile input and the terminal

Niobe reads bytes it did not write — a CLI's stream, the CLI's own transcript files, a config file
that arrived with a clone, a pasted path, the clipboard — and writes to a terminal that executes
what it is sent. Treat every reader as a boundary and the terminal as a sink that can be hurt.

### 6.1 Every reader is bounded and fails with a notice (MUST)

- A reader checks it has a regular file before it reads, reads only up to a stated size (a mebibyte
  of config, 256 MiB of transcript, 64 MiB per output line) and never waits on a FIFO.
- Bytes that are not UTF-8, a record cut off mid-character, a shape nobody recorded: each is passed
  over **with a notice**, and the rest still reads. Nothing a CLI can emit may panic, hang or stall
  the screen.
- An unknown shape is recorded as unknown, not guessed at: `conformance.rs` names the releases the
  shapes were recorded from, and a session says once when it is driving one nobody recorded.

### 6.2 The terminal is a sink (MUST)

- Content from outside is drawn through the text layer, which strips what would act on the
  terminal. `niobe-tui/tests/escape.rs` hands every place the shell draws content a payload of
  control characters, escape sequences and direction marks; a new place to draw content joins it.
- Every line the binary prints outside the TUI goes through `printable.rs`.
- The terminal is restored on every exit path — a clean quit, SIGTERM, SIGINT, SIGQUIT, a panic, a
  stop and continue. `niobe-cli/tests/pty.rs` proves it; the release profile unwinds for it.

### 6.3 Processes end with the session (MUST)

Everything the session starts — the CLI, each `!` command, an editor — runs in a process group
the session owns and the reaper ends when the session's pipe closes. A command that leaves its
group with `setsid` is the one thing left running, and the doc says so.

### 6.4 The credential line (NEVER)

Niobe never reads a CLI's token files, never sets a user agent, never opens a connection
(`AGENTS.md` §2.1–2.2). `cargo xtask layering` reads the tree for the names; review reads it for
the behaviour. A repository's config puts nothing in front of a backend until the operator has
trusted that file's bytes.

### 6.5 Self-check

- [ ] The new reader refuses a non-regular file and stops at its size limit, and a test shows both.
- [ ] A truncated, non-UTF-8 and over-long input each yields a notice and a working session.
- [ ] New drawn content is covered by `tests/escape.rs`.
- [ ] A new child process is in a group the reaper is told of.
- [ ] Nothing new names a credentials file, a user agent, a keychain or a network program.

---

## 7. Tests are the specification

`AGENTS.md` §6 mandates RED → GREEN → REFACTOR and lists where tests live. These are the
Niobe-specific traps.

### 7.1 Numbers come from outside the code under test (MUST)

- A fixture's expected figures are derived with `jq` from the recording, and the program is in the
  fixture's README. Never read an expected value off `SessionState`: a fold cannot check itself.
- A price test's expected dollars are worked out by hand from the dated table.
- A per-file `+`/`−` count is checked against `git diff --numstat`.

### 7.2 Snapshots are pictures, read before accepted (MUST)

`UPDATE_SNAPSHOTS=1` rewrites them. **Read the diff** of each snapshot file and accept only a frame
that is right. A snapshot regenerated blind turns the test into a tautology. Never on CI.

### 7.3 Terminal behaviour is proven on a terminal (MUST)

Raw mode, restoration, key protocols and stops belong to the process's controlling terminal, so
they are proven in `niobe-cli/tests/pty.rs` against stand-in `claude` scripts, not in a unit test.
Tests that draw use `enter_screen_only`, never `enter`.

### 7.4 Timing tests are timed in release, on a binary of their own (MUST)

The frame and replay budgets assert the median (or fastest) of 31 runs in `--release`; the debug
suite reports them ignored. A timing test that shares a binary with drawing tests is timing them.

### 7.5 Error paths and hostile inputs are tests, not comments (MUST)

Every notice a reader can emit has a test that provokes it. Every refusal (`a FIFO`, `a link to
/dev/zero`, `a rule in Claude Code's own :* form`) has a test that shows the refusal and its words.

### 7.6 Self-check

- [ ] The RED run was seen failing for the stated reason before any code changed.
- [ ] No expected number was read from the fold; the derivation is in a README or the test.
- [ ] Every regenerated snapshot's diff was read.
- [ ] Each new error path and each new reader limit has a test.

---

## 8. Change-risk tiers

Review effort follows risk, decided by triggers, not by feeling. The `niobe-task` skill classifies
first, then spends review in proportion. When unsure between two tiers, pick the higher.

### 8.1 Major-risk triggers (any one → **Major**)

- A new or changed `Event` variant, or a change to `SessionState`'s fold.
- Anything on a figure's path: `niobe-ledger`, usage translation in a bridge, the Usage pane's
  arithmetic, a provenance label.
- `terminal.rs`, `run.rs`, `input.rs`, `keys.rs`, signal handling, or a process group
  (`commands.rs`, `reaper.rs`, the driver's spawn).
- `trust.rs`, `permission.rs`, config layering, the `[permissions]` path, `write.rs`/`replace.rs`.
- The store's schema or triggers; `recorder.rs`.
- The layering table, a new dependency, `pre_exec`, any `#[allow(unsafe_code)]`.
- A new place the shell draws content that arrived from outside.
- `wire.rs`'s shapes or the conformance inventory; `install.sh`; `release.yml`.

### 8.2 Tier definitions

| Tier         | What it is                                                                                   | Review it earns                                                         |
| ------------ | -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| **Trivial**  | Typo, comment, doc, rename; at most two files; no logic; none of the §8.1 paths touched.       | The gate (`cargo xtask ci`). No reviewer.                               |
| **Standard** | One feature or fix; no §8.1 trigger.                                                          | TDD, then `niobe-reviewer` alone, then the gate.                        |
| **Major**    | Any §8.1 trigger.                                                                            | Design pass, TDD, the full `niobe-audit` panel, fix loop, CTO verdict.  |

Record the tier and the trigger that set it in the task's report.

---

## 9. Review ownership: one axis, one owner

Redundant review produces conflicting findings. Each axis has one owner; a reviewer checks only its
own axes and names the owner for anything else it notices. Mechanical checks belong to the gate and
are **not** re-grepped by any agent.

| Axis                                                                                                                           | Sole owner                   |
| ------------------------------------------------------------------------------------------------------------------------------ | ---------------------------- |
| Formatting; `unwrap`/`expect`/`print`/`dbg`/`todo` in code; the crate graph; network and credential names; headers; banners, planning references and unexplained gaps; the version sites; the binary size | **the gate** (`cargo xtask ci`) |
| Crate boundaries as design, trait-vs-concrete, fold-not-mutate, SRP, DRY, altitude, docs kept true                               | **Lead Software Architect**  |
| The event vocabulary, what a fold derives, provenance of figures, backend-neutrality of what crosses a seam, the trust model, process and terminal lifecycle as design | **Lead Niobe Architect** |
| Hostile input (CLI output, transcripts, repo config, paths, clipboard), escape injection, reader limits, the credential line, trust bypass, link/FIFO/size attacks, process leaks, wrapping arithmetic | **Lead Security Engineer** |
| Ownership and borrows, clone and allocation hygiene, iterator vs loop, draw-path and fold cost, error-handling shape            | **Lead Rust Engineer**       |
| Coverage, test honesty: hand-derived numbers, snapshot diffs read, error paths, hostile-input tests, test names, timing-test placement | **Lead QA Engineer**      |
| Cell widths, wrapping, tab expansion, escape stripping, key and paste protocols, raw mode and restore, frame budget, mouse        | **Lead Terminal Engineer**   |
| Every figure's trace to a `usage` field or its label, saturation, price dating, plan vs metered, the em dash                     | **Lead Accounting Engineer** |
| Cross-specialist synthesis, severity adjudication, practical sense, go/no-go                                                     | **CTO**                      |

A genuine cross-owner conflict (Rust wants a clone gone; Security says it decouples a hostile
buffer from the draw) goes to the CTO with evidence. Re-raising another owner's axis at a different
severity is noise, not conflict.

---

## 10. Severity rubric (canonical)

Every reviewer cites this scale. **Critical and major block; minor may defer with a recorded
follow-up.**

| Severity     | Definition                                                                 | Niobe examples                                                                                                                                                                                                                                                          |
| ------------ | -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **critical** | A product invariant (`AGENTS.md` §2) broken, or the operator misled.       | A figure drawn as a number that no `usage` field backs and no label marks; a `0` where an em dash belongs; a model priced by guess; a counter that can wrap; the terminal left raw on an exit path; a backend type in the TUI; a CLI's credentials file or a user agent named; an untrusted repository config putting an `env` or `args` in force; a store row updated or deleted; bytes a CLI can emit that panic, hang or stall the shell; content drawn with its escapes intact; a child process the session leaves running. |
| **major**    | A real defect or debt that must be fixed before ship, but no invariant broken. | A `_ =>` over a workspace enum; a changed path with no test; a snapshot accepted unread; an expected figure read off the fold; a fixture README whose arithmetic no longer matches; a function past 80 lines without a reason; a comment or doc an edit made false; a frame or replay budget regression; a reader without a size limit on a non-hostile path; a missing error-path test. |
| **minor**    | Style, idiom, defence in depth.                                             | A naming nit; an avoidable clone off the draw path; a doc comment that could say more; a theoretical hardening with no trigger.                                                                                                                                       |

A finding the owner cannot reproduce at a cited `file:line` is not a finding. The CTO spot-checks
every critical and any finding two specialists ranked differently; an uncontested major that cites a
line is trusted.

---

## 11. Smells

Flag on sight:

| Smell                                                              | Why it is bad                                            | Fix                                                        |
| ------------------------------------------------------------------ | -------------------------------------------------------- | ---------------------------------------------------------- |
| A figure made in `app.rs` or `ui.rs`                               | A second source of truth beside the fold.                | Derive it in `session.rs`; draw it.                         |
| `unwrap_or(0)` on a token or cost                                  | A fabricated zero reads as measured.                     | Carry the `Option`; draw an em dash.                        |
| A `serde_json::Value` or CLI field name outside a bridge           | The TUI knows a backend.                                 | Read the meaning in the bridge; cross with a core type.     |
| A file, process or env read in `niobe-tui`                         | Untestable; blocks the draw.                             | A trait, implemented in `niobe-cli`.                        |
| `_ =>` over an enum the workspace defines                          | A new variant compiles and is silently ignored.          | Exhaustive match.                                           |
| A reader with no size limit or `is_file` check                     | A FIFO or a vast file hangs or fills the shell.          | `read.rs`'s pattern: regular file, bounded read, a notice.  |
| Content drawn without the text layer                               | A CLI can act on the terminal.                           | Draw through `text.rs`; add the place to `tests/escape.rs`. |
| `UPDATE_SNAPSHOTS=1` with no diff read                             | The test now asserts whatever the code does.             | Read each snapshot's diff; revert the wrong frame.          |
| An expected number copied from `SessionState`                      | The fold checks itself.                                  | Re-derive with `jq`; put the program in the README.         |
| A `+` on a counter                                                 | Wraps in a long session.                                 | `saturating_add`.                                           |
| A timing assertion in a binary that also draws                     | Measures the other tests' load.                          | A binary of its own, timed in `--release`.                  |
| A `Journal`/`Watch`/`History` call on the draw path                 | A locked store or a slow `git` freezes the screen.       | A thread of its own; hand back plain data.                  |
| A `TODO` with no "what" and "why", or any `FIXME`/`HACK`/`XXX`     | A gap nobody can act on.                                 | Say what is missing and why, or delete it.                  |
| A comment naming a milestone, task, phase or plan                   | The reader has no plan.                                  | Describe the behaviour and its reason.                      |

---

## 12. How this document is enforced

- **The gate** (`cargo xtask ci`, `AGENTS.md` §6.1) mechanizes what can be: format, the clippy
  lints, every test, the crate graph and the credential and network names (`layering`), the
  headers, the comment sweep (`audit`), the version sites and the size budget.
- **`niobe-task`** is the default path for a feature or fix: one agent, TDD, a self-review through
  every lens in §9, the gate, then landing. **`niobe-panel-task`** is the same with the specialist
  panel, for Major-tier work. **`niobe-audit`** reviews and fixes an existing diff with the panel.
  **`niobe-scout`**, **`niobe-bolt`** and **`niobe-sentinel`** each land one proven bug fix, one
  measured performance win or one hardening. **`niobe-hilo`** names one highlight and one lowlight,
  read-only.
- **`niobe-reviewer`** is the single reviewer for a Standard-tier change; the `lead-*` agents are
  the Major-tier panel, each owning the axes §9 gives it.

When in doubt, optimize for the next reader. In this repository the next reader is often the
operator watching the screen, who has to be able to trust every figure on it.
