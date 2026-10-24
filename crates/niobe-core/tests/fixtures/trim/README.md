<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) Viacheslav Shynkarenko
-->

# Recorded cargo output

`tests/trim.rs` trims these outputs. Each is what one cargo command printed,
standard output and standard error together, as an agent running it with
`2>&1` reads it. The directory each was recorded in is written as `/work`,
and the home directory as `/home/user`.

| File                | Command                            | Status | Where                                                            |
| ------------------- | ---------------------------------- | ------ | ---------------------------------------------------------------- |
| `passing_cold.txt`  | `cargo test -p niobe-core`         | 0      | this workspace, with an empty `CARGO_TARGET_DIR`                 |
| `failing.txt`       | `cargo test --no-fail-fast`        | 101    | a crate of 62 unit tests, two of which fail and one is ignored, and two test files of 40 each |
| `compile_error.txt` | `cargo build`                      | 101    | a crate whose one function adds a `&str` to a `u64`              |
| `check_cold.txt`    | `cargo check -p niobe-config`      | 0      | this workspace, with an empty `CARGO_TARGET_DIR`                 |

## The figures the test asserts

The two test runs hold 37,625 bytes between them (`wc -c`: 30,502 and
7,123). Leaving out the lines the trim leaves out, with `grep` rather than the
code under test:

```sh
for f in passing_cold failing; do
  grep -vE '^ *(Adding|Blocking|Checking|Compiling|Documenting|Downloaded|Downloading|Fresh|Locking|Removing|Unpacking|Updating) ' $f.txt |
    grep -vE '^test .* \.\.\. (ok|ignored)(, .*)?$' | wc -c
done
```

prints 1,013 and 1,558: 2,571 bytes kept, a 93.2% reduction. The other two
outputs are under a kilobyte and are left whole.
