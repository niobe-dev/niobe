<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

## 2025-02-14 - Markdown styled run wrapping allocations & span merging
**Learning:** In `niobe-tui/src/markdown.rs`, `words()` was allocating owned `String`s for every word fragment across styled runs, and `wrap_runs` pushed separate `Span`s for every word and space.
**Action:** Borrow `&str` slices in `words` instead of allocating owned `String`s, and merge adjacent text fragments sharing the same `Style` into single `Span`s when building lines.

## 2026-10-24 - `split_to_width` slices instead of building strings
**Learning:** `split_to_width` in `niobe-tui/src/text.rs` built each piece by pushing grapheme after grapheme into a new `String`, on the path every wrapped paragraph and code line takes.
**Action:** Return `&str` slices of the input, tracking the byte offset as the graphemes go by, and let a caller that keeps a piece own it.

## 2026-10-25 - In-place String padding with `pad_owned`
**Learning:** `text::pad` in `niobe-tui/src/text.rs` allocated a new `String` for every call, even when formatting ephemeral strings returned from truncation or formatting helpers.
**Action:** Added `text::pad_owned` to reuse existing `String` allocations in-place when padding row components in TUI rendering paths.
