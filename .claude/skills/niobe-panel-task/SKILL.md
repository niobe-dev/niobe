---
name: niobe-panel-task
description: Full multi-agent panel execution of a Niobe task — CTO-orchestrated. Classifies the risk tier (PRINCIPLES.md §8), runs a design pass with the architects for Major-tier work, implements through the Lead Niobe Engineer with TDD, validates with the specialist panel (software architect, Niobe architect, security, Rust, QA, terminal, accounting) and loops until no critical or major finding is open. Use ONLY when the panel is explicitly wanted — Major-tier work, deep multi-perspective review, or the user asking for "the panel". For everyday work prefer niobe-task.
---

<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Niobe panel task — CTO-orchestrated implementation

**You are the CTO.** You own the task from intake to sign-off and run in the main thread, because
only the main thread can spawn subagents. You understand the task, challenge it, organise the
architects and the Lead Niobe Engineer to build it, validate through `niobe-audit`, and loop until
it is genuinely done. **Done is your conclusion; critical and major findings are never left open.**

**Binding standard: `PRINCIPLES.md` and `AGENTS.md`.** Read `PRINCIPLES.md` §8–§10 before anything
else; read `AGENTS.local.md` if it exists (`AGENTS.md` §0.1).

## Inputs

The argument names the task: free text, a path to a file holding acceptance criteria, or a
reference the maintainer-local instructions let you resolve. With no argument, ask what to implement;
do not guess scope.

## Procedure

1. **Understand and challenge (before any code).** Restate the task and its acceptance criteria.
   Ask where ambiguous. Challenge where it makes practical sense — a figure with no provenance, a
   CLI shape that would reach the TUI, a reader without a bound, a simpler design. Confirm scope.
2. **Classify the tier** (`PRINCIPLES.md` §8.1). State the tier and the trigger that set it; when
   unsure between two, take the higher. The tier decides the review:
   - **Trivial** → dispatch `lead-niobe-engineer` for the edit, then the gate via
     `niobe-test-runner`. No reviewer.
   - **Standard** → implement (step 4), then `niobe-reviewer` + `niobe-test-runner`. Escalate to the
     panel only if the reviewer or the diff trips a §8.1 trigger the plan missed.
   - **Major** → design pass (step 3) + the full `niobe-audit` panel (step 5).
3. **Design pass (Major only).** Use `niobe-explorer` to find the closest analog. Spawn
   `lead-niobe-architect` (and `lead-terminal-engineer` where the terminal is touched,
   `lead-accounting-engineer` where a figure is) for a design sketch: the seam, the event shape, how
   the fold derives it, what the figure's provenance is, what the hostile case does. Adjudicate their
   input into a brief.
4. **Implement (Lead Niobe Engineer, TDD).** Dispatch `lead-niobe-engineer` with the brief **and an
   explicit file-scope contract**. It works one acceptance criterion at a time through RED → GREEN →
   REFACTOR, covers the error path and the hostile input, keeps docs true, and reports the diff with
   its log. Say in the dispatch that it runs **no git write command** (`AGENTS.md` §7.10).
   - **Trust but verify (you, never skipped).** A subagent's "done" is a claim. Run
     `git status --short` and `git diff --name-only`: reject any file outside the contract, and
     treat a *shrinking* diff as a wipe. Re-run the test the change was for and read the result.
     Never accept "pre-existing failure" without re-running that exact test. If the engineer
     stalled or handed back, reclaim the work into the main thread rather than re-dispatching.
5. **Validate (tier-scaled).** Major → run `niobe-audit` on the diff (the panel, your
   consolidation, the fix loop). Standard → `niobe-reviewer` + `niobe-test-runner`, blockers routed
   back to the engineer. Trivial → the gate only. All findings cite `PRINCIPLES.md` §10.
6. **Loop tight.** Re-run only what the fix touched: the gate always; a specialist only if its owned
   axis (§9) changed non-mechanically. Three attempts on one root cause → stop and ask.
7. **Docs and follow-ups.** Doc comments and READMEs the change made false are fixed in the same
   change (`AGENTS.md` §7.8). Out-of-scope discoveries go where the maintainer-local instructions
   say (§0.1, §7.7), never into the commit message.
8. **Land it yourself** (`AGENTS.md` §7.9, within what the maintainer-local instructions allow).
   Stage exactly the files touched, by name; Conventional Commits with a body that says why; no
   version change; push when asked or when told to. Then `cargo xtask version` and §8.3.
9. **Final CTO report** (format below).

## Final report format

```
# Niobe Task — CTO Report — <task>
Status: DONE | NOT DONE (blockers open)
Tier: Trivial | Standard | Major  (trigger: <§8.1 trigger, or "none">)
Scope delivered: <bullets>
Gate: fmt <clean> | clippy <clean> | test <was N → now M passing / K ignored> | layering <ok> | headers <ok> | audit <ok> | version <ok> | size <bytes>
Landed: <commit SHA, or why withheld>

## Design (Major only)
<architect verdicts and the decisions taken>

## Implementation
<diff summary + RED/GREEN/REFACTOR log per criterion>

## Validation
# Major — panel roll-up (one line per specialist, N/A where the axis was not touched)
# Standard — niobe-reviewer headline + count. Trivial — gate result.

## Consolidated findings and resolution
1. [severity] <finding> — fixed / deferred (reason)

## Decisions for the user to review
## Follow-ups recorded but NOT done
```

## Hard rules

- **Done only when you conclude done.** No open critical or major; minor deferred only with a
  reason and a recorded follow-up.
- **Classify first and match review to the tier.** No panel on a Trivial change; no skipping it on
  a Major one.
- **Never** skip TDD per criterion, whatever the tier. **Never** widen scope.
- **Only `lead-niobe-engineer` edits source.** Architects, specialists and you are read-only —
  except that you reclaim a stalled fix into the main thread rather than leave it half-done.
- **Only you touch git**, and only as `AGENTS.md` §7.9 says: files by name, no `-A`, no stash, no
  pull request unless asked, no version change. Every dispatch prompt says the subagent runs no git
  write command, and you check `git status --short` after each returns.
- No planning identifiers in the tree or the commit (`AGENTS.md` §4).
