// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Adding a standing answer to a config file.
//!
//! The file belongs to the operator: it has their profiles in it, their
//! comments and their ordering, and Niobe writes one array in it and touches
//! nothing else. So the document is not re-rendered from the parsed config —
//! that would return a file with the comments gone and the keys sorted — but
//! spliced: the rule is written into the `allow` array just before its
//! closing bracket, at the byte offset the parser gives, and every other byte
//! of the file survives — the rules already there, a narrower one a broader
//! one covers included, and the comments between them.
//!
//! Three cases, and the third is the one that refuses:
//!
//! * There is no `permissions` table: a new one is appended at the end.
//! * There is a `permissions.allow` array: the rule is added as its last
//!   element, on a line of its own where the array is written one to a line.
//! * There is a `permissions` table written in some other shape: nothing is
//!   written, and the failure says what is in the way. Guessing where the rule
//!   belongs in a file this code does not understand is how an operator's
//!   config gets broken by a program they asked to remember one thing.

use std::io::Write as _;
use std::path::Path;

use niobe_core::permission::Rule;
use toml::de::{DeTable, DeValue};

use crate::{Config, ConfigError, replace};

/// Indentation of one rule in the array Niobe writes.
const INDENT: &str = "    ";

/// Adds `rule` to the config file at `path`, creating the file and its
/// directory if this is the first rule kept there.
///
/// A rule the file's own allowlist already covers is not written again: the
/// operator answering "always" twice must not grow the file twice.
///
/// Says what the file held when the lock was taken and what it holds when it
/// is let go, both read under it: whoever decides whether the new text is
/// trusted decides about exactly these, and not about whatever a later read
/// finds once another writer has had its turn.
pub fn remember(path: &Path, rule: &Rule) -> Result<Remembered, ConfigError> {
    remember_with(path, rule, |file, bytes| file.write_all(bytes))
}

/// The config file's text either side of [`remember`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remembered {
    /// What the file held before, or `None` where there was no file.
    pub before: Option<String>,
    /// What it holds now: the text written, or the text as it was where the
    /// rule was already covered and nothing was.
    pub after: String,
}

/// [`remember`], with the write of the new text handed in so that a test can
/// make it fail partway through.
///
/// The file is read, checked and written under a lock on its directory, so
/// two sessions answering "always" at once each add their rule to what the
/// other wrote rather than to what both read; and it is replaced whole, so a
/// write that fails or a process that dies inside it leaves the file as it was.
fn remember_with(
    path: &Path,
    rule: &Rule,
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<Remembered, ConfigError> {
    let failed = |error| ConfigError::Write {
        path: path.to_path_buf(),
        error,
    };
    let target = replace::resolved(path).map_err(failed)?;
    let dir = replace::parent(&target);
    std::fs::create_dir_all(dir).map_err(failed)?;
    let _lock = replace::Lock::directory(dir).map_err(failed)?;

    let before = match crate::read::text(&target) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                error,
            });
        }
    };
    let text = before.clone().unwrap_or_default();

    // Parsed before anything is written, so a rule is never added to a file
    // that would not load afterwards.
    let config = Config::parse(&text, path)?;
    if config.allowed().includes(rule) {
        return Ok(Remembered {
            before,
            after: text,
        });
    }

    let written = splice(&text, path, &rule.to_string())?;
    replace::replace_with(&target, written.as_bytes(), write).map_err(failed)?;
    Ok(Remembered {
        before,
        after: written,
    })
}

/// The file's text with `rule` added to its `permissions.allow` array.
fn splice(text: &str, path: &Path, rule: &str) -> Result<String, ConfigError> {
    let document = DeTable::parse(text).map_err(|_| unwritable(path, 1, "is not valid TOML"))?;
    let root = document.get_ref();

    let Some((key, value)) = root.get_key_value("permissions") else {
        let mut written = text.to_owned();
        if !written.is_empty() && !written.ends_with('\n') {
            written.push('\n');
        }
        if !written.is_empty() {
            written.push('\n');
        }
        written.push_str("[permissions]\nallow = ");
        written.push_str(&array(&[rule]));
        written.push('\n');
        return Ok(written);
    };

    let DeValue::Table(table) = value.get_ref() else {
        return Err(unwritable(
            path,
            line(text, key.span().start),
            "is not a table, so Niobe cannot add a rule to it",
        ));
    };
    let Some(allow) = table.get("allow") else {
        return Err(unwritable(
            path,
            line(text, key.span().start),
            "has no `allow` array for Niobe to add a rule to; add `allow = []` to it",
        ));
    };
    let DeValue::Array(items) = allow.get_ref() else {
        return Err(unwritable(
            path,
            line(text, allow.span().start),
            "is not an array, so Niobe cannot add a rule to it",
        ));
    };

    let span = allow.span();
    let close = span.end.saturating_sub(1);
    let last_end = items.last().map(|item| item.span().end);
    let open_a_line = text[span.start..close].contains('\n')
        && text[..close]
            .rfind('\n')
            .is_some_and(|at| text[at + 1..close].trim().is_empty());

    let mut written = String::with_capacity(text.len() + rule.len() + INDENT.len() + 4);
    let mut from = 0;
    // The element before the new one needs its comma, where it has none.
    if let Some(end) = last_end
        && !comma_after(&text[end..close])
    {
        written.push_str(&text[..end]);
        written.push(',');
        from = end;
    }
    match open_a_line {
        // One to a line: the rule goes on a line of its own, above the one
        // the bracket closes on and below whatever comment ends the array.
        true => {
            let at = text[..close].rfind('\n').map_or(close, |at| at + 1);
            written.push_str(&text[from..at]);
            written.push_str(INDENT);
            written.push_str(&quoted(rule));
            written.push_str(",\n");
            written.push_str(&text[at..]);
        }
        false => {
            written.push_str(&text[from..close]);
            if last_end.is_some() {
                written.push(' ');
            }
            written.push_str(&quoted(rule));
            written.push_str(&text[close..]);
        }
    }
    Ok(written)
}

