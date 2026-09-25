<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Recorded streams

## Which release each recording is of, and what that is for

Nothing here is written against a published schema: every shape in this
directory is one the `claude` CLI was recorded printing, and the release it was
recorded from is part of what the recording says. Each recording names its own
release the way its carrier does — `claude_code_version` on a live
`system`/`init`, which is the key the shipped CLI's init schema uses, and
`version` on a transcript record — and `conformance::RECORDED` in the crate
names the set.

Two things are checked against that, in `tests/conformance.rs`:

- **Every shape in every recording is one the bridge was written for.** The
  test builds the inventory — message types, `system` subtypes, stream events,
  content blocks, deltas — and compares it with a list that is checked in. This
  is the half no other test can do: a message type the bridge does not know
  announces itself at runtime, but a content block it has no place for, or a
  count that moved to a different stream event, arrives in silence. Here a new
  shape is a red test naming it.
- **The installed CLI is a release these recordings cover**, by `major.minor`.
  A patch bump is not held against the operator: the twenty-eight 2.1.x
  releases whose transcripts were on the machine this was written on carry the
  same record types and the same message shapes. The check is skipped, out
  loud, where no `claude` is installed — which is every CI runner.

A session driving a release outside that set says so once, in the timeline,
and goes on. So does an imported transcript written by one.

**After upgrading `claude`**: record a session with the flags below, replace or
add the recording, run `cargo test -p niobe-bridge-claude`, read what the shape
inventory says changed, decide what the bridge does with each new shape, and
only then add the release to `conformance::RECORDED`.

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
- a `Task` call, which is a sub-agent rather than an action. `Task` is the
  alias the CLI accepts for its sub-agent tool; the name models call it by is
  `Agent`, which `sub-agents.jsonl` holds as recorded;
- a `control_request` of subtype `can_use_tool`, which is the CLI stopping the
  turn until something answers on its standard input. The recording is of what
  the CLI printed, so the answer is not in it: folding the fixture leaves that
  prompt pending, which is what an unanswered prompt is;
- a call the CLI refused, which the stream reports **twice** — once as
  `system`/`permission_denied` between the call and its result, and again in
  the closing `result`'s `permission_denials`. Counting both would double every
  denial; the first is also what tells a call that was not allowed to run from
  one that broke.
- **cache writes bought for two different lifetimes.** The CLI reports the
  split per message in `usage.cache_creation`, and the provider bills the hour
  at twice the input rate where five minutes costs 1.25×. `msg_2`'s 50 writes
  were bought for five minutes and every other message's for the hour, so a
  bridge that reads only `cache_creation_input_tokens` prices this recording's
  writes 30.5% below what they cost. What the two readings come to is checked
  against a hand computation in the binary's `tests/cache_lifetime.rs`, which
  is the only crate that may name the bridge and the price table at once;
