---
name: niobe-task
description: Fast single-agent end-to-end execution of a Niobe task — no subagents. Classifies the change's risk tier (PRINCIPLES.md §8), implements it yourself with TDD (Red-Green-Refactor), self-reviews the diff through every specialist lens (architecture, the fold and the figures, hostile input and the terminal, Rust idiom, test honesty), runs the whole gate and lands the work. Use for everyday features and fixes, or /niobe-task <task>. For Major-tier work that wants independent eyes, use niobe-panel-task.
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

1. **Understand and challenge.** Restate the task and its acceptance criteria in your own words. Ask
   where scope, semantics or success are ambiguous. Challenge the request where it makes practical
   sense: wrong layer, a figure that would have no provenance, a seam that would put a CLI's shape
   in the TUI, a simpler design. Do not code until scope is confirmed.
2. **Classify the tier** (`PRINCIPLES.md` §8). Walk the §8.1 triggers against the confirmed scope and
   name the tier and the trigger that set it. The tier is re-confirmed after implementation, since
   the real diff may trip a trigger the plan did not.
3. **Map the change.** Place it on the path — bridge (`wire.rs` → `translate.rs`) → `Event` →
   `SessionState` fold → `App` → draw — or on the binary's edges (config, store, processes). Name
   new files against the closest analog. Run `get_impact_radius` so every consumer, test and snapshot
   you must update is known now, not one breakage at a time (`AGENTS.md` §7.5).
4. **Implement with TDD, gated (never skipped).** For a **bug**, the root-cause protocol first
   (`AGENTS.md` §6.2): a failing repro, the full path traced, the cause stated as `the bug is X at
   file:line, proven by Y` before any edit. Then, per acceptance criterion:
   - **RED** — the failing test first, where `AGENTS.md` §6 says that kind of test lives. Run it. It
     must fail for the stated reason; a test that passes at once is wrong.
   - **GREEN** — the minimum code. No speculative generality, no drive-by refactor.
   - **REFACTOR** — names, extraction, duplication, with the suite green after each step.
   - Cover the error path and the hostile input (`PRINCIPLES.md` §7.5): the notice, the refusal,
     the truncated record.
5. **Self-review through every lens** (`PRINCIPLES.md` §9), fixing as you go:
   - **Architecture** — the crate graph is honoured; what crosses a seam is a core type; nothing in
     `niobe-tui` touches a file, a process or the environment; the new outside need is a trait.
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
     (`AGENTS.md` §7.8); a new public item has a doc comment.
6. **Mechanical-transform safety.** After any `sed`, bulk rename or header insertion, re-read the
   touched files and build (`AGENTS.md` §7.4).
7. **Adversarial self-QA.** Switch hats and try to break it: the empty session, the widest line, a
   wide character at the wrap, a record cut mid-character, a `0` where an em dash belongs, a resumed
   session billed twice, a FIFO where a file was expected. Audit your own tests for green-washing: a
   snapshot regenerated unread, an expected number read off the fold, a name that describes the
   implementation rather than the behaviour. Add what is missing.
8. **Run the gate yourself and report exact counts** (`AGENTS.md` §6.1), in order; `cargo xtask ci`
   is the whole of it. Read the actual output. If any note calls a failure "pre-existing", re-run
   that test yourself before trusting it.
9. **Loop tight.** Fix, re-run what the fix touched, the whole suite before done. If the same root
   cause survives three attempts, stop and surface it; do not churn.
10. **Follow-ups.** Record what you found and did not do where the maintainer-local instructions say
    (`AGENTS.md` §0.1, §7.7), with a `TODO` at the code site that says what is missing and why.
    Never a paragraph in the commit message, never a planning identifier in the tree.
11. **Land it** (`AGENTS.md` §7.9). Gate green → stage exactly the files you touched, by name →
    commit in Conventional Commits with a body that says why → push when asked or when the
    maintainer-local instructions say so. The version is not part of this commit. Then run
    `cargo xtask version` and follow `AGENTS.md` §8.3.
12. **Final report** (format below).

## Final report format

```
# Niobe Task — <task>
Status: DONE | NOT DONE (blockers open)
Tier: Trivial | Standard | Major  (trigger: <PRINCIPLES.md §8.1 trigger, or "none">)
Scope delivered: <bullets>
Gate: fmt <clean> | clippy <clean> | test <was N → now M passing / K ignored> | layering <ok> | headers <ok> | audit <ok> | version <ok> | size <bytes>
Landed: <commit SHA>   (DONE requires this line, or the reason it is withheld)

## Implementation
<diff summary; RED → GREEN → REFACTOR log per criterion; for a bug, the root-cause sentence>

## Self-review (lenses applied)
Architecture <ok/notes> · Fold <ok> · Figures <ok> · Hostile input <ok> · Terminal <ok/N/A> · Rust <ok> · Docs <ok>

## Self-QA
<edge cases exercised, green-washing fixed>

## Follow-ups recorded but NOT done
```

## Hard rules

- **Done only when the whole gate is green, the self-QA leaves no open critical or major
  (`PRINCIPLES.md` §10), and the work is landed** as `AGENTS.md` §7.9 and the maintainer-local
  instructions allow. Never claim done on inference; never call a green-but-uncommitted tree done
  unless the maintainer-local instructions are what hold the commit, and then say so.
- **Never** skip RED → GREEN → REFACTOR per criterion.
- **Never** `unwrap()`/`expect()` in library or binary code; never a `_ =>` over a workspace enum.
- **Never** a figure without provenance, a `0` for an absent number, or a counter that can wrap.
- **Never** widen scope beyond what was confirmed; record discoveries as follow-ups.
- **Never** `git add -A`, `git add .` or `git stash` (`AGENTS.md` §7.9). Never open a pull request
  unless asked. Never touch the version.
- No planning identifiers anywhere in the tree or the commit (`AGENTS.md` §4).
