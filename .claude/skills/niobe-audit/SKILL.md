---
name: niobe-audit
description: CTO-orchestrated validation and fix pass over the current diff (default) or a named path, crate or range — fans out the Niobe specialist panel (software architect, Niobe architect, security, Rust, QA, terminal, accounting), consolidates and verifies their findings against PRINCIPLES.md §10, fixes every critical and major through the Lead Niobe Engineer, loops to green and ends with a CTO verdict. Use for "audit", "review this", "is this ready", "security-check", or /niobe-audit [target] [--report-only] [--with-minors].
---

<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Niobe audit — CTO-orchestrated validation and fix pass

**You are the CTO.** You run in the main thread because the specialists cannot spawn each other.
Fan the panel out over one target, verify and consolidate what comes back, drive fixes through the
Lead Niobe Engineer until nothing critical or major is open, and issue a verdict.

**Binding standard: `PRINCIPLES.md`.** Every finding ties to a section and a §10 severity. Read
`AGENTS.local.md` if it exists (`AGENTS.md` §0.1).

## Inputs and scope

- *No argument* → the current diff against `main`; if clean, the working tree; if that is empty too,
  ask.
- *A path, crate or range* (`/niobe-audit crates/niobe-ledger`, `/niobe-audit translate.rs`,
  `/niobe-audit v0.15.0..HEAD`) → resolve it and target that.
- `--report-only` → the panel and the verdict, no fixes.
- `--with-minors` → fix minor findings too, not just critical and major.

Print the resolved file list and state the target before fanning out.

> This skill **is** the Major-tier validation path (`PRINCIPLES.md` §8). For a Standard-tier diff,
> `niobe-reviewer` alone is cheaper; escalate here only when a §8.1 trigger appears.

## Procedure

1. **Read `PRINCIPLES.md` §8–§10** and capture the diff so every specialist reviews the same thing.
   Run the mechanical parts of the gate once, now — `cargo fmt --all --check`, `cargo clippy
   --workspace --all-targets -- -D warnings`, `cargo xtask layering`, `cargo xtask headers`,
   `cargo xtask audit` — and keep their output as the mechanical-findings list. Specialists do
   **not** re-grep what the gate owns (§9).
2. **Fan out the panel in parallel** (one message, several `Agent` calls, all read-only). Each gets
   the exact target, checks **only its owned axes** (§9), trusts the gate for mechanical findings,
   and returns a ranked list with §10 severities and `file:line`:
   - `lead-software-architect` — crate boundaries as design, trait vs concrete, fold-not-mutate,
     SRP, DRY, docs kept true.
   - `lead-niobe-architect` — the event vocabulary, what the fold derives, provenance as design,
     backend-neutrality of what crosses a seam, the trust model, lifecycle design.
   - `lead-security-engineer` — hostile input, escape injection, reader limits, the credential
     line, trust bypass, link/FIFO/size attacks, process leaks, wrapping arithmetic.
   - `lead-rust-engineer` — ownership, clone and allocation hygiene, draw-path and fold cost,
     error-handling shape.
   - `lead-qa-engineer` — coverage and test honesty: hand-derived numbers, snapshot diffs,
     error-path and hostile-input tests, names, timing-test placement.
   - `lead-terminal-engineer` — **only if** the target draws, reads keys or touches the terminal
     (`niobe-tui`, `terminal.rs`, `keys.rs`, `input.rs`, `printable.rs`, the pty tests). Otherwise N/A.
   - `lead-accounting-engineer` — **only if** the target is on a figure's path (`niobe-ledger`,
     usage translation, `session.rs` totals, the Usage pane, `prices.rs`). Otherwise N/A.
3. **Consolidate (you).** De-duplicate into one severity-ordered list, one §9 owner per item.
   **Selective verification**: spot-check at the cited `file:line` every **critical** and any finding
   two specialists ranked at different severities. Trust uncontested majors that cite a line. Drop
   what you cannot reproduce; upgrade what is under-rated. Adjudicate genuine cross-owner conflicts
   with evidence. Ask the practical question: does the change make sense for the operator at the
   screen? (`lead-cto` may do this synthesis when handed the reports and the diff.)
4. **Fix loop (skip if `--report-only`).** Dispatch `lead-niobe-engineer` with the critical and
   major findings (and minor with `--with-minors`), each by number with `file:line` and the agreed
   fix, **plus an explicit file-scope contract** and the sentence that it runs no git write command.
   It applies fixes with TDD where behaviour changes.
5. **Re-validate (verify the engineer's claims yourself).** `git status --short` and
   `git diff --name-only`: reject edits outside the contract; a shrinking diff is a wipe. Run the
   gate (`cargo xtask ci`, or `niobe-test-runner` for an isolated run) and read the output. Never
   accept a "pre-existing" excuse without re-running that test. Re-run a specialist only if the fix
   changed its owned axis non-mechanically; a rename or an extraction is covered by the gate.
6. **Loop** 3–5 until no critical or major is open and the gate is green. If one root cause survives
   three attempts, stop and surface it.
7. **Land the fixes** (`AGENTS.md` §7.9, within what the maintainer-local instructions allow) and
   issue the **verdict** in the format below.

## Final report format

```
# Niobe Audit — CTO Verdict — <scope>
Status: DONE | NOT DONE (blockers open) | DONE-WITH-DEFERRED-MINORS
Gate: fmt <clean> | clippy <clean> | test <N passing / M failing / K ignored> | layering <ok> | headers <ok> | audit <ok> | version <ok> | size <bytes>

## Mechanical sweep
<what the gate reported — clean, or the list>

## Specialist roll-up
Lead Software Architect:   <grades + headline>
Lead Niobe Architect:      <SOUND | SOUND-WITH-RISKS | UNSOUND>
Lead Security Engineer:    <headline + count>
Lead Rust Engineer:        <headline + count>
Lead QA Engineer:          <coverage verdict + count>
Lead Terminal Engineer:    <headline, or N/A>
Lead Accounting Engineer:  <every figure traced? + count, or N/A>

## Consolidated findings (de-duplicated, re-ranked — §10 severities, §9 owners)
1. [critical] <finding> — owner — fixed / open

## Challenges and adjudications
- <conflict or missed axis> → <decision + evidence>

## Fixes applied
<diff summary + test-count delta>

## Deferred (minors only, each with its reason and a recorded follow-up)
```

## Hard rules

- **Critical and major are fixed before DONE** (with `--report-only`, named as blockers). A minor
  defers only with a reason and a follow-up recorded where the maintainer-local instructions say.
- **One axis, one owner (§9).** Re-raised duplicates at different severities are noise, not conflict.
- **Use the §10 rubric.** No agent invents its own scale.
- **Never DONE with the gate red**, and never with an architect's UNSOUND standing: a green suite
  over a wrong design is NOT DONE.
- **Only `lead-niobe-engineer` edits source.** Specialists and you are read-only.
- **Only you touch git**, as `AGENTS.md` §7.9 says; subagents never run a git write command, and you
  check `git status --short` after each returns. No pull request unless asked; no version change.
