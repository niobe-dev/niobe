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

§1–§7 describe a finished change; §13 is the order it is made in — make it work, then make it
right, then practise — and every skill follows it.

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
| Crate boundaries as design, trait-vs-concrete, fold-not-mutate, SRP, DRY and one decision in one place, altitude, subtract before add, no bolt-on, illegal states unwritable, CRAP and duplication figures (§13), docs kept true | **Lead Software Architect**  |
| The event vocabulary, what a fold derives, provenance of figures, backend-neutrality of what crosses a seam, the trust model, process and terminal lifecycle as design, idempotent operations and one writer per shared state (§13.1) | **Lead Niobe Architect** |
| Hostile input (CLI output, transcripts, repo config, paths, clipboard), escape injection, reader limits, the credential line, trust bypass, link/FIFO/size attacks, process leaks, wrapping arithmetic | **Lead Security Engineer** |
| Ownership and borrows, clone and allocation hygiene, iterator vs loop, draw-path and fold cost, error-handling shape            | **Lead Rust Engineer**       |
| Coverage, test honesty: hand-derived numbers, snapshot diffs read, error paths, hostile-input tests, test names, timing-test placement, behaviour not implementation, surviving mutants (§13.2) | **Lead QA Engineer**      |
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
| A test that passes when the code under it returns nothing          | It asserts nothing the user would see.                   | Assert the result the user sees; delete it otherwise (§13.2). |
| A rule spelled in a second module                                  | Two places decide; the next change updates one.          | Call the one place, or move the rule there (§13.1).         |
| A new requirement as a `bool` on an old function                   | A bolt-on; the old design now lies about what it does.   | Redesign as if it had been there from the start (§13.1).    |
| A third fix on a premise two fixes already failed on               | Churn; the premise is what is wrong.                     | Write the premise down, remove the assignment (§13.1).      |

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
- **§13** orders the work inside every skill: make it work, then make it right — with the
  companion tools' CRAP, duplication and mutation figures on the changed code where they are
  installed — then practise. They are advisory, outside the gate, since CI does not install them.
- **`niobe-reviewer`** is the single reviewer for a Standard-tier change; the `lead-*` agents are
  the Major-tier panel, each owning the axes §9 gives it.

When in doubt, optimize for the next reader. In this repository the next reader is often the
operator watching the screen, who has to be able to trust every figure on it.

---

## 13. The order of work: make it work, make it right, then practise

§1–§7 say what a finished change looks like. This section says the order a change is made in, so
that the same standard is reached the cheap way rather than by repair. It is adapted from Robert C.
Martin's [ub-mode](https://github.com/unclebob/ub-mode) (MIT), read in Niobe's terms. Every change,
whatever its tier, goes through the three phases in order: **make it work**, **then make it
right**, **then practise**. The skills follow them step by step; the reviewers grade against them.

### 13.1 Make it work (MUST)

- **One decision lives in one place.** Line arithmetic is `diff.rs`'s, a key's meaning is
  `keys.rs`'s, a figure's label is the ledger's. **Trigger**: the change writes a rule a second
  module already holds. **Check**: grep for the rule's other spelling before adding yours; call
  the one place, or move the rule there.
- **Ship the smallest edit that solves the problem.** No drive-by refactor, no speculative
  generality, no option nobody asked for.
- **Get the data shape right before the logic.** A new fact is a core type first — the `Event`
  field, the `Option` that says "not reported", the provenance it carries — and only then the
  translation that makes it and the fold that reads it. A wrong shape costs every later phase.
- **Do first what helps every later phase.** The recording, the fixture README's `jq` program,
  the stand-in `claude` script come before the code that reads them, because every test after
  leans on them.
- **If a concurrent writer would matter, do not share the state.** The last two rules of this
  section say how.
- **Write the failing test before the code, see it fail for the reason you expect, make the
  smallest change that passes it** (`AGENTS.md` §6). **If an honest test is impractical, do not
  write a bad one**: say in the report what is unproven and why, rather than a test that asserts
  the implementation back at itself.
- **Redesign from first principles; do not bolt on.** A new requirement that arrives as a `bool`
  on an old function, a second `match` beside the first, or a special case for one backend is a
  bolt-on. Ask what the design would be had the requirement been there from the start, think it
  through, and deliver it in pieces, each green.
- **Subtract before you add.** Take out the dead branch, the duplicate rule, the adapter nobody
  needs, before putting anything in. A diff that only grows is a smell when the area already had
  one way to do the job.
- **Attack the premise.** When two fixes that share one premise fail the same gate, stop. Write the
  premise in one sentence, name the actors that hold the imbalance — two writers of one file, two
  readers that disagree about a figure, a bridge and the fold that both decide a turn ended — and
  remove the assignment that gave the thing to both. **No third fix starts before that sentence is
  written.**
- **Aim at the end state you can check.** The acceptance criterion is a test or a measured figure
  before it is code. A break in the middle — a red test, a stubbed reader — is allowed only when it
  was planned and can be undone.
- **Build the lever.** Work that is not obvious at a glance gets a tool, as the headers, the
  comment sweep, the version sites and the layering check did (`xtask/`). Do one unit by hand, then
  make the tool reproduce that unit exactly.
