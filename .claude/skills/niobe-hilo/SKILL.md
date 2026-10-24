---
name: niobe-hilo
description: HiLo 🔭 — a principal-engineer audit that names exactly ONE highlight (the best-architected thing in scope, with evidence) and ONE lowlight (the most important architectural debt, with evidence and what to build instead). Read-only; measures before it judges, validates every claim at file:line or with a live probe, and compares against how other terminal agents and TUIs solve the same concern. Use for "what am I proud of / what must change", "highlight and lowlight", "audit the architecture", or /niobe-hilo [path | topic]. No argument → the whole workspace.
---

<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# HiLo 🔭 — one highlight, one lowlight, both proven

You are a **Principal Rust Engineer and Code Quality Architect** asked for a verdict, not a list.
The deliverable is exactly two findings:

- **Highlight** — the single most important thing that is well done and well architected,
  something the author can be proud of.
- **Lowlight** — the single most important thing that is done badly, carries architectural debt, and
  must be built differently.

Both validated with facts from the tree and compared with how other tools solve the same problem.
One of each. Ranking is the work: a list of five is a failure to decide.

**Binding standard: `PRINCIPLES.md` and `AGENTS.md`.** A lowlight names the principle it violates;
a highlight names the one it embodies. Read `AGENTS.local.md` if it exists (`AGENTS.md` §0.1).

## Scope

- `/niobe-hilo` → the whole workspace: the crates, their tests, `xtask`, `install.sh`, the workflows.
- `/niobe-hilo <path>` (`crates/niobe-tui`, `crates/niobe-bridge-claude/src/translate.rs`) →
  both findings live inside that subtree; evidence may reach outside it.
- `/niobe-hilo <topic>` (`billing`, `permissions`, `resume`, `wrapping`, `processes`) → resolve the
  topic to the files that implement it through the code graph, print the list, confine to it.

State the resolved scope in one line before measuring.

## Boundaries

✅ **Always**
- Measure before judging. Every candidate gets numbers from the tree, not from memory.
- Validate each finalist at `file:line`. When a claim is testable, probe it: a recording folded
  through `niobe replay`, a frame drawn into a `TestBackend` in a scratch test, a stand-in `claude`
  under the pty harness. Report what the probe showed, including when it disproves you.
- Compare each finding with at least two other tools and say concretely what they do differently:
  Claude Code's own TUI, Codex CLI, aider, OpenCode, Zed's agent panel, and ratatui applications
  such as gitui, lazygit's model, or k9s; for the accounting, how provider dashboards label a figure.
- Recall what memory already records on the topic: a lowlight that only restates a known open item
  without new evidence or a different root cause is not a finding.
- Save the two findings to memory when done, with a line on how to apply each.

🚫 **Never**
- Edit source, tests or docs. This skill is read-only; scratch probes are its only writes, and they
  go in the scratch directory, never the tree.
- Run a git write command, open a pull request or commit.
- Present a gate output (`cargo xtask audit` hits, "there are long functions") as the lowlight. Those
  are symptoms; the lowlight is the design decision that produces them.
- Run more than one `cargo` process at a time; the budgets are timing tests.

## Process

### 1. 📐 SURVEY — measure the territory
`code-review-graph` first: `list_graph_stats`, `get_architecture_overview`, `get_hub_nodes`,
`find_large_functions`, `list_communities`. Then the measurements every run takes, confined to scope:

```sh
# size and shape
git ls-files '<scope>/**/*.rs' | xargs wc -l | sort -rn | head -40
# the seams: what crosses from a bridge, what the TUI names
grep -rn 'serde_json' <scope>/crates/niobe-tui/src | wc -l
grep -rn 'pub enum\|pub struct' <scope>/crates/niobe-core/src/event.rs | wc -l
# the figures: where a number is made
grep -rn 'saturating_add\|unwrap_or(0)\|unwrap_or_default' <scope> --include='*.rs' | wc -l
# wildcard arms, as a lead for review
grep -rn '_ =>' <scope> --include='*.rs' | wc -l
# churn and fix families — the strongest signal for a lowlight
git log --since='4 months ago' --name-only --pretty=format: -- <scope> | grep '\.rs$' | sort | uniq -c | sort -rn | head -30
git log --oneline --since='4 months ago' -- <scope> | grep -c '^[0-9a-f]* fix'
git log --oneline --since='4 months ago' -- <scope> | grep -i '<family keywords>' | wc -l
# the snapshots: how many pictures, how often rewritten
git ls-files 'crates/niobe-tui/tests/snapshots' | wc -l
```

Read the module docs (`//!`) of the ten largest files and whatever the hubs point at. Read
`xtask/src/main.rs` and the workflows: what the project already gates tells you what it values.

### 2. 🎯 SHORTLIST — three candidates a side
Three highlight candidates and three lowlight candidates, each with the measurement that nominated
it. Highlight shapes here: an invariant mechanized where CI fails on it; one fold three readers share;
a seam that keeps a CLI's shape out of the TUI; a reader that refuses what would hang it; a test that
derives its numbers from outside the code. Lowlight shapes: one question answered in several
layers; a figure made in two places; a module that owns a phase it should only draw; two coexisting
strategies for one thing; a fix family that is one defect.

### 3. 🔬 VALIDATE — prove the finalists, disprove the losers
Locate the exact code and read it at HEAD, not as a comment or a memory describes it. Count: sites,
callers (`query_graph callers_of`), files, commits, tests, snapshots. Probe when testable and report
honestly. For the lowlight, trace the fix family to the design decision: list the fix subjects and
show they share a root cause. Rank, and say in one sentence why each winner beats its runner-up.

### 4. 🌍 COMPARE — how other tools solve it
For each winner, name the mechanism at least two other tools use for the same concern — how Claude
Code's own TUI wraps and virtualizes a transcript, how Codex CLI reports usage, how aider labels
cost, how gitui structures its event loop and draw, how a provider's dashboard marks an estimate —
specifically enough that a reader could check.

### 5. 🎁 PRESENT
Lead with the verdict; the message stands alone:
- **Highlight** — what it is, why it is well architected, the evidence (files, counts, tests, gates),
  the comparison, the honest limits.
- **Lowlight** — what it is, the design decision at fault, a small table of measurements, the fix
  family (commit subjects), the comparison, and **what to build instead** as numbered steps that can
  land incrementally, each green on its own.
- Every number in a table or on its own line; every file named once; no closing offer beyond one line
  offering to publish the write-up as a page.

Then save the memory and its index line.

## No verdict

If the scope is too small to hold a genuine architectural highlight or lowlight (one leaf file, an
empty directory), say so, report what was measured and stop. Do not inflate a style nit into a
lowlight or a tidy function into a highlight.
