<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Sentinel's Journal

## 2025-03-08 - TOML Spanned Array Slicing Bounds Check
**Vulnerability:** TOML spanned byte ranges in `niobe-config/src/write.rs` (`splice_into_array`) were sliced directly into `&str` without validating character boundaries (`is_char_boundary`) or range bounds (`start <= close`), causing thread panics on malformed config spans or comments containing multi-byte UTF-8 sequences.
**Learning:** AST span ranges from manual TOML walking provide raw byte offsets that may point inside multi-byte UTF-8 character sequences or produce empty/inverted ranges when array tokens are malformed or missing expected delimiters.
**Prevention:** Always validate slice ranges with `safe_slice` (checking `start <= end`, `end <= text.len()`, and `text.is_char_boundary`) before slicing `&str` in config write/splice operations, returning a typed `ConfigError` on failure.
