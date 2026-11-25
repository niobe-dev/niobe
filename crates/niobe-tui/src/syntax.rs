// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Syntax highlighting: a line of code read as the tokens a reader tells
//! apart — keywords, types, strings, comments — in the language it is
//! written in.
//!
//! A highlighter reads one line at a time and carries what is still open at
//! the end of one — a block comment, a string that runs on — into the next,
//! as [`State`]. A view that draws a few lines of a long file, or an editor
//! that has to read again from a line that changed, keeps the state each
//! line started in and picks up there, rather than reading from the top.
//!
//! The highlighter is a lexer driven by a table per language, not a grammar:
//! it knows how each language writes a comment, a string, a number and its
//! keywords, and names a word by what it is or by what is next to it — the
//! word after `fn` is a function, a word followed by `(` is called. It does
//! not parse, so it reads a string's interpolations as string and does not
//! tell a regular expression's slashes from division. What it costs is a
//! pass over the line, with no dependency and nothing loaded first, which is
//! what a shell that lays its transcript out again on every resize can pay.
//!
//! What a token is drawn in is the theme's ([`paint`]): this module names
//! kinds and byte ranges, and nothing here knows of a colour or a terminal.

mod language;
mod lexer;
mod markup;
pub(crate) mod paint;
mod spec;

use std::ops::Range;

pub use language::Language;

/// What a token of code is, as a theme colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A word the language reserves: `fn`, `if`, `return`; a markup tag's name.
    Keyword,
    /// A type's name: a built-in one, one being declared, or a capitalised
    /// word in a language that capitalises its types.
    Type,
    /// A function's name, where it is declared or called; a macro.
    Function,
    /// A key a value is written under — a JSON or YAML key, a TOML key, a
    /// markup attribute — and a variable a sigil marks, as `$HOME`.
    Property,
    /// A string, a character literal, a template.
    String,
    /// A number, in any base, with its suffix.
    Number,
    /// A value the language names: `true`, `None`, `nil`, `self`, a symbol.
    Constant,
    /// A comment.
    Comment,
    /// An annotation on what follows it: a decorator, a Rust attribute, a
    /// preprocessor directive, a lifetime.
    Attribute,
    /// A line a diff adds.
    Inserted,
    /// A line a diff removes.
    Deleted,
}

/// A run of a line that is one kind of thing: its bytes and what they are.
///
/// The range is of the line the highlighter was handed, and falls on its
/// character boundaries. What no token covers is plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Where in the line the token is.
    pub range: Range<usize>,
    /// What it is.
    pub kind: TokenKind,
}

/// What a line left open for the next: the comment or string a line ended
/// inside, or nothing.
///
/// Two lines that start in equal states are read the same way, which is what
/// an editor that keeps each line's state relies on to stop reading again
/// once a change has stopped making a difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct State {
    within: Within,
}

/// What a line ended inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum Within {
    /// Nothing: the next line starts as code.
    #[default]
    Code,
    /// The language's block comment numbered `which`, nested `depth` deep.
    Comment { which: u8, depth: u8 },
    /// The string the language writes with its quote numbered `which`.
    Quote { which: u8 },
    /// A raw string closed by a quote and `hashes` hashes, as Rust's
    /// `r#"…"#` is.
    Raw { hashes: u8 },
    /// A markup tag, between its name and its `>`.
    Tag,
}

/// The longest line a highlighter reads, in bytes. What comes after it in a
/// longer line is plain, and the next line starts as code: a line that long
/// is a minified bundle or a data dump, which no reader reads by its
/// keywords, and reading it would cost a frame.
pub const MAX_LINE_BYTES: usize = 4_096;

/// Reads the lines of one piece of code in a language, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlighter {
    language: Language,
    state: State,
}

impl Highlighter {
    /// A highlighter at the top of a piece of code in `language`.
    pub fn new(language: Language) -> Self {
        Self::resume(language, State::default())
    }

    /// A highlighter picking up at a line that starts in `state`, as an
    /// earlier highlighter's [`Highlighter::state`] gave it.
    pub fn resume(language: Language, state: State) -> Self {
        Self { language, state }
    }

    /// The state the next line starts in.
    pub fn state(&self) -> State {
        self.state
    }

    /// The language it reads.
    pub fn language(&self) -> Language {
        self.language
    }