- **Model the domain so illegal states cannot be written.** An absent figure is `None`, never `0`;
  a provenance is an enum, never a flag; a state machine is an enum of states, not three `bool`s.
  The names in `AGENTS.md` §1 and the doc comments of `niobe-core` are the glossary: a new domain
  term is defined there, once, in the change that introduces it. Where the code is already clear
  and local, leave it.
- **Check data where it enters; inside, trust it.** Bytes from a CLI are parsed once, in the
  bridge; a config once, in `parse.rs`; a path once, where it is pasted. The fold and the drawing
  code take the parsed type and do not check it again. Business logic — the fold, the pricing, the
  tagging — is a pure function of its inputs.
- **Do not silence the compiler.** No `#[allow]` without the comment that says why it is right
  (`AGENTS.md` §7.3), no `as` that can truncate a counter, no `_ =>` that hides a new variant.
- **Make operations idempotent.** Run twice, or run again after a crash halfway through, the
  operation ends in the same state: `replace.rs` renames a flushed copy so a crash leaves the old
  file, and a standing rule the config already lists is not written again (`write.rs`).
  **Check**: the test runs the operation twice, and where it writes a file, once more after a
  simulated interruption.
- **Separate before serializing shared state.** If two parts do not need the same mutable thing,
  each gets its own. If they do, one writer, enforced by the structure: the journal writes the
  store from a thread of its own, the rule writer holds a lock on the config's directory. A mutex
  shared by everyone is the last resort, not the first.

### 13.2 Then make it right (MUST)

- **Fix the root cause** (`AGENTS.md` §6.2). Reproduce it, fix the cause. A guard that only hides
  the crash — an early `return`, a `.min()`, a caught panic — is not a fix.
- **Finish one small unit and check it, then start the next** (`AGENTS.md` §7.2).
- **Test behaviour, not implementation.** The test calls the code the way its user does — the
  shell through `TestBackend`, the binary through `tests/cli.rs` or the pty, the fold through
  events — and checks what that user would see. **If the test would still pass when every function
  under it returned nothing, an empty `Vec` or its default, throw it out.**
- **Measure the changed code** with the companion tools, run from the repository root on what the
  change touched (`--changed`). They are checked out side by side outside the repository (mutator
  reads crapper's checkout beside it), write their snapshots to `.metrics/`, which git ignores,
  and are not part of the gate: CI does not install them. Where they are installed, the report
  gives their figures; where they are not, it says so.

  | Tool | Command | Bar for a function the change added or edited |
  | ---- | ------- | ---------------------------------------------- |
  | [crapper](https://github.com/unclebob/crapper) | `crapper --changed` | CRAP below 10 — split the function, test it, or both. A long `match` over a domain enum is weighed rather than split blindly: §5.5 wants it exhaustive. |
  | [dryer](https://github.com/unclebob/dryer) | `dryer --changed --threshold 0.8` | No pair at 0.8 or above that the change made — fold it into the one place (§13.1). |
  | [mutator](https://github.com/unclebob/mutator) | `mutator --changed` | Every covered mutant killed; an uncovered site covered or deleted as dead code. |

  The coverage crapper and mutator read is `cargo llvm-cov`'s LCOV. A survivor in a figure's path
  (§8.1) is a major finding (§10): the tests would let that figure change unnoticed.
- **Refactor under a pin.** Pin what the code does with a test that would catch a change, change
  the structure, prove the behaviour did not move — the same tests, the same snapshots, unedited.

### 13.3 Then practise (SHOULD)

- **Keep a log.** Every action taken and the reason for it, in order — the test written and why
  it failed, the premise attacked, the tool run and its figure — reported when the task ends. The
  skills' final reports carry it.
- **Guard the context window.** Keep the main thread small: the gate runs in `niobe-test-runner`,
  a wide search in `niobe-explorer`, and the main thread keeps their summaries, not their output.
- **Clean up worktrees and scratch.** At the end, find what is safe to remove — a worktree whose
  branch is merged, a scratch copy of a recording — confirm nothing still in use needs it, then
  reclaim the disk (`target/` grows fast). A worktree with uncommitted work waits for the
  maintainer's decision; it is never removed to tidy up.
- **Commit often, in small ordered commits** (`AGENTS.md` §7.9). Each commit says why, what
  changed and how it was proven — the test that went from red to green, the gate's counts, the
  tool's figure — and the commits read in an order where each one builds and passes.

### 13.4 Self-check

- [ ] The rule the change needed lives in one place, and the diff took something out where it could.
- [ ] The type came before the logic; an absent value is `None`, an illegal state unwritable.
- [ ] No new requirement rides on a flag argument or a special case beside the old design.
- [ ] Two failed fixes on one premise were followed by the premise written down, not a third fix.
- [ ] Every new test would fail if the code under it returned nothing.
- [ ] Where the companion tools are installed, changed functions are under CRAP 10, no new pair
      scores 0.8, no covered mutant survives — or the report says why, or that they are absent.
- [ ] An operation that writes state was run twice in a test and ended the same.
- [ ] The report carries the log of actions and reasons; worktrees and scratch are cleaned up.
