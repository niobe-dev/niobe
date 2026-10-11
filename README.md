<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Niobe

A terminal coding agent that keeps you aware of what is being built and how: what the agent
did, decided and touched, what it is doing now and what it cost, on screen while it happens.
Every cost figure is either one the backend reported or labelled as an estimate; where there
is no figure, the screen shows an em dash.

Niobe drives the official `claude` CLI as a subprocess, so a session runs on whatever that CLI
is already signed in with. Niobe never reads the CLI's credential files and makes no network
calls of its own. Profiles for the `codex` CLI can be written, but that bridge does not start
the CLI yet, so a session under one has nowhere to send a prompt and says so.

## Install

On macOS (arm64, x86_64) or Linux (x86_64, arm64):

```sh
curl -fsSL https://github.com/niobe-dev/niobe/releases/latest/download/install.sh | sh
```

The script checks the archive against the SHA-256 published beside it and puts `niobe` in
`~/.local/bin`; `NIOBE_INSTALL_DIR` puts it elsewhere and `NIOBE_VERSION` picks a release. It
edits no shell profile and asks for no `sudo`.

You need the `claude` CLI installed and signed in.

## Use

Run `niobe` in a repository. With no config file it runs the `claude` on your `PATH` with that
CLI's own settings, under a profile it calls `claude`. `niobe --help` lists the commands, the
config keys and the keys the shell answers to; `niobe sessions` lists the sessions you can carry
on with `niobe --resume <id>`.

To choose how a backend is run, write a profile in `~/.config/niobe/config.toml`. A config file
replaces the implied profile, so name the one you want as the default:

```toml
default_profile = "personal"

[profiles.personal]
backend = "claude"
```

`niobe doctor` checks that the `claude` CLI is installed and signed in, the config and the
terminal, and says how to fix what is not; `niobe doctor --json` prints the same for an issue.
`niobe profiles` lists the profiles in force and, where none is written, the three lines that
make the implied one explicit.

A terminal that eats an F-key can give the bar's action another one: `[keys]` with
`mcp = "ctrl+l"` in the config, and the bar, the menus and the help draw the new key.
`niobe --help` lists the actions and the keys that can be bound.

A theme of your own is a TOML file in `~/.config/niobe/themes`: a `base` theme and the colours to
change, in hex or the sixteen names. `niobe --help` has the format; one that cannot be read is
refused at its line.

A repository can commit its own `.niobe/config.toml`. What it may put in front of a backend —
an environment, arguments, a settings file, a login command, permission rules — stays out of
force until you have read it with `niobe trust` and agreed.

## Building

```sh
cargo build --release
```

The workspace needs Rust 1.88 or later. `cargo xtask ci` runs the checks CI runs. `AGENTS.md`
describes how the code is laid out and the rules a change keeps.

## License

Apache-2.0; see `LICENSE`.
