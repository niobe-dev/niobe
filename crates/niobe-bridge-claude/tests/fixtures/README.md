<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Recorded streams

## `stream-json.jsonl`

A two-turn `claude` session as the CLI prints it under

```sh
claude -p --input-format stream-json --output-format stream-json --verbose \
  --include-partial-messages --permission-mode default --model <m>
```

The **shapes** are those of two sessions recorded from Claude Code 2.1.275 on
18 September 2026 — every key here appeared in that recording, and no key of
that recording that the bridge reads is missing. The **content** is written by
hand: the recording was of a real machine, and a fixture is read by everyone.

It exists so that the translation can be asserted against numbers this crate
did not produce, and it carries the cases that are easy to get wrong:

- **`assistant` messages repeat their `usage`**, and that `usage` is a
  mid-stream snapshot: `msg_1` reports 2 output tokens for a message that
  finished at 40. Folding it instead of the `message_delta` is the bug this
  fixture exists to fail.
- **`result` totals are cumulative for the session**: turn two's `modelUsage`
  and `total_cost_usd` include turn one's, so a bridge that adds them up
  reports the session's cost twice.
- a model that never produces a message of its own (`claude-haiku-4-5`, which
  the CLI uses for its own small jobs) and whose tokens and cost therefore
  arrive only in `modelUsage`;
- a `Task` call, which is a sub-agent rather than an action;
- a call the CLI refused, which the stream reports **twice** — once as
  `system`/`permission_denied` between the call and its result, and again in
  the closing `result`'s `permission_denials`. Counting both would double every
  denial; the first is also what tells a call that was not allowed to run from
  one that broke.
- `system`/`thinking_tokens`, the CLI's running *estimate* of the thinking
  tokens of the message in flight — a guess, and already a share of the output
  tokens the turn is billed for, so it is counted nowhere;
- a context compaction, a `system` status line, a message type this bridge
  does not know, and a line that is not JSON at all.

### The arithmetic the tests assert

Every `message_delta` in a turn adds up to that turn's `result.usage`:

| Turn | in | out | cache read | cache write |
| ---- | -- | --- | ---------- | ----------- |
| 1    | 3 + 2 + 1 = **6** | 40 + 25 + 12 = **77** | 1000 + 1100 + 1150 = **3250** | 100 + 50 + 10 = **160** |
| 2    | 1 + 1 + 1 = **3** | 20 + 10 + 8 = **38** | 1200 + 1250 + 0 = **2450** | 10 + 10 + 5 = **25** |

`modelUsage` for `claude-sonnet-5` is the running total of both: 9 in, 115 out,
5700 cache read, 185 cache write. Its cost runs 0.05 then 0.09, so turn two
reports $0.04; `claude-haiku-4-5` costs $0.001, reported in full on turn one
and unchanged after. `total_cost_usd` is the sum of the two — 0.051, then
0.091 — which is the checksum the bridge warns about when it does not hold.

To re-derive the table from the fixture:

```sh
jq -s 'map(select(.type=="stream_event" and .event.type=="message_delta").event.usage)
       | {in: (map(.input_tokens)|add), out: (map(.output_tokens)|add),
          cache_read: (map(.cache_read_input_tokens)|add),
          cache_write: (map(.cache_creation_input_tokens)|add)}' stream-json.jsonl
```
