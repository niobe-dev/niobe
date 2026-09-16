<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Recorded logs

## `session-200.jsonl`

200 events, one JSON object per line, in the wire form `Event` serializes to.
The session — an agent adding etag support to a catalog fetcher — is
**synthetic**: it was written by hand rather than recorded from a backend. It
exists so that the fold in `niobe_core::session` can be asserted against
numbers this crate did not produce.

It is also read by `niobe-store`'s tests, which record it into a session store
and fold what comes back, and by `niobe-cli`'s, which replay and resume it
through the binary. Those tests compare the stored copy with the log rather
than with fixed numbers, except for the totals `tests/cli.rs` checks in the
printed summary (`122,554` tokens, `≥$2.47`, 4 of 14 records without a cost).

It deliberately carries the awkward cases a real recording has:

- usage records with `cost_usd: null` — the backend reported tokens and no
  money, so the session's cost is a floor, not a measurement;
- a second model (`sonnet-5`) partway through;
- a `tool_call_end` whose `tool_call_start` the producer dropped;
- a denied permission, a failed tool call, a cancelled and a failed sub-agent;
- sub-agents still running, and assistant deltas not yet closed by a message,
  when the recording is cut;
- a fatal error at the end.

### Re-deriving the expected totals

The numbers asserted in `tests/replay.rs` come from `jq` over this file, not
from `SessionState`. To check them, or to regenerate them after editing the
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
  decisions: (map(select(.type=="decision")) | length),
  checkpoints: (map(select(.type=="checkpoint")) | length),
  agents_spawned: (map(select(.type=="agent_spawn")) | length),
  agents_completed: (map(select(.type=="agent_exit" and .outcome=="completed")) | length),
  agents_failed: (map(select(.type=="agent_exit" and .outcome=="failed")) | length),
  agents_cancelled: (map(select(.type=="agent_exit" and .outcome=="cancelled")) | length),
  errors: (map(select(.type=="error")) | length),
  user_messages: (map(select(.type=="user_message")) | length),
  assistant_messages: (map(select(.type=="assistant_message")) | length),
  agents: ([ .[] | select(.type=="agent_spawn" or .type=="agent_exit") ]
    | reduce .[] as $e ({running: [], peak: 0};
        if $e.type=="agent_spawn"
        then .running = (.running + [$e.id] | unique)
             | .peak = ([.peak, (.running | length)] | max)
        else .running = (.running - [$e.id]) end)
    | {peak: .peak, still_running: (.running | length)})
}' session-200.jsonl
```

`reported_cost_usd` prints as `2.4699999999999998`: floating-point addition,
which is why the test compares it against `2.47` with a tolerance rather than
for equality.
