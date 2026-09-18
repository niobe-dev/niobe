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
- a `control_request` of subtype `can_use_tool`, which is the CLI stopping the
  turn until something answers on its standard input. The recording is of what
  the CLI printed, so the answer is not in it: folding the fixture leaves that
  prompt pending, which is what an unanswered prompt is;
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

## `edits.jsonl`

One turn that edits files, in the shapes recorded from Claude Code 2.1.277 on
18 September 2026. It is where the changes pane's per-file `+`/`−` counts are
checked, so its content is a scenario that was actually applied to files and
measured with `git`, and the fixture describes those same edits.

The session, in order: a `Read`, an `Edit` that replaces one line with three, a
`Write` that makes a file that was not there, a second `Edit` to the first
file, a `Write` the CLI refused because nothing had read the file yet, a `Read`
of that file and the `Write` that then succeeds, and an `Edit` with
`replace_all` set.

### The counts the tests assert

`git diff --numstat` on the same edits, applied to the same starting files:

| numstat | file | what the stream says |
| ------- | ---- | -------------------- |
| `4  2`  | `catalog/fetch.ts`       | **`+4 −2`** — two `Edit` calls, each carrying the text it replaced and the text it wrote |
| `3  0`  | `catalog/conditional.ts` | **`+3 −0`** — a `Write` the CLI answered with `File created successfully at:`, so there was nothing there to remove |
| `1  4`  | `notes.md`               | **`+1`, removal unstated** — a `Write` carries what the file becomes and never what it was |
| `2  2`  | `catalog/cache.ts`       | **both unstated** — `replace_all` matched twice and the call describes one occurrence |

The last two are the cases the stream cannot support, and the fold marks them
rather than filling them in. Both figures are under the measured one, never
over it: that is what makes them a floor.

The refused `Write` is in the fixture because a call that did not run changed
no file. Counting it would put `notes.md` in the change set twice and would
report a file as changed on a session where the edit never happened.

Every `message_delta` in the turn adds up to the closing `result.usage` — 15
in, 279 out, 6760 cache read, 91 cache write — by the same `jq` program above,
with `edits.jsonl` in place of `stream-json.jsonl`.

## `transcripts/`

A directory of session transcripts in the shape the CLI writes them: one
directory per working directory it has run in, one JSON Lines file per session,
named by the id the CLI's own `--resume` takes. It holds one session,
`2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42`, recorded in `/repo`.

The **shapes** are those of twenty-five transcripts recorded by Claude Code
2.1.277 on 18 September 2026 — every key here appeared in them, and no key of
theirs that the bridge reads is missing. The **content** is written by hand, for
the same reason the streams above are.

It carries what the transcript does differently from the live stream:

- **the operator's own turns**, which a live session never has to be told: a
  turn the CLI wrote for itself and marked `isMeta`, a `/clear` it expanded
  into a turn of markup, and the prompt that was actually typed. The history
  keeps all but the marked one; the session list names the first the CLI did
  not write;
- **one API response written out over two records** — `msg_1`, once for its
  thinking and once for its text and its call — with the *same* `usage` stamped
  on both. Unlike the live stream's per-record snapshots these repeated figures
  are final, so a session counted from its messages counts one per `message.id`:
  counting both would bill the message twice;
- **no `result` line, and the bill under another name.** What the session cost
  is the closing `cost-state` record, whose `modelUsage` is the same running
  total per model that closes a live turn — but keyed by the id the session was
  *billed* under. The messages here name `claude-sonnet-5` and the bill says
  `claude-sonnet-5[1m]`, which is what every one of the twenty-five recorded
  transcripts did: not one message named the id its session was billed under,
  and the two are different rates for the same work. A session with a
  `cost-state` is therefore counted from it alone. Adding the per-message
  counts to it is the bug this fixture exists to fail — it would report every
  token of the session twice;
- a model that never produced a message of its own (`claude-haiku-4-5`) and
  whose tokens and cost therefore arrive only in `modelUsage`;
- the CLI's own furniture — `mode`, `permission-mode`, `ai-title`,
  `last-prompt`, `attachment`, `file-history-snapshot`, `system` — which is
  read for nothing and must not be reported as records Niobe cannot read;
- a record type this bridge does not know, and a line that is not JSON at all.

### The arithmetic the tests assert

The session was closed, so it is counted from `cost-state` and from nothing
else:

| Billed as | in | out | cache read | cache write | cost |
| --------- | -- | --- | ---------- | ----------- | ---- |
| `claude-haiku-4-5`    | 900 | 10 | 0    | 0   | $0.001 |
| `claude-sonnet-5[1m]` | 5   | 65 | 2100 | 150 | $0.05  |
| **session**           | **905** | **75** | **2100** | **150** | **$0.051** |

`totalCostUSD` is 0.051, which is the sum of the two — the checksum the bridge
warns about when it does not hold. Nothing here is a floor: the CLI recorded
what the session cost, so every record carries its own money.

The `claude-sonnet-5[1m]` row is exactly what the two messages add up to, which
is what makes the double count visible: a fold that counted the messages as
well would report 910 in, 140 out, 4200 cache read and 300 cache write. The
per-message table is

| Message | in | out | cache read | cache write |
| ------- | -- | --- | ---------- | ----------- |
| `msg_1` | 3  | 40  | 1000       | 100 (all bought for the hour) |
| `msg_2` | 2  | 25  | 1100       | 50 |
| **sum** | **5** | **65** | **2100** | **150** |

and it is what a session the CLI has not closed — one with no `cost-state` —
is counted from instead, as tokens with no money against them.

To re-derive that table from the fixture — the fixture holds a line that is not
JSON, so the program reads the file as text and drops what will not parse, the
way the fold does:

```sh
jq -R -s 'split("\n") | map(fromjson? // empty)
          | map(select(.type=="assistant").message) | unique_by(.id) | map(.usage)
          | {in: (map(.input_tokens)|add), out: (map(.output_tokens)|add),
             cache_read: (map(.cache_read_input_tokens)|add),
             cache_write: (map(.cache_creation_input_tokens)|add)}' \
  transcripts/2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42.jsonl
```
