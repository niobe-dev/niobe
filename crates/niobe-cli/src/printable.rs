// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Text from a config, the session store or a transcript, made safe to print.
//!
//! The shell's frame is drawn by ratatui, which drops control characters, and
//! has its direction marks taken out after; what `niobe profiles` and `niobe
//! sessions` print goes to the terminal as it stands. A repository's config
//! arrives with a clone, and a stored prompt is whatever was typed or pasted,
//! so either can hold an escape sequence that clears the screen, moves the
//! cursor over the lines saying a file is not trusted, retitles the window or
//! writes the clipboard. Every line those commands print is passed through
//! [`printable`] first, so what reaches the terminal is only ever text.

use std::borrow::Cow;

/// `text` with every character that would act on the terminal rather than show
/// replaced by one that shows where it was: a C0 control by its Control
/// Pictures glyph (`␛` for an escape, `␇` for a bell), DEL by `␡`, and a C1
/// control or a character that reorders the text around it by `�`. Line
/// breaks and tabs are kept, because the lines printed are meant to have them.
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(acts) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().map(stand_in).collect())
}

/// [`printable`], with line breaks and tabs shown too (`␤`, `␉`), so that
/// `text` prints on the one line it is given.
///
/// For a value the operator is asked to agree to: a value that breaks the line
/// it is printed on can print lines of its own under it, which read as rows of
/// the listing or as niobe's own words.
pub fn on_one_line(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| matches!(c, '\n' | '\t') || acts(c)) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().map(picture).collect())
}

/// Whether `c` does something to the terminal rather than show.
fn acts(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => true,
        _ => is_direction_mark(c),
    }
}

/// The embeddings and overrides U+202A–U+202E, the isolates U+2066–U+2069 and
/// the marks U+200E, U+200F and U+061C: invisible, and they reverse what follows them.
fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{61c}')
}

/// What is printed in place of `c`.
fn stand_in(c: char) -> char {
    match acts(c) {
        true => picture(c),
        false => c,
    }
}

/// What shows where `c` was, for any `c` that is not plain text.
fn picture(c: char) -> char {
    match c {
        '\n' => '\u{2424}',
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + u32::from(c)).unwrap_or('\u{fffd}'),
        '\u{7f}' => '\u{2421}',
        c if c.is_control() || is_direction_mark(c) => '\u{fffd}',
        c => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_printed_as_it_is() {
        assert!(matches!(
            printable("max  claude  /repo/.niobe/config.toml"),
            Cow::Borrowed(_)
        ));
        assert_eq!(printable("a\tb\nc"), "a\tb\nc");
    }

    #[test]
    fn an_escape_sequence_is_printed_as_text_that_shows_where_it_was() {
        assert_eq!(printable("x\u{1b}]0;PWNED\u{7}"), "x␛]0;PWNED␇");
        assert_eq!(printable("m\u{1b}[2J"), "m␛[2J");
        assert_eq!(printable("a\rb\u{7f}"), "a␍b␡");
    }

    #[test]
    fn a_value_on_one_line_shows_its_line_breaks_and_tabs_too() {
        assert!(matches!(on_one_line("--model opus"), Cow::Borrowed(_)));
        assert_eq!(on_one_line("a\nb\tc\u{1b}[2J"), "a␤b␉c␛[2J");
    }

    #[test]
    fn a_c1_control_and_a_direction_override_are_printed_as_a_replacement() {
        assert_eq!(printable("a\u{9b}2Jb"), "a\u{fffd}2Jb");
        assert_eq!(printable("safe\u{202e}txt.exe"), "safe\u{fffd}txt.exe");
        assert_eq!(printable("T\u{61c}x"), "T\u{fffd}x");
    }
}
