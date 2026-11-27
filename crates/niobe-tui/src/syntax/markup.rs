// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The two languages read by something other than the lexer: markup, whose
//! words mean one thing inside a tag and nothing outside it, and a diff,
//! whose lines are what their first character says.

use super::{State, Token, TokenKind, Within};

/// The tokens of a line of HTML or XML read from `state`, and the state the
/// next line starts in: inside a comment, inside a tag, or neither.
///
/// A tag's name is a keyword, its attributes properties and their values
/// strings; a character reference is a constant. Text between tags is plain,
/// and so is a script or a style inside one — markup is read as markup.
pub(super) fn line(line: &str, state: State) -> (Vec<Token>, State) {
    let mut tokens = Vec::new();
    let mut at = 0;
    // A string, a raw string or a heredoc is code's, and markup has none: a
    // state code left is read from the top.
    let mut within = match state.within {
        Within::Comment { .. } | Within::Tag => state.within,
        Within::Code | Within::Quote { .. } | Within::Raw { .. } | Within::Heredoc { .. } => {
            Within::Code
        }
    };
    if matches!(within, Within::Comment { .. }) {
        let end = comment_end(line, 0);
        push(
            &mut tokens,
            0,
            end.unwrap_or(line.len()),
            TokenKind::Comment,
        );
        at = end.unwrap_or(line.len());
        within = match end {
            Some(_) => Within::Code,
            None => within,
        };
    }
    while let Some(rest) = line.get(at..).filter(|rest| !rest.is_empty()) {
        let c = rest.chars().next().unwrap_or(' ');
        if within == Within::Tag {
            let (end, kind, closes) = in_tag(rest);
            if let Some(kind) = kind {
                push(&mut tokens, at, at + end, kind);
            }
            at += end.max(1);
            if closes {
                within = Within::Code;
            }
            continue;
        }
        if rest.starts_with("<!--") {
            let end = comment_end(line, at + 4);
            push(
                &mut tokens,
                at,
                end.unwrap_or(line.len()),
                TokenKind::Comment,
            );
            match end {
                Some(end) => at = end,
                None => {
                    return (
                        tokens,
                        State {
                            within: Within::Comment { which: 0, depth: 1 },
                        },
                    );
                }
            }
            continue;
        }
        if c == '<' {
            let marks =
                1 + rest[1..].len() - rest[1..].trim_start_matches(is_tag_opening_mark).len();
            let name = name_end(&rest[marks..]);
            if name > 0 {
                push(
                    &mut tokens,
                    at + marks,
                    at + marks + name,
                    TokenKind::Keyword,
                );
                at += marks + name;
                within = Within::Tag;
                continue;
            }
        }
        if c == '&'
            && let Some(end) = reference_end(rest)
        {
            push(&mut tokens, at, at + end, TokenKind::Constant);
            at += end;
            continue;
        }
        at += c.len_utf8();
    }
    (tokens, State { within })
}

// Searched with a named function rather than a char array: a `[char; N]`
// pattern shares its searcher with every other one of that length in the
// crate, and under the release profile's `opt-level = "z"` one more caller
// stopped it being inlined into `text::one_line`, which a megabyte prompt
// runs through on every frame.
/// Whether `c` is a mark between a tag's `<` and its name: `</a>`,
/// `<!DOCTYPE>`, `<?xml?>`.
fn is_tag_opening_mark(c: char) -> bool {
    matches!(c, '/' | '!' | '?')
}

/// What starts `rest` inside a tag: how long it is, what kind of token —
/// none for a blank or the tag's punctuation — and whether it closes the
/// tag.
fn in_tag(rest: &str) -> (usize, Option<TokenKind>, bool) {
    if let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        let end = rest[1..].find(quote).map_or(rest.len(), |close| close + 2);
        return (end, Some(TokenKind::String), false);
    }
    let name = name_end(rest);
    if name > 0 {
        return (name, Some(TokenKind::Property), false);
    }
    let c = rest.chars().next().map_or(1, char::len_utf8);
    (c, None, rest.starts_with('>'))
}

/// Where a tag's or an attribute's name at the start of `text` ends.
fn name_end(text: &str) -> usize {
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, c)) if c.is_alphabetic() || c == '_' => {}
        _ => return 0,
    }
    chars
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')))
        .map_or(text.len(), |(at, _)| at)
}

/// Where the comment whose body goes on from `from` in `line` ends, past
/// its `-->`.
fn comment_end(line: &str, from: usize) -> Option<usize> {
    line.get(from..)?.find("-->").map(|found| from + found + 3)
}

/// Where a character reference at the start of `text` — `&amp;`, `&#38;`,
/// `&#x26;` — ends, past its `;`.
fn reference_end(text: &str) -> Option<usize> {
    let body = text.strip_prefix('&')?;
    let name = body.strip_prefix('#').unwrap_or(body);
    let length = name
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(name.len());
    (length > 0 && name[length..].starts_with(';')).then(|| text.len() - name.len() + length + 1)
}

/// Adds the token `start..end` of `kind` to `tokens`, where it holds
/// anything.
fn push(tokens: &mut Vec<Token>, start: usize, end: usize, kind: TokenKind) {
    if end > start {
        tokens.push(Token {
            range: start..end,
            kind,
        });
    }
}

/// The one token of a diff's line: the whole line, as what its first
/// characters say it is — a file's header, a hunk's, a line added or
/// removed — or none, for an unchanged line.
pub(super) fn diff_line(line: &str) -> Vec<Token> {
    let kind = if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") {
        Some(TokenKind::Keyword)
    } else if line.starts_with("@@") {
        Some(TokenKind::Attribute)
    } else if line.starts_with('+') {
        Some(TokenKind::Inserted)
    } else if line.starts_with('-') {
        Some(TokenKind::Deleted)
    } else {
        None
    };
    kind.filter(|_| !line.is_empty())
        .map(|kind| Token {
            range: 0..line.len(),
            kind,
        })
        .into_iter()
        .collect()
}
