<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Security

Niobe drives the official `claude` and `codex` CLIs and draws what they do. This page says what
it will never do, what a repository's config can and cannot do before you have read it, and how
to report a vulnerability. Each claim names what enforces it, so that you can check it rather
than take it on trust.

## What Niobe never does

- **It never reads a CLI's credentials.** Only the official binaries touch subscription
  credentials. Sign-in is asked of the CLIs' own status commands, never read from the files they
  keep it in. `cargo xtask layering` fails on any Rust source line that is not a comment and
  names a credentials file or a user agent.
- **It never opens a connection of its own.** No crate that speaks HTTP or TLS is in the
  dependency tree, and no source under a `src/` directory resolves an address, opens a socket or
  starts a network program. `cargo xtask layering` reads the dependency tree and the source for
  exactly that, and CI runs it on every change.
- **It sends no telemetry.** There is nothing in the binary that could: the two checks above are
  the whole of its network surface. What reaches a provider is what the CLI you started sends,
  signed in as you signed it in.
- **It restores your terminal.** On a clean quit, a panic, and SIGTERM, SIGINT, SIGQUIT and
  SIGHUP the terminal is handed back as it was. `crates/niobe-cli/tests/pty.rs` runs the binary
  on a real pty and proves each.

The layering check reads names and text, not behaviour. A name built at run time, or a program
named through a variable, would pass it; that is what review is for, and why a change that
touches a process or a path says so.

## What a repository's config can do before you trust it

A repository can commit `.niobe/config.toml`, and it arrives with the clone. Until you have read
it with `niobe trust` and agreed, it may not:

- set an environment, arguments, a settings file or a login command in front of a backend;
- say how an account is billed;
- choose the default profile or replace one of yours;
- answer a permission prompt, or allow a tool call without asking.

The file is trusted as the exact bytes you were shown, by SHA-256, and again when it changes.
A settings file or a file an argument names is trusted with it, at the contents you were shown.
The record is kept beside your own config, the one directory a repository cannot write to.
`crates/niobe-config/src/trust.rs` has the record's tests, and `crates/niobe-cli/tests/pty.rs`
the question a session opens on.

## Bytes Niobe did not write

A CLI's stream, the transcript files it keeps, a repository's config, a pasted path and the
clipboard are all read as hostile.

- Every file is read only if it is a regular file and only up to a bound; a link to `/dev/zero`
  or a FIFO is refused rather than waited on or held in memory.
- Everything drawn to the terminal passes through a text layer that shows a character that would
  act on the terminal as one that says where it was. `crates/niobe-tui/tests/escape.rs` hands
  every place content is drawn a payload of control characters and escape sequences and checks
  the bytes written.
- A CLI and every command you run with `!` start in a process group of their own, which a
  closing session ends whole.

## Reporting a vulnerability

Please report it privately, not in a public issue: use **Report a vulnerability** under this
repository's Security tab, which opens a private report only the maintainers can read.

Include the version (`niobe --version`), what you did and what happened, and the output of
`niobe doctor --json` if you can. Do not include a transcript or a stream recording without
looking at it first: it holds your conversation.

You will get an acknowledgement, a fix in a patch release where one is needed, and credit in the
release notes if you want it. Niobe is maintained by one person, so the response is not a
service level; a report of a credential read, a connection Niobe opened itself or a trust bypass
is read first.
