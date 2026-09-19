// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The copyright header every file carries, and which files are exempt.
//!
//! A file type with neither a rule nor an exemption is reported rather than
//! skipped, so a new kind of file forces a decision instead of slipping through
//! without a header.

use std::path::Path;

/// The first line of every header.
pub const LICENSE_LINE: &str = "SPDX-License-Identifier: Apache-2.0";

/// The second line of every header.
pub const COPYRIGHT_LINE: &str = "Copyright (c) Viacheslav Shynkarenko";

/// Files that cannot carry a header: the license text itself, a file that must
/// hold nothing but an import, and a lockfile cargo rewrites.
const EXEMPT_NAMES: &[&str] = &["LICENSE", "CLAUDE.md", "Cargo.lock"];

/// How a file carries the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// The two lines, each behind this line-comment prefix.
    LineComment(&'static str),
    /// The two lines inside a leading `<!--` … `-->` block.
    HtmlComment,
    /// The two lines behind `# `, under the `#!` line, which the kernel only
    /// honours as the very first line of the file.
    Script,
    /// The format has no comments, or its bytes are compared exactly.
    Exempt,
}

/// The rule for a path relative to the workspace root, or `None` when no rule
/// covers its file type.
pub fn rule_for(path: &str) -> Option<Rule> {
    let name = path.rsplit('/').next().unwrap_or(path);

    let is_snapshot = path.contains("/tests/snapshots/") && name.ends_with(".txt");
    if EXEMPT_NAMES.contains(&name) || is_snapshot || name.ends_with(".jsonl") {
        return Some(Rule::Exempt);
    }
    if name == ".gitignore" {
        return Some(Rule::LineComment("# "));
    }

    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("rs") => Some(Rule::LineComment("// ")),
        Some("toml" | "yml" | "yaml") => Some(Rule::LineComment("# ")),
        Some("md") => Some(Rule::HtmlComment),
        Some("sh") => Some(Rule::Script),
        _ => None,
    }
}

/// Whether `contents` opens with the header `rule` asks for, followed by a
/// blank line.
pub fn has_header(rule: Rule, contents: &str) -> bool {
    let mut lines = contents.lines();
    let expected: Vec<String> = match rule {
        Rule::Exempt => return true,
        Rule::Script => match lines.next() {
            Some(shebang) if shebang.starts_with("#!") => {
                vec![format!("# {LICENSE_LINE}"), format!("# {COPYRIGHT_LINE}")]
            }
            _ => return false,
        },
        Rule::LineComment(prefix) => vec![
            format!("{prefix}{LICENSE_LINE}"),
            format!("{prefix}{COPYRIGHT_LINE}"),
        ],
        Rule::HtmlComment => vec![
            "<!--".to_owned(),
            LICENSE_LINE.to_owned(),
            COPYRIGHT_LINE.to_owned(),
            "-->".to_owned(),
        ],
    };

    expected
        .iter()
        .all(|line| lines.next() == Some(line.as_str()))
        && lines.next() == Some("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n\
                               // Copyright (c) Viacheslav Shynkarenko\n\n";

    #[test]
    fn each_file_type_gets_its_comment_syntax() {
        assert_eq!(
            rule_for("crates/niobe-core/src/lib.rs"),
            Some(Rule::LineComment("// "))
        );
        assert_eq!(rule_for("Cargo.toml"), Some(Rule::LineComment("# ")));
        assert_eq!(
            rule_for(".github/workflows/ci.yml"),
            Some(Rule::LineComment("# "))
        );
        assert_eq!(rule_for(".gitignore"), Some(Rule::LineComment("# ")));
        assert_eq!(rule_for("AGENTS.md"), Some(Rule::HtmlComment));
    }

    #[test]
    fn files_that_cannot_carry_a_header_are_exempt() {
        assert_eq!(rule_for("LICENSE"), Some(Rule::Exempt));
        assert_eq!(rule_for("CLAUDE.md"), Some(Rule::Exempt));
        assert_eq!(rule_for("Cargo.lock"), Some(Rule::Exempt));
        assert_eq!(
            rule_for("crates/niobe-core/tests/fixtures/claude-session.jsonl"),
            Some(Rule::Exempt)
        );
        assert_eq!(
            rule_for("crates/niobe-tui/tests/snapshots/empty-80x24.txt"),
            Some(Rule::Exempt)
        );
    }

    #[test]
    fn a_shell_script_carries_the_header_under_its_shebang() {
        assert_eq!(rule_for("install.sh"), Some(Rule::Script));
        let file = "#!/bin/sh\n# SPDX-License-Identifier: Apache-2.0\n\
                    # Copyright (c) Viacheslav Shynkarenko\n\nset -eu\n";
        assert!(has_header(Rule::Script, file));
        assert!(!has_header(Rule::Script, "#!/bin/sh\nset -eu\n"));
        assert!(!has_header(
            Rule::Script,
            "# SPDX-License-Identifier: Apache-2.0\n\
             # Copyright (c) Viacheslav Shynkarenko\n\nset -eu\n"
        ));
    }

    #[test]
    fn an_unknown_file_type_has_no_rule() {
        assert_eq!(rule_for("assets/logo.png"), None);
        assert_eq!(rule_for("notes.txt"), None);
    }

    #[test]
    fn a_rust_file_with_the_header_passes() {
        let file = format!("{RUST_HEADER}//! Docs.\n");
        assert!(has_header(Rule::LineComment("// "), &file));
    }

    #[test]
    fn a_missing_or_partial_header_fails() {
        assert!(!has_header(Rule::LineComment("// "), "//! Docs.\n"));
        assert!(!has_header(
            Rule::LineComment("// "),
            "// SPDX-License-Identifier: Apache-2.0\n\n//! Docs.\n"
        ));
    }

    #[test]
    fn the_header_must_be_followed_by_a_blank_line() {
        let file = RUST_HEADER.trim_end().to_owned() + "\n//! Docs.\n";
        assert!(!has_header(Rule::LineComment("// "), &file));
    }

    #[test]
    fn a_markdown_header_sits_in_an_html_comment() {
        let file = "<!--\nSPDX-License-Identifier: Apache-2.0\n\
                    Copyright (c) Viacheslav Shynkarenko\n-->\n\n# Title\n";
        assert!(has_header(Rule::HtmlComment, file));
        assert!(!has_header(Rule::HtmlComment, "# Title\n"));
    }
}
