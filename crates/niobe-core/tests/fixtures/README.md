<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Recorded logs

## `claude-session.jsonl`

603 events, one JSON object per line, in the wire form `Event` serializes to.

It is a **recording of a real `claude` bridge session**, driven through
`niobe-bridge-claude`'s own `Session` against Claude Code 2.1.278 on
19 September 2026. Every line in it is an event the bridge
produced from what the CLI actually printed, or one the shell produces
alongside it — the prompt the operator typed, the answer they gave a
permission prompt, the model they moved the session to. None of it was written
by hand.

The CLI's raw stream was kept beside it, and this file is that stream folded
again through the bridge as it stands, with the shell's events where they were:
616 of the bridge's events compared equal to the ones produced live, and 14
per-message usage records gained the one-hour share of their cache writes,
which the live run read as none. Folded once more after the bridge learned to
pass over the CLI's background-task bookkeeping, 588 compared equal again and
the 28 entries that had called that bookkeeping unreadable were gone. The last two prompted turns of that stream,
and the two the CLI started after them, are `niobe-bridge-claude`'s
`tests/fixtures/sub-agents.jsonl`.

The session works on a small Python catalog client, in a repository scrubbed to
`/repo`: a `Read` and a `grep`, two `Edit`s that add etag support to
`catalog/fetch.py`, a test run, a `Write` the operator refused, a failing test
run, a move from `claude-sonnet-5` to `claude-haiku-4-5`, and three sub-agents —
one on its own and two in parallel — whose work, and the turns the CLI started
when each finished, run to the end of the recording. The CLI was still running
when it was cut.

**What was scrubbed, and nothing else**: the working directory to `/repo`, the
CLI's own session id to a fixed one, the directory it keeps a background task's
output in, and an interpreter path to `/usr/lib/python3.14`. The tool-call ids,
the token counts, the costs, the prompts and every tool result are as they were
recorded.

It is also read by `niobe-store`'s tests, which record it into a session store
and fold what comes back, and by `niobe-cli`'s, which replay and resume it
through the binary. Those tests compare the stored copy with the log rather
than with fixed numbers, except for the totals `tests/cli.rs` checks in the
printed summary (`562,988` tokens, `≥$0.87`, 14 of 25 records without a cost).

It carries the cases a real session has and a written one tends not to:

- **14 of the 25 usage records carry no cost.** The CLI reports tokens per
  message and money only in the closing `result`, so the session's cost is a
  floor, not a measurement.
- **A third model that produced no counted message.** The session ran on
  `claude-sonnet-5` and then `claude-haiku-4-5-20251001`, and the closing
  `modelUsage` also billed `claude-opus-5[1m]` — 24 in, 13,963 out, 169,624
  cache reads, 36,584 cache writes, $0.662657 — for the two sub-agents that
  reviewed the code. Their messages reach the stream whole but never as the
  `message_delta`s the bridge counts from, so the bill is the only place their
  tokens are.
- **Three sub-agents, each launched in the background.** Each `Agent` call
  returns at once and the agent ends later, when the CLI says so; the two
  asked for in parallel ran together, so two were running at the peak.
- **A refused call**: a `Write` the operator denied, which the CLI reports
  twice and the bridge counts once.
- **Three failed calls**: a test module that does not exist, a `grep` whose
  glob the shell refused, and a reviewer's probe that exited non-zero. A call
  that did not run changed no file.
- **No error entry.** Nothing in the session failed, and the stream held 28
  lines of Claude Code 2.1.278's background-task bookkeeping —
  `task_started`, `task_progress`, `task_updated` and
  `background_tasks_changed` — which the bridge passes over on purpose.
- **Every cache write a message reported was bought for the hour**: 40,054 of
  them. The 42,833 on the records from the closing `modelUsage` carry no split.
- **A `rate_limit_event`** three times, which on a flat-rate plan is the budget.

## `producer-gaps.jsonl`

Nine events, and **synthetic on purpose**. They are what the shared event
model defines and a recording of a healthy, current session cannot contain, so
that the fold is still held to handling them:

- a `tool_call_end` whose `tool_call_start` never arrived. A producer that
  dropped an event is not something that can be recorded from one that did not;
- a sub-agent that was **cancelled**, and one still running when the log ends.
  The Claude bridge reads a sub-agent the CLI stopped as cancelled, and every
  sub-agent in the recorded session completed;
- a `Decision` and a `Checkpoint`. Nothing produces either yet;
- an error the session **carried on past** — the one the bridge gives for a
  tool result whose call it never saw announced — and a **fatal** one. Nothing
  failed in the recorded session, so it carries neither; the driver produces
  the fatal one when the CLI leaves badly, and the recorded session's CLI was
  still running when the recording was cut.

