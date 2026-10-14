<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

## 2025-02-14 - Markdown styled run wrapping allocations & span merging
**Learning:** In `niobe-tui/src/markdown.rs`, `words()` was allocating owned `String`s for every word fragment across styled runs, and `wrap_runs` pushed separate `Span`s for every word and space.
**Action:** Borrow `&str` slices in `words` instead of allocating owned `String`s, and merge adjacent text fragments sharing the same `Style` into single `Span`s when building lines.
