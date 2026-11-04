---
name: niobe-task
description: Fast single-agent end-to-end execution of a Niobe task — no subagents. Classifies the change's risk tier (PRINCIPLES.md §8), then makes it work, makes it right and practises (PRINCIPLES.md §13) — implements it yourself with TDD (Red-Green-Refactor), measures the changed code, self-reviews the diff through every specialist lens (architecture, the fold and the figures, hostile input and the terminal, Rust idiom, test honesty), runs the whole gate and lands the work. Use for everyday features and fixes, or /niobe-task <task>. For Major-tier work that wants independent eyes, use niobe-panel-task.
---

<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Niobe task — single-agent fast path

**You do the whole task yourself, in the main thread. No subagents.** One engineer who implements,
reviews their own diff through every lens, tries to break it, and refuses to call it done until the
gate is green and the work is landed. This path exists because delegating everyday work to a panel is
slow, and subagents over-report progress, miss real failures and run out of context. You keep the
context; you own the result.

**Binding standard: `PRINCIPLES.md` and `AGENTS.md` at the repository root.** Read the sections the
change touches before writing code. If `AGENTS.local.md` exists, read it first and follow it
(`AGENTS.md` §0.1).

If the task trips a Major-risk trigger (`PRINCIPLES.md` §8.1 — a new `Event` variant, a figure's
path, `terminal.rs`/`run.rs`, trust or permissions, the store schema, a process group, the layering
table) and independent review is wanted, say so and recommend `niobe-panel-task`. Otherwise proceed.

## Explore through the code graph first

Before any Grep/Glob/Read sweep, use the `code-review-graph` tools (`AGENTS.md` §1.2):
`semantic_search_nodes` / `query_graph` to find the closest existing pattern, `get_impact_radius` and
`get_affected_flows` for the blast radius before editing, `query_graph` with `tests_for` for the
coverage that exists. Fall back to file search only for what the graph does not cover, and read the
span you need, not the file.

## Procedure

The steps fall in the three phases of `PRINCIPLES.md` §13, in order: **make it work**, **then make
it right**, **then practise**. Keep the log as you go (§13.3): each action and the reason for it,
one line each, from step 1. The final report carries it.

### Make it work

1. **Understand and challenge.** Restate the task and its acceptance criteria in your own words, as
   end states you can check (§13.1). Ask where scope, semantics or success are ambiguous. Challenge
   the request where it makes practical sense: wrong layer, a figure that would have no
   provenance, a seam that would put a CLI's shape in the TUI, a simpler design. Do not code until
   scope is confirmed.
2. **Classify the tier** (`PRINCIPLES.md` §8). Walk the §8.1 triggers against the confirmed scope and
   name the tier and the trigger that set it. The tier is re-confirmed after implementation, since
   the real diff may trip a trigger the plan did not.
3. **Map the change.** Place it on the path — bridge (`wire.rs` → `translate.rs`) → `Event` →
   `SessionState` fold → `App` → draw — or on the binary's edges (config, store, processes). Name
   new files against the closest analog. Run `get_impact_radius` so every consumer, test and snapshot
   you must update is known now, not one breakage at a time (`AGENTS.md` §7.5). Then, before any
   code (§13.1):
   - **Find the one place** the decision belongs. If a module already holds the rule, the change
     calls it or moves the rule there; it does not spell it a second time.
   - **Subtract first.** Name what the change can take out — a dead branch, a duplicate rule, an
     adapter — and take it out before adding.
   - **Redesign rather than bolt on.** If the requirement would ride on a `bool`, a second `match`
     or a special case, say what the design would be had it been there from the start, and plan it
     in pieces, each green.
   - **Shape the data first.** The core type, the `Option` for "not reported", the enum that makes
     the illegal state unwritable — then the logic. Parse outside data once, where it enters.
   - **Build the lever** where the work is repetitive and not obvious at a glance: one unit by
     hand, then the tool that reproduces it.
4. **Implement with TDD, gated (never skipped).** For a **bug**, the root-cause protocol first
   (`AGENTS.md` §6.2): a failing repro, the full path traced, the cause stated as `the bug is X at
   file:line, proven by Y` before any edit. Then, per acceptance criterion:
   - **RED** — the failing test first, where `AGENTS.md` §6 says that kind of test lives. Run it. It
     must fail for the stated reason; a test that passes at once is wrong. If an honest test is
     impractical, write none and say in the report what is unproven and why.
   - **GREEN** — the smallest change that passes. No speculative generality, no drive-by refactor.
   - Cover the error path and the hostile input (`PRINCIPLES.md` §7.5): the notice, the refusal,
     the truncated record. An operation that writes state runs twice in a test and ends the same
     (§13.1).

### Then make it right

5. **Refactor under the pin.** Names, extraction, duplication, with the same tests and snapshots
   green and unedited after each step (§13.2).
6. **Measure the changed code** (§13.2) where the companion tools are installed, from the
   repository root: `crapper --changed` (every added or edited function under CRAP 10),
   `dryer --changed --threshold 0.8` (no new pair at 0.8 or above), `mutator --changed` (every
   covered mutant killed, every uncovered site covered or deleted). Split, test or delete until
   the bars hold, or say in the report why one does not. Where the tools are absent, say so; do not
   install them into the repository.