    /// The tokens of `line` — one line, without its terminator — in order and
    /// apart, and the state the next line starts in kept for it.
    pub fn line(&mut self, line: &str) -> Vec<Token> {
        let (read, capped) = match line.len() > MAX_LINE_BYTES {
            true => (cut_at_boundary(line, MAX_LINE_BYTES), true),
            false => (line, false),
        };
        let (tokens, state) = match self.language.spec() {
            spec::Grammar::Code(spec) => lexer::line(spec, read, self.state),
            spec::Grammar::Markup => markup::line(read, self.state),
            spec::Grammar::Diff => (markup::diff_line(read), State::default()),
        };
        self.state = match capped {
            true => State::default(),
            false => state,
        };
        tokens
    }
}

/// The longest start of `line` no longer than `bytes` that ends on a
/// character boundary.
fn cut_at_boundary(line: &str, bytes: usize) -> &str {
    let mut end = bytes.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each token of `line` read in `language` from the top, as its text and
    /// kind.
    pub(super) fn read(language: Language, line: &str) -> Vec<(String, TokenKind)> {
        let mut highlighter = Highlighter::new(language);
        shown(line, &highlighter.line(line))
    }

    pub(super) fn shown(line: &str, tokens: &[Token]) -> Vec<(String, TokenKind)> {
        tokens
            .iter()
            .map(|token| (line[token.range.clone()].to_owned(), token.kind))
            .collect()
    }

    fn kind_of(language: Language, line: &str, text: &str) -> Option<TokenKind> {
        read(language, line)
            .into_iter()
            .find(|(said, _)| said == text)
            .map(|(_, kind)| kind)
    }

    #[test]
    fn a_rust_line_names_its_keywords_function_types_and_literals() {
        let line = r#"pub fn parse(input: &str) -> Option<u32> { let n = 42; "done" } // ok"#;
        let tokens = read(Language::Rust, line);
        let expect = [
            ("pub", TokenKind::Keyword),
            ("fn", TokenKind::Keyword),
            ("parse", TokenKind::Function),
            ("str", TokenKind::Type),
            ("Option", TokenKind::Type),
            ("u32", TokenKind::Type),
            ("let", TokenKind::Keyword),
            ("42", TokenKind::Number),
            ("\"done\"", TokenKind::String),
            ("// ok", TokenKind::Comment),
        ];
        for (text, kind) in expect {
            assert!(
                tokens.contains(&(text.to_owned(), kind)),
                "{text} as {kind:?} in {tokens:?}"
            );
        }
        assert_eq!(kind_of(Language::Rust, line, "input"), None);
        assert_eq!(kind_of(Language::Rust, line, "n"), None);
    }

    #[test]
    fn tokens_are_in_order_apart_and_on_character_boundaries() {
        let line = "let café = \"naïve\"; /* ünï */ 3.5e-2";
        let mut highlighter = Highlighter::new(Language::Rust);
        let tokens = highlighter.line(line);
        let mut end = 0;
        for token in &tokens {
            assert!(token.range.start >= end, "{tokens:?}");
            assert!(token.range.start < token.range.end, "{tokens:?}");
            assert!(line.is_char_boundary(token.range.start));
            assert!(line.is_char_boundary(token.range.end));
            end = token.range.end;
        }
        assert!(end <= line.len());
    }

    #[test]
    fn a_block_comment_left_open_carries_to_the_next_line_and_closes_there() {
        let mut highlighter = Highlighter::new(Language::C);
        let first = highlighter.line("int x; /* starts");
        assert_eq!(
            shown("int x; /* starts", &first).last(),
            Some(&("/* starts".to_owned(), TokenKind::Comment))
        );
        assert_ne!(highlighter.state(), State::default());
        let second = highlighter.line("still */ int y;");
        assert_eq!(
            shown("still */ int y;", &second),
            vec![
                ("still */".to_owned(), TokenKind::Comment),
                ("int".to_owned(), TokenKind::Type),
            ]
        );
        assert_eq!(highlighter.state(), State::default());
    }

    #[test]
    fn a_nested_comment_closes_only_at_its_own_end_where_the_language_nests_them() {
        let mut rust = Highlighter::new(Language::Rust);
        rust.line("/* outer /* inner */ still outer");
        assert_ne!(rust.state(), State::default());
        let mut c = Highlighter::new(Language::C);
        c.line("/* outer /* inner */ code");
        assert_eq!(c.state(), State::default());
    }

    #[test]
    fn a_python_docstring_runs_over_lines_and_a_plain_string_ends_with_its_line() {
        let mut python = Highlighter::new(Language::Python);
        python.line("def f():");
        python.line("    \"\"\"Starts here");
        let inside = python.line("    and goes on \"quoted\" \"\"\" + x");
        assert_eq!(
            shown("    and goes on \"quoted\" \"\"\" + x", &inside),
            vec![(
                "    and goes on \"quoted\" \"\"\"".to_owned(),
                TokenKind::String
            )]
        );
        let mut js = Highlighter::new(Language::JavaScript);
        js.line("const s = \"never closed");
        assert_eq!(js.state(), State::default());
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let line = r#"x = "a \" b" + y"#;
        assert_eq!(
            kind_of(Language::JavaScript, line, r#""a \" b""#),
            Some(TokenKind::String)
        );
    }

    #[test]
    fn rust_tells_a_char_from_a_lifetime_and_reads_raw_strings_and_macros() {
        let line = r##"fn f<'a>(s: &'a str) -> char { println!("{}", r#"raw "q""#); 'x' }"##;
        let tokens = read(Language::Rust, line);
        assert!(tokens.contains(&("'a".to_owned(), TokenKind::Attribute)));
        assert!(tokens.contains(&("'x'".to_owned(), TokenKind::String)));
        assert!(tokens.contains(&("r#\"raw \"q\"\"#".to_owned(), TokenKind::String)));
        assert!(tokens.contains(&("println!".to_owned(), TokenKind::Function)));
        assert_eq!(
            kind_of(
                Language::Rust,
                "#[derive(Debug)] struct S;",
                "#[derive(Debug)]"
            ),
            Some(TokenKind::Attribute)
        );
        assert_eq!(
            kind_of(Language::Rust, "struct Name;", "Name"),
            Some(TokenKind::Type)
        );
    }

    #[test]
    fn a_range_is_not_read_as_a_decimal_point() {
        assert_eq!(
            read(Language::Rust, "0..10"),
            vec![
                ("0".to_owned(), TokenKind::Number),
                ("10".to_owned(), TokenKind::Number)
            ]
        );
        assert_eq!(
            read(Language::Python, "x = 0x1F + 1_000.5e-3"),
            vec![
                ("0x1F".to_owned(), TokenKind::Number),
                ("1_000.5e-3".to_owned(), TokenKind::Number)
            ]
        );
    }

    #[test]
    fn json_keys_are_properties_and_values_strings() {
        let line = r#"{"name": "niobe", "count": 3, "ok": true, "none": null}"#;
        let tokens = read(Language::Json, line);
        assert!(tokens.contains(&("\"name\"".to_owned(), TokenKind::Property)));
        assert!(tokens.contains(&("\"niobe\"".to_owned(), TokenKind::String)));
        assert!(tokens.contains(&("3".to_owned(), TokenKind::Number)));
        assert!(tokens.contains(&("true".to_owned(), TokenKind::Constant)));
        assert!(tokens.contains(&("null".to_owned(), TokenKind::Constant)));
    }

    #[test]
    fn toml_reads_its_tables_keys_and_comments() {
        assert_eq!(
            read(Language::Toml, "[workspace.package] # the version"),
            vec![
                ("[workspace.package]".to_owned(), TokenKind::Type),
                ("# the version".to_owned(), TokenKind::Comment)
            ]
        );
        assert_eq!(
            read(Language::Toml, "rust-version = \"1.88\""),
            vec![
                ("rust-version".to_owned(), TokenKind::Property),
                ("\"1.88\"".to_owned(), TokenKind::String)
            ]
        );
    }

    #[test]
    fn yaml_keys_are_properties_and_a_hash_inside_a_word_is_no_comment() {
        let tokens = read(Language::Yaml, "  runs-on: ubuntu-latest#x # why");
        assert_eq!(
            tokens,
            vec![
                ("runs-on".to_owned(), TokenKind::Property),
                ("# why".to_owned(), TokenKind::Comment)
            ]
        );
    }

    #[test]
    fn a_shell_line_reads_variables_strings_and_comments() {
        let line = r#"export PATH="$HOME/bin:$PATH" # set ${X}"#;
        let tokens = read(Language::Shell, line);
        assert!(tokens.contains(&("export".to_owned(), TokenKind::Keyword)));
        assert!(tokens.contains(&("\"$HOME/bin:$PATH\"".to_owned(), TokenKind::String)));
        assert!(tokens.contains(&("# set ${X}".to_owned(), TokenKind::Comment)));
        assert_eq!(
            kind_of(Language::Shell, "echo ${NAME} $1", "${NAME}"),
            Some(TokenKind::Property)
        );
    }

    #[test]
    fn sql_keywords_are_read_in_any_case() {
        let tokens = read(
            Language::Sql,
            "select id FROM users where name = 'x' -- why",
        );
        assert!(tokens.contains(&("select".to_owned(), TokenKind::Keyword)));
        assert!(tokens.contains(&("FROM".to_owned(), TokenKind::Keyword)));
        assert!(tokens.contains(&("'x'".to_owned(), TokenKind::String)));
        assert!(tokens.contains(&("-- why".to_owned(), TokenKind::Comment)));
    }

    #[test]
    fn c_preprocessor_lines_and_python_decorators_are_attributes() {
        assert_eq!(
            kind_of(Language::C, "  #include <stdio.h>", "#include"),
            Some(TokenKind::Attribute)
        );
        assert_eq!(
            kind_of(Language::Python, "@functools.cache", "@functools.cache"),
            Some(TokenKind::Attribute)
        );
        assert_eq!(
            kind_of(Language::Python, "class Shell(Base):", "Shell"),
            Some(TokenKind::Type)
        );
        assert_eq!(
            kind_of(Language::Python, "def run(self):", "run"),
            Some(TokenKind::Function)
        );
        assert_eq!(
            kind_of(Language::Python, "def run(self):", "self"),
            Some(TokenKind::Constant)
        );
    }

    #[test]
    fn miri_reads_as_its_compiler_tokenises_it() {
        let lines = [
            "use system.collections.list",
            "@must_use",
            "fn times(v [float]) [float]",
            "    let n = v.length() // count",
            "    var out = List<float>()",
            "    unless n > 0x1F and not done",
            "        print(f\"{n} rows\", re\"a+\"i)",
            "    return None",
        ];
        let mut miri = Highlighter::new(Language::Miri);
        let tokens: Vec<_> = lines
            .iter()
            .flat_map(|line| shown(line, &miri.line(line)))
            .collect();
        let expect = [
            ("use", TokenKind::Keyword),
            ("system", TokenKind::Keyword),
            ("@must_use", TokenKind::Attribute),
            ("fn", TokenKind::Keyword),
            ("times", TokenKind::Function),
            ("float", TokenKind::Type),
            ("let", TokenKind::Keyword),
            ("length", TokenKind::Function),
            ("// count", TokenKind::Comment),
            ("var", TokenKind::Keyword),
            ("List", TokenKind::Type),
            ("unless", TokenKind::Keyword),
            ("0x1F", TokenKind::Number),
            ("and", TokenKind::Keyword),
            ("not", TokenKind::Keyword),
            ("f\"{n} rows\"", TokenKind::String),
            ("re\"a+\"", TokenKind::String),
            ("return", TokenKind::Keyword),
            ("None", TokenKind::Constant),
        ];
        for (text, kind) in expect {
            assert!(
                tokens.contains(&(text.to_owned(), kind)),
                "{text} as {kind:?} in {tokens:?}"
            );
        }
        assert_eq!(
            kind_of(Language::Miri, "    var out = List<float>()", "out"),
            None,
            "a word Miri reserves in one position only is a name everywhere else"
        );
        let mut nested = Highlighter::new(Language::Miri);
        nested.line("/* a /* nested */ comment");
        assert_ne!(nested.state(), State::default(), "Miri nests its comments");
    }

    #[test]
    fn html_reads_tags_attributes_values_comments_and_entities() {
        let line = r#"<a href="/x" data-id='1'>Tom &amp; Jerry</a><!-- note"#;
        let mut html = Highlighter::new(Language::Html);
        let tokens = shown(line, &html.line(line));
        let expect = [
            ("a", TokenKind::Keyword),
            ("href", TokenKind::Property),
            ("\"/x\"", TokenKind::String),
            ("data-id", TokenKind::Property),
            ("'1'", TokenKind::String),
            ("&amp;", TokenKind::Constant),
            ("<!-- note", TokenKind::Comment),
        ];
        for (text, kind) in expect {
            assert!(
                tokens.contains(&(text.to_owned(), kind)),
                "{text} as {kind:?} in {tokens:?}"
            );
        }
        assert_eq!(
            kind_of(Language::Html, "Tom &amp; Jerry", "Tom"),
            None,
            "text between tags is plain"
        );
        assert_ne!(html.state(), State::default());
        let closing = html.line("end --> <br");
        assert_eq!(
            shown("end --> <br", &closing).first(),
            Some(&("end -->".to_owned(), TokenKind::Comment))
        );
        let attribute = html.line("  class=\"wide\">");
        assert_eq!(
            shown("  class=\"wide\">", &attribute).first(),
            Some(&("class".to_owned(), TokenKind::Property)),
            "a tag left open carries its attributes to the next line"
        );
    }

    #[test]
    fn a_diff_reads_its_added_removed_and_hunk_lines() {
        let lines = [
            "--- a/x.rs",
            "+++ b/x.rs",
            "@@ -1,2 +1,2 @@",
            "-old",
            "+new",
            " same",
        ];
        let kinds: Vec<_> = lines
            .iter()
            .map(|line| read(Language::Diff, line).first().map(|(_, kind)| *kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                Some(TokenKind::Keyword),
                Some(TokenKind::Keyword),
                Some(TokenKind::Attribute),
                Some(TokenKind::Deleted),
                Some(TokenKind::Inserted),
                None
            ]
        );
    }

    #[test]
    fn a_line_past_the_bound_is_read_only_as_far_as_it_and_the_next_starts_as_code() {
        let long = format!("/* {}", "é".repeat(MAX_LINE_BYTES));
        let mut highlighter = Highlighter::new(Language::Rust);
        let tokens = highlighter.line(&long);
        assert!(tokens.iter().all(|token| token.range.end <= MAX_LINE_BYTES));
        assert!(
            tokens
                .iter()
                .all(|token| long.is_char_boundary(token.range.end))
        );
        assert_eq!(highlighter.state(), State::default());
    }

    #[test]
    fn a_state_another_language_left_is_read_as_code() {
        let mut html = Highlighter::new(Language::Html);
        html.line("<a href=\"x\"");
        let mut rust = Highlighter::resume(Language::Rust, html.state());
        assert_eq!(
            shown("fn f", &rust.line("fn f")),
            vec![
                ("fn".to_owned(), TokenKind::Keyword),
                ("f".to_owned(), TokenKind::Function)
            ]
        );
        let mut python = Highlighter::new(Language::Python);
        python.line("x = \"\"\"open");
        let mut html = Highlighter::resume(Language::Html, python.state());
        assert_eq!(
            shown("<b>", &html.line("<b>")),
            vec![("b".to_owned(), TokenKind::Keyword)]
        );
        assert_eq!(html.state(), State::default());
    }

    #[test]
    fn every_language_reads_hostile_bytes_without_panicking_and_on_boundaries() {
        let lines = [
            "",
            "\"",
            "'",
            "`",
            "/*",
            "*/",
            "<!--",
            "r#\"",
            "#[",
            "@",
            "$",
            "${",
            "0x",
            "1e+",
            ".5",
            "\\",
            "'\\",
            "<",
            "</",
            "&",
            "&#",
            "\u{1b}[31m red \u{7}",
            "é'é\"é`é",
            "日本語 = \"値\" // 注",
            "\u{200b}\u{202e}abc",
            "--[[",
            "{-",
            "<#",
            "\"\"\"",
            "'''",
            "[[",
            "#",
            "#!",
            ":",
            "::sym",
        ];
        for language in Language::ALL {
            let mut highlighter = Highlighter::new(language);
            for line in lines {
                for token in highlighter.line(line) {
                    assert!(
                        token.range.start < token.range.end
                            && line.is_char_boundary(token.range.start)
                            && line.is_char_boundary(token.range.end)
                            && token.range.end <= line.len(),
                        "{language:?} read {line:?} as {token:?}"
                    );
                }
            }
        }
    }
}
