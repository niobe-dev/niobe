<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# The bench suite

Twenty small tasks for `niobe bench`, each one bug fixed or one function
written in a few lines of Python. A turn spends little on any of them. The
suite is not a model leaderboard. It exists so that two runs of the same
tasks can be compared: a profile against another, or a session setting
against its absence.

```sh
niobe bench bench/ --json > results.json
```

## A task

```
NN-name/
  prompt.md    what the turn is asked, after the licence comment
  files/       the repository the turn starts in
  check.sh     run with sh in the repository the turn left; exit 0 passes
  hidden.py    the tests check.sh runs, which the turn never sees
  solution/    a reference solution, laid over files/ by the suite's own test
```

`niobe bench` copies `files/` into a directory of its own under the system's
temporary directory and makes it a git repository. It runs the prompt there
as one `niobe -p` turn under the profile and `--budget` it was given, then
runs `check.sh` from this directory, not from the copy, so the agent cannot
change what judges it. The turn may edit and write files inside its copy and
nowhere else. Anything more, such as running the tests itself, needs an
`--allow` rule given to `niobe bench`, which passes it to each turn:
`--allow "Bash(python3 *)"`. A task that does not pass keeps its copy, and the
results say where.

`crates/niobe-cli/tests/bench.rs` checks that every task fails before
anything is done to it and passes once `solution/` is laid over it. A pass
therefore means the turn did the work, and a failure means it did not.

## What a run costs

Each task is a real turn on the profile's backend. On a plan, all twenty draw
on the five-hour window. The results say where the window stood after the
first turn that reported it and after the last. On a metered profile, they
carry the money the backend reported for each turn.
