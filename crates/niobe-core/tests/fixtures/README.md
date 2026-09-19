<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Recorded logs

## `claude-session.jsonl`

313 events, one JSON object per line, in the wire form `Event` serializes to.

It is a **recording of a real `claude` bridge session**, driven through
`niobe-bridge-claude`'s own `Session` against Claude Code 2.1.278 on
19 September 2026. Every line in it is an event the bridge
produced from what the CLI actually printed, or one the shell produces
alongside it — the prompt the operator typed, the answer they gave a
permission prompt, the model they moved the session to. None of it was written
by hand.

The session works on a small Python catalog client, in a repository scrubbed to
`/repo`: a `Read` and a `grep`, an `Edit` that adds etag support to
`catalog/fetch.py`, a test run, a `Write` the operator refused, a failing test
run, a move from `claude-sonnet-5` to `claude-haiku-4-5`, and three sub-agent
calls. The recording is cut while the last turn is still going, which is what a
session someone closed looks like.

**What was scrubbed, and nothing else**: the working directory to `/repo`, the
CLI's own session id to a fixed one, the directory it spills large tool results
into, and an interpreter path to `/usr/lib/python3.14`. The tool-call ids, the
token counts, the costs, the prompts and every tool result are as they were
recorded.

It is also read by `niobe-store`'s tests, which record it into a session store
and fold what comes back, and by `niobe-cli`'s, which replay and resume it
through the binary. Those tests compare the stored copy with the log rather
than with fixed numbers, except for the totals `tests/cli.rs` checks in the
printed summary (`367,077` tokens, `≥$0.34`, 14 of 21 records without a cost).

It carries the cases a real session has and a written one tends not to:

- **14 of the 21 usage records carry no cost.** The CLI reports tokens per
  message and money only in the closing `result`, so the session's cost is a
  floor, not a measurement.
- **A third model that produced no message.** The session ran on
  `claude-sonnet-5` and then `claude-haiku-4-5-20251001`, and the closing
  `modelUsage` also billed `claude-opus-5[1m]` — 4 in, 621 out, 25,791 cache
  writes, $0.177 — under an id **no message in the session ever named**. The
  `[1m]` suffix is a different rate for the same model, so anything that
  reconciles per-message counts against `modelUsage` by model id has to treat
  the two as unrelated, which is what the bridge does.
- **A refused call**: a `Write` the operator denied, which the CLI reports
  twice and the bridge counts once.
- **Two failed calls**, one of them a tool this CLI release does not have:
  asked for `Grep`, it answered `No such tool available: Grep`. A call that did
  not run changed no file, and the changes pane shows one file, not two.
- **18 entries the bridge could not read.** Claude Code 2.1.278 emits
  `system` subtypes this version of the bridge has no place for —
  `task_started`, `task_progress`, `task_updated`, `task_notification` and
  `background_tasks_changed` — and each becomes a visible entry rather than a
  crash. They are in the recording because they are what the session produced.
- **A `rate_limit_event`** twice, which on a flat-rate plan is the budget.

### What it does not carry

Three sub-agent calls are in it as ordinary tool calls, under the name the CLI
gave them (`Agent`), and not as `AgentSpawn` / `AgentExit`: this version of the
bridge recognises the sub-agent tool only under its other name, `Task`. Nothing
here is worked around — the recording says what the bridge produced.

There are no one-hour cache writes in it either: every `cache_creation` the CLI
reported in this session was a five-minute write. The one-hour split is held in
`niobe-bridge-claude`'s own fixtures instead, where it is priced.

## `producer-gaps.jsonl`

Eight events, and **synthetic on purpose**. They are what the shared event
model defines and a recording of a healthy, current session cannot contain, so
that the fold is still held to handling them:

- a `tool_call_end` whose `tool_call_start` never arrived. A producer that
  dropped an event is not something that can be recorded from one that did not;
- a sub-agent that was **cancelled**, and one still running when the log ends.
  No backend produces `AgentOutcome::Cancelled` today;
- a `Decision` and a `Checkpoint`. Nothing produces either yet;
- a **fatal** error. The driver produces one when the CLI leaves badly; the
  recorded session's CLI was still running when the recording was cut.

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

`reported_cost_usd` prints as `0.3443623`: floating-point addition, which is
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