/// Whether `between`, the text from the end of an array's last element to its
/// closing bracket, holds the comma that separates another element from it,
/// outside any comment.
fn comma_after(between: &str) -> bool {
    between
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .any(|code| code.contains(','))
}

/// A `permissions` table Niobe will not write into.
fn unwritable(path: &Path, line: usize, message: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_path_buf(),
        line,
        key: Some("permissions".to_owned()),
        message: message.to_owned(),
    }
}

/// The rules as a TOML array, one to a line so that a file with a dozen of
/// them still reads down the page and a diff of it shows the rule that was
/// added rather than the whole line.
fn array(rules: &[&str]) -> String {
    let mut out = String::from("[\n");
    for rule in rules {
        out.push_str(INDENT);
        out.push_str(&quoted(rule));
        out.push_str(",\n");
    }
    out.push(']');
    out
}

/// A rule as a TOML basic string.
///
/// A target is whatever a call acts on, so it can hold a quote, a backslash or
/// a newline; every one of those is escaped here, because a rule that was
/// written unescaped would be a config file the next session cannot read.
fn quoted(rule: &str) -> String {
    let mut out = String::with_capacity(rule.len() + 2);
    out.push('"');
    for c in rule.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The line, counted from 1, a byte offset falls on.
fn line(text: &str, at: usize) -> usize {
    text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("/configs/repo/config.toml")
    }

    /// The text `rule` splices into `text`, or the failure as the operator
    /// reads it.
    fn spliced(text: &str, rule: &str) -> Result<String, String> {
        splice(text, &path(), rule).map_err(|error| error.to_string())
    }

    #[test]
    fn a_file_with_no_permissions_table_gets_one_at_the_end() {
        let written = spliced(
            "# my profiles\n[profiles.max]\nbackend = \"claude\"\n",
            "Bash(cargo test)",
        )
        .expect("the file takes a new table");

        assert_eq!(
            written,
            "# my profiles\n[profiles.max]\nbackend = \"claude\"\n\n\
             [permissions]\nallow = [\n    \"Bash(cargo test)\",\n]\n"
        );
    }

    #[test]
    fn an_empty_file_is_written_without_a_blank_line_at_the_top() {
        let written = spliced("", "Read").expect("an empty file takes a table");

        assert_eq!(written, "[permissions]\nallow = [\n    \"Read\",\n]\n");
    }

    #[test]
    fn everything_but_the_array_survives_being_written() {
        let before = "\
default_profile = \"max\"  # the one I use

[permissions]
# added by hand
allow = [\"Read\"]

[profiles.max]
backend = \"claude\"
";
        let written = spliced(before, "Bash(cargo test)").expect("the array is spliced");

        assert_eq!(
            written,
            "\
default_profile = \"max\"  # the one I use

[permissions]
# added by hand
allow = [\"Read\", \"Bash(cargo test)\"]

[profiles.max]
backend = \"claude\"
"
        );
    }

    /// The rules already written, the narrower one a broader one covers
    /// included, and the comments between them are the operator's: the new
    /// rule is one line added, and nothing else moves.
    #[test]
    fn a_rule_is_added_to_an_array_without_touching_what_is_in_it() {
        let before = "\
[permissions]
allow = [
 \"Bash\", # everything
 \"Bash(cargo test)\",
 # \"Write\", disabled
]
";
        let written = spliced(before, "Read").expect("the array is spliced");

        assert_eq!(
            written,
            "\
[permissions]
allow = [
 \"Bash\", # everything
 \"Bash(cargo test)\",
 # \"Write\", disabled
    \"Read\",
]
"
        );
    }

    #[test]
    fn a_last_element_with_no_comma_is_given_one() {
        assert_eq!(
            spliced("[permissions]\nallow = [\n  \"Bash\" # all\n]\n", "Read").expect("spliced"),
            "[permissions]\nallow = [\n  \"Bash\", # all\n    \"Read\",\n]\n"
        );
        assert_eq!(
            spliced("[permissions]\nallow = []\n", "Read").expect("spliced"),
            "[permissions]\nallow = [\"Read\"]\n"
        );
    }

    #[test]
    fn a_rule_that_would_not_read_back_is_escaped() {
        let written =
            spliced("", r#"Bash(echo "one" \ two)"#).expect("the rule is written escaped");

        let config = Config::parse(&written, &path()).expect("what was written reads back");
        assert_eq!(
            config.allowed().rules()[0],
            Rule::targeted("Bash", r#"echo "one" \ two"#)
        );
    }

    #[test]
    fn a_permissions_table_niobe_does_not_understand_is_left_alone() {
        let said = spliced("[permissions]\n", "Read").expect_err("there is no array");
        assert!(said.contains("no `allow` array"), "{said}");
        assert!(said.contains("config.toml:1"), "{said}");

        let said = spliced("permissions = \"none\"\n", "Read").expect_err("not a table");
        assert!(said.contains("is not a table"), "{said}");
    }

    #[test]
    fn an_inline_permissions_table_is_spliced_like_any_other() {
        let written = spliced("permissions = { allow = [] }\n", "Read")
            .expect("an inline table holds an array too");

        let config = Config::parse(&written, &path()).expect("what was written reads back");
        assert_eq!(config.allowed().rules().len(), 1);
    }

    #[test]
    fn a_rule_already_in_the_file_is_not_written_again() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[permissions]\nallow = [\"Read\"]\n").expect("the file is written");

        remember(&file, &Rule::tool("Read")).expect("nothing to do");

        assert_eq!(
            std::fs::read_to_string(&file).expect("the file is still there"),
            "[permissions]\nallow = [\"Read\"]\n"
        );
    }

    #[test]
    fn a_first_rule_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join(".niobe").join("config.toml");

        remember(&file, &Rule::targeted("Bash", "cargo test")).expect("the file is created");
        remember(&file, &Rule::tool("Read")).expect("the file is added to");

        let config = Config::read(&file)
            .expect("what was written reads back")
            .expect("the file is there");
        assert_eq!(
            config.allowed().rules(),
            [Rule::targeted("Bash", "cargo test"), Rule::tool("Read")]
        );
    }

    #[test]
    fn a_config_that_does_not_load_is_not_written_to() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[profiles.max]\nbackend = \"nothing\"\n").expect("written");

        let said = remember(&file, &Rule::tool("Read"))
            .expect_err("the file does not load")
            .to_string();

        assert!(said.contains("is not a backend"), "{said}");
        assert_eq!(
            std::fs::read_to_string(&file).expect("the file is still there"),
            "[profiles.max]\nbackend = \"nothing\"\n",
            "a rule was added to a config that will not load"
        );
    }

    #[test]
    fn a_write_that_fails_halfway_leaves_the_file_as_it_was() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let file = dir.path().join("config.toml");
        let before = "# mine\n[profiles.max]\nbackend = \"claude\"\n";
        std::fs::write(&file, before).expect("the file is written");

        let failed = remember_with(&file, &Rule::tool("Read"), |out, bytes| {
            out.write_all(&bytes[..bytes.len() / 2])?;
            Err(std::io::Error::other("the disk is full"))
        })
        .expect_err("the write failed");

        assert!(failed.to_string().contains("the disk is full"), "{failed}");
        assert_eq!(
            std::fs::read_to_string(&file).expect("the file is still there"),
            before,
            "a failed write changed the operator's config"
        );
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("the directory lists")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(left, ["config.toml"], "a failed write left a file behind");
    }

    #[test]
    fn two_sessions_answering_always_at_once_both_keep_their_rule() {
        for round in 0..100 {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let file = dir.path().join("config.toml");
            std::fs::write(&file, "[profiles.max]\nbackend = \"claude\"\n").expect("written");
            let start = std::sync::Barrier::new(2);

            std::thread::scope(|scope| {
                for rule in [Rule::targeted("Bash", "cargo test"), Rule::tool("Read")] {
                    let (file, start) = (&file, &start);
                    scope.spawn(move || {
                        start.wait();
                        remember(file, &rule).expect("the rule is written");
                    });
                }
            });

            let config = Config::read(&file)
                .expect("what was written reads back")
                .expect("the file is there");
            let rules = config.allowed().rules();
            assert!(
                rules.contains(&Rule::targeted("Bash", "cargo test"))
                    && rules.contains(&Rule::tool("Read")),
                "round {round} lost a rule: {rules:?}"
            );
        }
    }
}
