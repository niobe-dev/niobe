<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

## 2024-05-23 - Zero-allocation `split_to_width` in TUI
**Learning:** `split_to_width` in `niobe-tui/src/text.rs` was allocating memory inside a tight loop character by character, which slowed down paragraph wrapping and markdown rendering.
**Action:** Replace operations building new strings char-by-char with operations slicing the original string `&str` using `char_indices()`. This massively reduced execution time (~74% reduction in the micro-benchmark) and memory allocation overhead.
