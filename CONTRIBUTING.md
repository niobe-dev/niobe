<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Contributing to Niobe

Thank you for considering a contribution. Niobe is a tool people will trust with their
subscriptions and the figures on their screen, so the bar is production quality from the first
commit. This page is the short version; the long one is `AGENTS.md` (how the code is laid out and
how to work in it) and `PRINCIPLES.md` (what good looks like, operationally).

## Getting started

You need Rust 1.88 or later and, to run a session, the `claude` CLI installed and signed in.

```sh
cargo build --release
cargo test --workspace
```

`cargo xtask ci` runs the whole gate that CI runs, in the same order, with warnings denied.

## Before you open a pull request

The gate has to be green on your machine. In order:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --release -p niobe-tui --test frame_budget
cargo test --release -p niobe-store --test replay_budget
cargo test --release -p niobe-cli --test replay_budget
cargo xtask layering
cargo xtask headers
cargo xtask audit
cargo xtask version --check
cargo xtask size
```

Report the exact pass and fail counts in the pull request. The timed tests run only in a release
build; the debug suite reports them ignored, which is expected.

## What a change keeps

The invariants in `AGENTS.md` §2 hold for every change, and breaking one is a bug even when the
suite is green:

- Only the official `claude` and `codex` binaries touch credentials. Niobe never reads their token
  files, sets a user agent or opens a connection.
- Every cost figure traces to a `usage` field the backend reported, or is labelled an estimate.
  An unknown model reads "unpriced". Where there is no number, the screen shows an em dash.
- The terminal is restored on every exit path, panics and signals included.
- The TUI names no backend-specific type. The crate graph enforces it; `cargo xtask layering` fails
  when it is broken.

## How to work

- **Test first.** Write the failing test, watch it fail for the right reason, then write the code.
  Where tests live and what each binary proves is in `AGENTS.md` §6.
- **Bugs are root-caused.** Reproduce as a failing test, trace the whole path from the bridge to
  the screen, state the cause with a `file:line`, then fix at that layer (`AGENTS.md` §6.2).
- **Snapshots are read.** After an intentional layout change, regenerate with `UPDATE_SNAPSHOTS=1`
  and read the diff of every snapshot file before accepting it.
- **Comments say why.** No banners, no planning references, no `FIXME`; `cargo xtask audit` refuses
  them. A `TODO` says what is missing and why.
- **Every file carries the SPDX header** in the form `AGENTS.md` §5 gives for its type.

## Commits

Conventional Commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`. The subject says
what changed; the body says why it is right rather than restating the diff. The commit type is what
decides the next release's version (`AGENTS.md` §8), so a `fix:` that adds a feature is a bug in the
message. A contribution never touches the version: releasing is the maintainer's own commit.

## Working with an agent

If you work with a coding agent, the repository carries skills under `.claude/skills/` and
reviewers under `.claude/agents/` that encode this page and `PRINCIPLES.md`. `niobe-task` is the
default for a feature or fix; `PRINCIPLES.md` §12 lists the rest.

## Questions

Open an issue. A question about a figure on the screen is always welcome: if a number cannot be
explained, that is a bug.