- a `rate_limit_event`, which is how much of the plan's five-hour and seven-day
  windows is gone. On a flat-rate plan that is the budget, and it is the one
  figure in the stream that is a level rather than a total, each report
  replacing the last. The CLI reads it from an API response's headers and
  emits it after that response, only when the level has moved a whole point
  (or the window's status or reset changed) — so a process can report it
  once, several times, or not at all;
- `system`/`thinking_tokens`, the CLI's running *estimate* of the thinking
  tokens of the message in flight — a guess, and already a share of the output
  tokens the turn is billed for, so it is counted nowhere;
- a context compaction, a `system` status line, a message type this bridge
  does not know, and a line that is not JSON at all.

### The arithmetic the tests assert

Every `message_delta` in a turn adds up to that turn's `result.usage`:

| Turn | in | out | cache read | cache write | of which for an hour |
| ---- | -- | --- | ---------- | ----------- | -------------------- |
| 1    | 3 + 2 + 1 = **6** | 40 + 25 + 12 = **77** | 1000 + 1100 + 1150 = **3250** | 100 + 50 + 10 = **160** | 100 + 0 + 10 = **110** |
| 2    | 1 + 1 + 1 = **3** | 20 + 10 + 8 = **38** | 1200 + 1250 + 0 = **2450** | 10 + 10 + 5 = **25** | 10 + 10 + 5 = **25** |

`modelUsage` for `claude-sonnet-5` is the running total of both: 9 in, 115 out,
5700 cache read, 185 cache write — 135 of those writes bought for the hour and
50 for five minutes. Its cost runs 0.05 then 0.09, so turn two reports $0.04;
`claude-haiku-4-5` costs $0.001, reported in full on turn one and unchanged
after. `total_cost_usd` is the sum of the two — 0.051, then 0.091 — which is
the checksum the bridge warns about when it does not hold.

Because each of those figures is what the CLI has billed under its model less
what it had already billed, it covers every message the model produced up to
that point. The bridge marks it as settling them, so the nine records this
recording folds to leave nothing owed for: the session reads `$0.09`, the CLI's
own figure, and not `≥$0.09`. Cut the recording short of its first `result` and
the fold says instead which tokens no cost covers — that is the shape the cost
pane values while a turn is still running.

At the rates the ledger bundles for Claude Sonnet 5 from 2026-06-30 — $2.00
input, $10.00 output, $0.20 cache read, $2.50 cache write, $4.00 cache write
for an hour, each per million tokens — the six messages come to

    9×2.00 + 115×10.00 + 5700×0.20 + 50×2.50 + 135×4.00 = 2973 millionths

or $0.002973, against $0.0027705 for the same tokens with every write read as
a five-minute one. The difference is $0.0002025, which is 30.5% of what the
writes cost.

To re-derive the table from the fixture — the fixture holds a line that is not
JSON, so the program reads the file as text and drops what will not parse, the
way the fold does:

```sh
jq -R -s 'split("\n") | map(fromjson? // empty)
          | map(select(.type=="stream_event" and .event.type=="message_delta").event.usage)
          | {in: (map(.input_tokens)|add), out: (map(.output_tokens)|add),
             cache_read: (map(.cache_read_input_tokens)|add),
             cache_write: (map(.cache_creation_input_tokens)|add),
             cache_write_1h: (map(.cache_creation.ephemeral_1h_input_tokens)|add)}' \
  stream-json.jsonl
```

### Where a cache write's lifetime comes from, and what happens without one

`cache_creation_input_tokens` is every cache write of a message; the
`cache_creation` object beside it says how many of them were bought for an
hour and how many for five minutes. The bridge reads the one-hour count and
leaves the rest of the total to be priced as five-minute writes, which is
right because the two lifetimes always add up to the total (below). A message
whose `cache_creation` is missing altogether is recorded as **no one-hour
writes** — not as writes of an unknown lifetime, and not as one-hour ones — so
it prices at the five-minute rate. That is what `wire.rs` pins in
`cache_writes_without_a_reported_split_are_no_one_hour_writes`, and it is the
one reading here that is a convention rather than a measurement — it is
written down because a CLI that stopped reporting the split would otherwise
make every write quietly cheaper with nothing on screen to say so.

Measured on the 1,017 transcripts on the machine this was written on, over
27 Claude Code 2.1.x releases up to 2.1.277 and 174,140 messages that carried
usage (19 September 2026): every one of them carried `cache_creation`, and on
every one of them `cache_creation_input_tokens` was exactly the two lifetimes
added together — so the no-breakdown case above has not been observed, and the
two keys never disagree with the total. The lifetimes themselves are close to
evenly split: 520,535,496 cache-write tokens, 263,468,094 of them bought for
the hour and 257,067,402 for five minutes. No single message mixed the two,
which is why one is hand-built in `wire.rs` rather than recorded — the field
allows it and the arithmetic has to hold for it.

**On the live stream the split is not always beside the total.** Claude Code
2.1.278's `message_delta` — the line the bridge folds usage from — carries
`cache_creation_input_tokens` with no `cache_creation` beside it; the split is
on each entry of `usage.iterations`, one per API request the message took. The
bridge takes the split beside the total where there is one and sums the
iterations' where there is not. Read only the first way, every write in
`sub-agents.jsonl` — every one of them bought for the hour — would price as a
five-minute write.

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

The last two are the cases the calls' arguments cannot support. The CLI also
reports each file tool's own diff beside its result, as `tool_use_result`'s
`structuredPatch` (see `stdio-answers.jsonl`), and that settles both; this
fixture leaves those reports out, so it is where a stream without them is
checked. The fold marks the two figures rather than filling them in. Both are
under the measured one, never over it: that is what makes them a floor.

The refused `Write` is in the fixture because a call that did not run changed
no file. Counting it would put `notes.md` in the change set twice and would
report a file as changed on a session where the edit never happened.

Every `message_delta` in the turn adds up to the closing `result.usage` — 15
in, 279 out, 6760 cache read, 91 cache write — by the same `jq` program above,
with `edits.jsonl` in place of `stream-json.jsonl`.

## `stdio-answers.jsonl`

One turn with two permission prompts, recorded from Claude Code 2.1.278 on
19 September 2026 through this crate's own `Session`, spawned with
`--permission-prompt-tool stdio` and answered from Niobe's shell. The lines are
the CLI's own. The only edits: paths rewritten to `/repo`, `system`/`init` cut
down to the keys that say what ran, the `result`'s `subagent_stats` dropped,
and the stream events, the `Read` the model did first and the `system`
status lines left out, because nothing here is about them.

The turn asked for an `Edit` and then a `Write`. The `Edit` was allowed and the
file was changed. The `Write` was refused and the file was never created.
`tests/answers.rs` replays this through a stand-in `claude` that stops at each
`control_request` until an answer arrives, as the CLI does, and keeps the
answers so they can be checked.

What the recording settles:

- **A call refused over the control channel gets no `system`/`permission_denied`.**
  The only signs of it are the tool result, an error that carries the refusal's
  `message` word for word, and the closing `result`'s `permission_denials`. So
  the driver has to tell the translator about the refusal before that tool
  result arrives. Without that, the call reads as a tool that broke.
- **The tool result for that call carries `tool_result_meta`**:
  `[{"id": …, "non_execution_kind": "permission-rule"}]`. The CLI's own
  transcript of the same session does not keep it, so a session read back
  from disk could not rely on it. The bridge does not read it.
- **The `control_request` carries `display_name` and `permission_suggestions`**
  (here, a switch to `acceptEdits` for the session). The bridge reads neither.
- **The `Edit`'s result carries the CLI's own diff of the file** in
  `tool_use_result`: `structuredPatch` is one hunk, `oldStart 1`, `oldLines 2`,
  `newStart 1`, `newLines 2`, with the lines `" alpha"`, `"-beta"`, `"+gamma"`
  — three lines of context where the file has them, as `git diff` keeps. That
  is the hunk `tests/stream.rs` asserts the change carries. The CLI's own
  transcripts of 2.1.27x–2.1.281 carry the same report under `toolUseResult`,
  on every `Edit` and on a `Write` over a file that was there (`type:
  "update"`); a `Write` that created its file reports `type: "create"`, its
  `content` and an empty `structuredPatch`.
- **The approval went back with the arguments in `updatedInput` and the call
  ran.** Separately, from the same release: an approval with no
  `updatedInput`, and one with an empty `updatedInput`, each let a `Write` run
  as the model asked for it. The field is not what makes a call go ahead.

## `shell.jsonl`

One turn with two shell commands, recorded from Claude Code 2.1.281 on
24 September 2026 through Niobe's shell, with `--permission-prompt-tool stdio`.
The model was asked to `touch a.txt` and then `touch b.txt`; the first was
allowed and ran, the second was refused with words in place of a choice. The
lines are the CLI's own. The only edits: paths rewritten to `/repo`,
`system`/`init` cut down to the keys that say what ran, the `result`'s
`subagent_stats` dropped, and the stream events and the `system` `status` and
`thinking_tokens` lines left out. The recording they were left out of carried
no shape that is not already in `tests/conformance.rs`.

What it settles is how a shell command's exit status is read:

- **A success carries no status.** The result is `(Bash completed with no
  output)`, `is_error: false`, and beside it the tool's report: `stdout`,
  `stderr`, `interrupted`, `isImage`, `noOutputExpected`. The CLI's own
  transcripts carry the same report under `toolUseResult`, and three more keys
  the bridge reads when they are there: `returnCodeInterpretation` on a
  non-zero status the CLI read as a success (`No matches found`),
  `backgroundTaskId` on a command it moved to the background, and
  `interrupted: true`. A success with none of those is a command that exited
  zero, and the bridge says so; with any of them it names no status.
- **A refusal made over the control channel reads as a failure here**, with
  the refusal's words as its reason: the recording cannot say the call was
  refused, and the driver is what tells the translator (see
  `stdio-answers.jsonl`).

A failed command's status is in `sub-agents.jsonl`: its result opens with
`Exit code 1` on a line of its own, and the rest is the command's output. The
CLI writes no report beside a sub-agent's successful commands, so those name
no status. Across 18,756 shell results in 336 of the CLI's own transcripts of
2.1.27x–2.1.281, every failed command that ran opened with `Exit code <n>` and
no successful one did.

## `tool-results/cargo-test-workspace.txt`

The whole output of a `cargo test --workspace` on this repository, as Claude
Code 2.1.282 saved it for being too large to hand the model: 88,059 bytes,
exactly the `persistedOutputSize` its report named, recorded byte for byte
(which is why it carries no header). The CLI keeps such files in its session's
`tool-results/` directory and gives the model a `<persisted-output>` block
with the first 2 KB in their place. `tests/transcript.rs` folds the call and
its result as the CLI's own transcript recorded them, pointed at this file.

It does this only for a command that succeeded. A failed command's output is
cut to about 10 KB instead, by characters and from the middle, around a line
`... [<n> characters truncated] ...`, and its report is the error text alone,
naming no file. Across the CLI's transcripts on the machine this was recorded
on, every report that named a saved file was of a success, and each of the 16
character cuts was of a failure. A failing run past that size is not
counted; `cargo-test-workspace-cut.txt` is one.

### The counts the tests assert

```sh
grep '^test result:' tool-results/cargo-test-workspace.txt |
  awk '{p+=$4; f+=$6; i+=$8; n++} END {print p, f, i, n}'
# 1040 0 0 32
```

That is 1,040 passed, none failed or ignored, in 32 suites — one per
`Running` or `Doc-tests` header, of which there are also 32.

## `cargo-test-workspace-cut.txt`

The result a failing `cargo test --workspace` on this repository came back
as from Claude Code 2.1.282, recorded byte for byte (which is why it carries
no header): `Exit code 101`, then 10,040 characters in all with a line
`... [20014 characters truncated] ...` in their middle. `tests/transcript.rs`
folds the call and this result as the CLI's own transcript recorded them.

The part after the cut is not the end of the run. It stops mid-word, inside
`tests/cli.rs`, and holds neither a failure nor the binary that failed: the CLI keeps only the first 30,000 or so characters of a failed
command's output and cuts the middle out of those. Of the 16 character cuts
in the CLI's transcripts on the machine this was recorded on, 2.1.243 to
2.1.282, the five whose kept and cut characters come to about 30,000 all end
mid-line; the eleven that come to less end where the command's output did.
So a long failing run's failures list and summaries never reach the model,
and no count can be read from what does.

What it still shows is the build finishing and the first test binary
starting, after which `cargo test` exited 101 — which a build that failed
also exits with, but only after saying a crate could not compile and before
any binary runs. That is a run that failed after its tests started, and it is
what the bridge reports, with no count.

## `long-context.jsonl`

A two-turn session on `claude-opus-5[1m]` — Opus 5 with its 1M-token window
selected — recorded from Claude Code 2.1.278 on 19 September 2026 with the
flags shown for `stream-json.jsonl`, `--model 'claude-opus-5[1m]'` and no
`--permission-mode`, so the CLI started it in `auto`. Each turn asked for
one word. The lines are the CLI's own. The only edits: `system`/`init` cut
down to the keys that say what ran, with its `cwd` rewritten to `/repo`; the
`result`'s `subagent_stats` dropped; and `system`/`status`,
`content_block_start`, `content_block_stop` and `message_stop` left out,
because the bridge reads none of them.

It exists because the session names its model two ways:

| Where | Id |
| ----- | -- |
| `system`/`init` `model` | `claude-opus-5[1m]` |
| `stream_event`/`message_start` `message.model` | `claude-opus-5` |
| `assistant` `message.model` | `claude-opus-5` |
| `result` `modelUsage` key | `claude-opus-5[1m]`, with `canonicalModel: "claude-opus-5"` |

`modelUsage` is the session's running total, and the bridge reports what each
turn added beyond what the messages already carried. Matched by id alone, the
messages under `claude-opus-5` and the bill under `claude-opus-5[1m]` have
nothing in common, and every token is counted twice. `canonicalModel` is what
says they are the same model. The same session without `--model`, on the
default Opus, names `claude-opus-5` in all four places.

### The arithmetic the tests assert

| Turn | in | out | cache read | cache write | `modelUsage` cost, running |
| ---- | -- | --- | ---------- | ----------- | -------------------------- |
| 1    | 2  | 3   | 10,118     | 10,948      | $0.114624 |
| 2    | 2  | 3   | 11,854     | 14,885      | $0.269486 |
| **session** | **4** | **6** | **21,972** | **25,833** | **$0.269486** |

The per-turn rows are the `message_delta` usage, which equals each `result`'s
`usage`; the session row is the second `result`'s `modelUsage`. A fold that
counts both the messages and the bill reports 8 in, 12 out, 43,944 cache read
and 51,666 cache write.

Each turn is one request, so its prompt — the context it filled — is its
`in + cache read + cache write`: **21,068** and **26,741**. The window is the
`contextWindow` of the `modelUsage` entry keyed `claude-opus-5[1m]`,
**1,000,000**, which the first `result` is the first to report.

## `sub-agents.jsonl`

The last two turns of a real session and the two turns the CLI started on its
own after them, recorded from Claude Code 2.1.278 on 19 September 2026
through the bridge's own `Session`, with the flags shown for `stream-json.jsonl`
and `--strict-mcp-config`. 600 lines, the CLI's own. The only edits:
`system`/`init` cut down to the keys that say what ran and which agents it
offers; the working directory rewritten to `/repo`, the CLI's session id to a
fixed one, and the directory it keeps a background task's output in to
`/tmp/claude-sessions/-repo/`. The two prompts asked, in these words, for "the
Task tool" to spawn one sub-agent and then two in parallel.

It exists because of what a sub-agent call is on this release:

- **The tool is called `Agent`.** `init` lists `Task` and no `Agent`; the
  model, told to use the Task tool, called `Agent` all three times, with
  `{subagent_type, description, prompt}`. The CLI registers `Agent` with `Task`
  as its alias and counts a call under either name as a spawn. Across the
  CLI's own transcripts on the machine this was recorded on — 477 sub-agent
  calls from 21 releases between 2.1.231 and 2.1.278 — every call was `Agent`.
- **The call returns before the agent is done.** Each `tool_result` comes back
  at once, and its `tool_use_result` says `"status": "async_launched"`. The
  agent's end is a `system`/`task_notification` naming the call in
  `tool_use_id`, with `status` one of `completed`, `failed` or `stopped` in the
  CLI's own schema for the message. Here all three completed, and the two
  reviews ran at the same time: the review of `catalog/cache.py` ended first.
- **A notification that arrives between turns starts one.** The model is told
  the agent finished and answers, so the recording holds four `result`s for two
  prompts; the first agent's notification arrived while the second prompt's
  turn was running, and started nothing. `total_cost_usd` on them runs $0.1611705, $0.27226665,
  $0.689141, $0.86552395 — the whole session's running total, as ever.
- **A sub-agent's messages reach the stream whole, and its fragments do not.**
  35 `assistant` and 16 `user` lines carry the `parent_tool_use_id` of the call
  that spawned their agent, so its tool calls are in the timeline; no
  `stream_event` does, so there is no `message_delta` to count its tokens from.
  What the agents spent arrives only in the closing `modelUsage`, under
  `claude-opus-5[1m]` for the two reviewers. The three permission prompts
  here are all a reviewer's, and each `control_request` names the agent in
  `agent_id`.
- **Every cache write was bought for the hour**, and the `message_delta`s say
  so only inside `usage.iterations` — see above.

### What each sub-agent reports about itself

Three things arrive per agent, each keyed by the `tool_use_id` of the call
that spawned it, and the tests in `tests/stream.rs` assert them as follows:

- **Its model** is on its own `assistant` messages, as `message.model`. The
  spawn does not carry one: neither the `Agent` call's input nor
  `task_started` names a model. The summariser (`quick-lookup`) ran on
  `claude-haiku-4-5-20251001` and both reviewers (`deep-reasoner`) on
  `claude-opus-5` — which `modelUsage` bills as `claude-opus-5[1m]`.
- **Its step** is `task_progress`'s `description`, which the CLI words from
  the tool call the agent last made: `Reading catalog/cache.py`, `Running
  Show fetch.py diff`. When it stops, its own answer is the `summary` of its
  `task_notification`; the first line of it is what the Activity pane shows.
- **Its token count** is `usage.total_tokens` on both. It is one number, and
  it is **the size of the agent's conversation at its latest message**, not
  what the agent was billed. The summariser's two messages report 10 + 5714 +
  0 = 5724 and 8 + 535 + 5714 = 6257 tokens of input, cache write and cache
  read; its reports read 5967 after the first and 6566 at the end — each one
  its latest message's input plus that message's output, where a sum of both
  messages would be over 12 000. The reviewers follow the same curve, and end
  at 21508 (cache) and 25861 (fetch). What any agent cost is in `modelUsage`,
  per model and not per agent, so the figure is drawn as a size and never
  priced.

To re-derive the counts, in the order the tests assert them:

```sh
jq -c 'select(.type=="system" and (.subtype=="task_progress" or .subtype=="task_notification"))
       | [.tool_use_id, .usage.total_tokens]' sub-agents.jsonl
```

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
- the CLI's own title for the session, `ai-title` — `Etag on the catalog
  response` — which is read as the session's caption;
- the CLI's own furniture — `mode`, `permission-mode`, `agent-name`, `last-prompt`, `pr-link`, `attachment`,
  `file-history-snapshot`, `bridge-session`, `system` — which is read for
  nothing and must not be reported as records Niobe cannot read. The last three
  of those were missing when the shape inventory was first taken across every
  transcript on the machine this was written on: `bridge-session` alone stood
  in fifteen thousand records, each of which had been a warning entry in front
  of the operator saying the record could not be read;
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

| Message | in | out | cache read | cache write | of which for an hour |
| ------- | -- | --- | ---------- | ----------- | -------------------- |
| `msg_1` | 3  | 40  | 1000       | 100         | 100 |
| `msg_2` | 2  | 25  | 1100       | 50          | 0   |
| **sum** | **5** | **65** | **2100** | **150**  | **100** |

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
             cache_write: (map(.cache_creation_input_tokens)|add),
             cache_write_1h: (map(.cache_creation.ephemeral_1h_input_tokens)|add)}' \
  transcripts/2f6c1e10-8f4b-4d2a-9c3e-7a5b0d1e6f42.jsonl
```