7. **Self-review through every lens** (`PRINCIPLES.md` §9), fixing as you go:
   - **Architecture** — the crate graph is honoured; what crosses a seam is a core type; nothing in
     `niobe-tui` touches a file, a process or the environment; the new outside need is a trait; each
     decision lives in one place; nothing was bolted on.
   - **The fold** — the figure is derived in `session.rs`, once, and replay gives the same answer.
   - **The figures** — every number traces to a `usage` field or carries its label; no `unwrap_or(0)`;
     counters saturate; where there is no number, an em dash.
   - **Hostile input and the terminal** — every reader bounded, refusing a non-regular file; every
     drawn content through the text layer and in `tests/escape.rs`; every child in a process group;
     nothing names a credentials file, a user agent or a network program.
   - **Matching** — exhaustive over every workspace enum; `_ =>` only on foreign ones.
   - **Rust** — no needless clone on the draw path or in the fold, iterators over index loops, `?`
     over long-hand matches, no avoidable O(n²) per frame.
   - **Terminal** — widths by cells, wrap counts exact, tabs expanded, snapshot frames right.
   - **Docs** — every doc comment and README the change made false is fixed in the same change
     (`AGENTS.md` §7.8); a new public item has a doc comment; a new domain term is defined once.
8. **Mechanical-transform safety.** After any `sed`, bulk rename or header insertion, re-read the
   touched files and build (`AGENTS.md` §7.4).
9. **Adversarial self-QA.** Switch hats and try to break it: the empty session, the widest line, a
   wide character at the wrap, a record cut mid-character, a `0` where an em dash belongs, a resumed
   session billed twice, a FIFO where a file was expected. Audit your own tests for green-washing: a
   snapshot regenerated unread, an expected number read off the fold, a name that describes the
   implementation rather than the behaviour, a test that would still pass if the code under it
   returned nothing (§13.2). Add what is missing; delete what asserts nothing.
10. **Run the gate yourself and report exact counts** (`AGENTS.md` §6.1), in order; `cargo xtask ci`
    is the whole of it — in `niobe-test-runner` where its output would crowd the main thread.
    Read the actual output. If any note calls a failure "pre-existing", re-run that test yourself
    before trusting it.
11. **Loop tight, and attack the premise.** Fix, re-run what the fix touched, the whole suite before
    done. When two fixes that share one premise fail the same gate, stop: write the premise in one
    sentence, name the actors that hold the imbalance, remove the assignment (§13.1). No third fix
    before that sentence. If the premise cannot be found, surface it; do not churn.

### Then practise

12. **Follow-ups.** Record what you found and did not do where the maintainer-local instructions say
    (`AGENTS.md` §0.1, §7.7), with a `TODO` at the code site that says what is missing and why.
    Never a paragraph in the commit message, never a planning identifier in the tree.
13. **Land it** (`AGENTS.md` §7.9). Gate green → stage exactly the files you touched, by name →
    commit in Conventional Commits, small and ordered where the task has separable units, each with
    a body that says why, what changed and how it was proven → push when asked or when the
    maintainer-local instructions say so. The version is not part of this commit. Then run
    `cargo xtask version` and follow `AGENTS.md` §8.3.
14. **Clean up** (§13.3). Remove the worktrees whose work is landed and the scratch copies nothing
    still needs; a worktree with uncommitted work waits for the maintainer.
15. **Final report** (format below), with the log.

## Final report format

```
# Niobe Task — <task>
Status: DONE | NOT DONE (blockers open)
Tier: Trivial | Standard | Major  (trigger: <PRINCIPLES.md §8.1 trigger, or "none">)
Scope delivered: <bullets>
Gate: fmt <clean> | clippy <clean> | test <was N → now M passing / K ignored> | layering <ok> | headers <ok> | audit <ok> | version <ok> | size <bytes>
Quality: CRAP <worst changed function> | dryer <pairs ≥ 0.8> | mutator <killed/survived/uncovered>  (or "tools not installed")
Landed: <commit SHA>   (DONE requires this line, or the reason it is withheld)

## Implementation
<diff summary; what was subtracted; RED → GREEN → REFACTOR log per criterion; for a bug, the
root-cause sentence; any premise attacked>

## Self-review (lenses applied)
Architecture <ok/notes> · Fold <ok> · Figures <ok> · Hostile input <ok> · Terminal <ok/N/A> · Rust <ok> · Docs <ok>

## Self-QA
<edge cases exercised, green-washing fixed>

## Follow-ups recorded but NOT done

## Log
<each action taken and the reason for it, in order>
```

## Hard rules

- **Done only when the whole gate is green, the self-QA leaves no open critical or major
  (`PRINCIPLES.md` §10), and the work is landed** as `AGENTS.md` §7.9 and the maintainer-local
  instructions allow. Never claim done on inference; never call a green-but-uncommitted tree done
  unless the maintainer-local instructions are what hold the commit, and then say so.
- **Never** skip RED → GREEN → REFACTOR per criterion, or the phases' order: make it work, then
  make it right, then practise.
- **Never** a third fix on a premise two fixes already failed on.
- **Never** `unwrap()`/`expect()` in library or binary code; never a `_ =>` over a workspace enum.
- **Never** a figure without provenance, a `0` for an absent number, or a counter that can wrap.
- **Never** widen scope beyond what was confirmed; record discoveries as follow-ups.
- **Never** `git add -A`, `git add .` or `git stash` (`AGENTS.md` §7.9). Never open a pull request
  unless asked. Never touch the version.
- No planning identifiers anywhere in the tree or the commit (`AGENTS.md` §4).