Each of these is one line, and each line is here for the reason above it. A
variant that a backend starts producing belongs in a recording instead.

### Re-deriving the expected totals

The numbers asserted in `tests/replay.rs` come from `jq` over these files, not
from `SessionState`. To check them, or to regenerate them after replacing a
fixture:

```sh
jq -s '{
  events: length,
  input: (map(select(.type=="usage").input) | add),
  output: (map(select(.type=="usage").output) | add),
  cache_read: (map(select(.type=="usage").cache_read) | add),
  cache_write: (map(select(.type=="usage").cache_write) | add),
  cache_write_1h: (map(select(.type=="usage").cache_write_1h) | add),
  reasoning: (map(select(.type=="usage").reasoning) | add),
  usage_records: (map(select(.type=="usage")) | length),
  records_without_cost: (map(select(.type=="usage" and .cost_usd==null)) | length),
  reported_cost_usd: (map(select(.type=="usage" and .cost_usd!=null).cost_usd) | add),
  models: (map(select(.type=="usage").model) | unique),
  tool_started: (map(select(.type=="tool_call_start")) | length),
  tool_finished: (map(select(.type=="tool_call_end")) | length),
  tool_failed: (map(select(.type=="tool_call_end" and .outcome=="failed")) | length),
  tool_denied: (map(select(.type=="tool_call_end" and .outcome=="denied")) | length),
  output_bytes: (map(select(.type=="tool_call_end").bytes) | add),
  by_name: (map(select(.type=="tool_call_end").name) | group_by(.)
            | map({key: .[0], value: length}) | from_entries),
  unmatched_ends: ((map(select(.type=="tool_call_start").id) | unique) as $s
                   | map(select(.type=="tool_call_end"
                         and (.id as $i | ($s | index($i)) == null))) | length),
  perm_requests: (map(select(.type=="permission_request")) | length),
  perm_denied: (map(select(.type=="permission_response" and .decision=="deny")) | length),
  files_changed: (map(select(.type=="file_change").path) | unique | length),
  lines_added: (map(select(.type=="file_change" and .added!=null).added) | add),
  lines_removed: (map(select(.type=="file_change" and .removed!=null).removed) | add),
  decisions: (map(select(.type=="decision")) | length),
  checkpoints: (map(select(.type=="checkpoint")) | length),
  agents_spawned: (map(select(.type=="agent_spawn")) | length),
  agents_cancelled: (map(select(.type=="agent_exit" and .outcome=="cancelled")) | length),
  errors: (map(select(.type=="error")) | length),
  fatal_errors: (map(select(.type=="error" and .fatal==true)) | length),
  usage_windows: (map(select(.type=="usage_windows")) | length),
  user_messages: (map(select(.type=="user_message")) | length),
  assistant_messages: (map(select(.type=="assistant_message")) | length),
  agents: ([ .[] | select(.type=="agent_spawn" or .type=="agent_exit") ]
    | reduce .[] as $e ({running: [], peak: 0};
        if $e.type=="agent_spawn"
        then .running = (.running + [$e.id] | unique)
             | .peak = ([.peak, (.running | length)] | max)
        else .running = (.running - [$e.id]) end)
    | {peak: .peak, still_running: (.running | length)})
}' claude-session.jsonl
```

`reported_cost_usd` prints as `0.86552395`: floating-point addition, which is
why the test compares it against that figure with a tolerance rather than for
equality.

### Recording a new one

The recording was made by driving `niobe_bridge_claude::Session` with
`ask_over_stdio` on, answering each permission prompt the way the shell does
and writing every event out as it arrived. Three things have to be true of the
machine it runs on, or the recording is of that machine rather than of the CLI:

- **no `PreToolUse` hook that rewrites commands.** One that does will put its
  own tooling in the recorded `Bash` inputs and in their results.
- **`--strict-mcp-config`**, so the tool list is the CLI's own rather than
  whatever servers the operator has configured.
- **the working directory passed exactly as the CLI reports it** — its resolved
  form, or the paths the bridge makes relative stay absolute.
- **the system directories still on `PATH`**, even where the hook's binary is
  kept off it: the CLI reads its login from the operating system's credential
  store through a tool that lives there, and without it every turn comes back
  "Not logged in".

Keep what the CLI printed as well as what the bridge made of it — a `claude`
first on `PATH` that runs the real one and `tee`s its standard output is
enough. A fix to the bridge can then be carried into the recording by folding
the stream again, rather than by recording a different session.
