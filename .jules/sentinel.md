<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Sentinel's Journal

## 2025-03-08 - TOML Spanned Array Slicing Bounds Check
**Vulnerability:** TOML spanned byte ranges in `niobe-config/src/write.rs` (`splice_into_array`) were sliced directly into `&str` without validating character boundaries (`is_char_boundary`) or range bounds (`start <= close`), causing thread panics on malformed config spans or comments containing multi-byte UTF-8 sequences.
**Learning:** AST span ranges from manual TOML walking provide raw byte offsets that may point inside multi-byte UTF-8 character sequences or produce empty/inverted ranges when array tokens are malformed or missing expected delimiters.
**Prevention:** Always validate slice ranges with `safe_slice` (checking `start <= end`, `end <= text.len()`, and `text.is_char_boundary`) before slicing `&str` in config write/splice operations, returning a typed `ConfigError` on failure.

## 2025-03-08 - Safe String Slicing in Bridge Transcript and Memory Parsing
**Vulnerability:** In `niobe-bridge-claude`, string slicing in `tag()` (`transcript.rs`) and `entries()` (`memory.rs`) used direct indexing (`text[start..start + length]` and `rest[close + 1..]`) and unchecked byte addition (`start + open.len()`), risking thread panics on malformed subprocess strings or multi-byte UTF-8 sequences.
**Learning:** `find()` and substring length calculations on string slices produce raw byte offsets. Using direct `[]` string indexing without bounds/boundary validation can trigger thread panics if delimiter calculations produce invalid character boundaries.
**Prevention:** Use `.checked_add()` for index arithmetic and `.get(...)` for slicing string references, returning `None` gracefully on invalid bounds or character boundaries.
