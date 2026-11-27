// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The lexer every language but markup and a diff is read by, driven by the
//! language's [`Spec`].
//!
//! Every delimiter a table names is ASCII, so the lexer compares bytes where
//! it looks for one and steps a whole character everywhere else: a token
//! never starts or ends inside a character.

use super::spec::{Hash, Keys, Quote, Spec};
use super::{HeredocWord, State, Token, TokenKind, Within};

/// The tokens of `line` in the language `spec` describes, read from `state`,
/// and the state the next line starts in.
pub(super) fn line(spec: &Spec, line: &str, state: State) -> (Vec<Token>, State) {
    let mut lexer = Lexer {
        spec,
        line,
        at: 0,
        tokens: Vec::new(),
        within: state.within,
        previous_word: None,
        heredoc: None,
        parens: Parens::default(),
    };
    lexer.resume();
    lexer.run();
    // A heredoc's body starts on the next line, once the rest of the line
    // that opened it has been read as code; a line that ends inside a
    // string goes on as that string.
    let within = match (lexer.within, lexer.heredoc) {
        (Within::Code, Some(heredoc)) => heredoc,
        (within, _) => within,
    };
    (lexer.tokens, State { within })
}

struct Lexer<'a> {
    spec: &'a Spec,
    line: &'a str,
    at: usize,
    tokens: Vec<Token>,
    /// What the line is inside at [`Lexer::at`].
    within: Within,
    /// The word read last, where nothing but blanks came after it: what
    /// names the word after `fn` as a function.
    previous_word: Option<&'a str>,
    /// The heredoc the line opened first, whose body the next line starts:
    /// a second one on the line has its word read as a string, and its body
    /// is read as the end of the first's.
    heredoc: Option<Within>,
    /// The `((` and `))` counted so far, which say whether a `<<` shifts.
    parens: Parens,
}

/// The `((` and `))` a line holds before [`Parens::scanned`], counted as
/// the lexer moves on, so that a line of many `<<`s is not read again from
/// its start at each.
#[derive(Default)]
struct Parens {
    scanned: usize,
    opens: usize,
    closes: usize,
}

impl<'a> Lexer<'a> {
    fn rest(&self) -> &'a str {
        self.line.get(self.at..).unwrap_or_default()
    }

    /// Takes the line from [`Lexer::at`] to `end` as a token of `kind`.
    fn push(&mut self, end: usize, kind: TokenKind) {
        if end > self.at {
            self.tokens.push(Token {
                range: self.at..end,
                kind,
            });
        }
        self.at = end.max(self.at);
    }

    /// Takes what the line above left open, to where it closes or to the end
    /// of this line.
    fn resume(&mut self) {
        let (end, within, kind) = match self.within {
            Within::Code => return,
            // A tag is markup's, and code has none: a state markup left is
            // read from the top.
            Within::Tag => {
                self.within = Within::Code;
                return;
            }
            Within::Comment { which, depth } => {
                let (end, within) = self.comment_from(0, usize::from(which), depth);
                (end, within, TokenKind::Comment)
            }
            Within::Quote { which } => match self.spec.quotes.get(usize::from(which)) {
                Some(quote) => {
                    let (end, within) = quote_from(self.line, 0, *quote, which);
                    (end, within, TokenKind::String)
                }
                None => (0, Within::Code, TokenKind::String),
            },
            Within::Raw { hashes } => {
                let (end, within) = raw_from(self.line, 0, hashes);
                (end, within, TokenKind::String)
            }
            Within::Heredoc { word, tabs } => {
                let within = match closes_heredoc(self.line, word, tabs) {
                    true => Within::Code,
                    false => self.within,
                };
                (self.line.len(), within, TokenKind::String)
            }
        };
        self.push(end, kind);
        self.within = within;
    }

    fn run(&mut self) {
        while self.within == Within::Code
            && let Some(c) = self.rest().chars().next()
        {
            if c.is_whitespace() {
                self.at += c.len_utf8();
                continue;
            }
            let previous = self.previous_word.take();
            if !self.token(c, previous) {
                self.at += c.len_utf8();
            }
        }
    }

    /// Reads the token at [`Lexer::at`], which starts with `c`, the word
    /// before it being `previous`, and says whether there was one:
    /// punctuation is none.
    fn token(&mut self, c: char, previous: Option<&'a str>) -> bool {
        let rest = self.rest();
        if let Some(which) = self.block_comment_at(rest) {
            let open = self
                .spec
                .block_comments
                .get(which)
                .map_or(0, |(o, _)| o.len());
            let (end, within) = self.comment_from(self.at + open, which, 1);
            self.push(end, TokenKind::Comment);
            self.within = within;
            return true;
        }
        if self.line_comment_at(rest) {
            self.push(self.line.len(), TokenKind::Comment);
            return true;
        }
        if self.spec.sections && c == '[' && self.at_line_start() {
            self.push(self.at + section_end(rest), TokenKind::Type);
            return true;
        }
        if let Some(end) = self.hash_at(rest) {
            self.push(self.at + end, TokenKind::Attribute);
            return true;
        }
        if self.spec.heredocs
            && c == '<'
            && let Some((operator, word, heredoc)) = self.heredoc_at(rest)
        {
            self.at += operator;
            self.push(self.at + word, TokenKind::String);
            self.heredoc.get_or_insert(heredoc);
            return true;
        }
        if self.spec.lifetimes && c == '\'' {
            return self.lifetime_or_char(rest);
        }
        if let Some((prefix, which)) = self.quote_at(rest) {
            return self.string(prefix, which);
        }
        if self.spec.raw_strings
            && let Some((open, hashes)) = raw_open(rest)
        {
            let (end, within) = raw_from(self.line, self.at + open, hashes);
            self.push(end, TokenKind::String);
            self.within = within;
            return true;
        }
        if self.spec.decorators && c == '@' {
            let end = 1 + dotted_word_end(self.spec, &rest[1..]);
            if end > 1 {
                self.push(self.at + end, TokenKind::Attribute);
                return true;
            }
        }
        if self.spec.sigils.contains(c)
            && let Some(end) = sigil_end(self.spec, rest)
        {
            let kind = match self.is_constant(&rest[..end]) {
                true => TokenKind::Constant,
                false => TokenKind::Property,
            };
            self.push(self.at + end, kind);
            return true;
        }
        if self.spec.symbols && c == ':' && !self.line[..self.at].ends_with(':') {
            let end = 1 + word_end(self.spec, &rest[1..]);
            if end > 1 {
                self.push(self.at + end, TokenKind::Constant);
                return true;
            }
        }
        if c.is_ascii_digit() || (c == '.' && starts_digit(&rest[1..]) && !self.after_word()) {
            self.push(self.at + number_end(rest), TokenKind::Number);
            return true;
        }
        if is_word_start(self.spec, c) {
            self.word(rest, previous);
            return true;
        }
        false
    }

    fn at_line_start(&self) -> bool {
        self.line[..self.at].trim().is_empty()
    }

    /// Whether a word, a closing bracket or a `.` is right before
    /// [`Lexer::at`]: then a `.` and a digit there are a field, as `t.0`, or
    /// the end of a range, as `0..10`, and not a number of their own.
    fn after_word(&self) -> bool {
        self.line[..self.at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ')' | ']' | '.'))
    }

    fn block_comment_at(&self, rest: &str) -> Option<usize> {
        self.spec
            .block_comments
            .iter()
            .position(|(open, _)| rest.starts_with(open))
    }

    fn line_comment_at(&self, rest: &str) -> bool {
        let opens = self
            .spec
            .line_comments
            .iter()
            .any(|mark| rest.starts_with(mark));
        opens
            && (!self.spec.comment_after_blank
                || self.line[..self.at]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace))
    }

    /// Where a `#` directive or attribute at the start of `rest` ends.
    fn hash_at(&self, rest: &str) -> Option<usize> {
        let after = rest.strip_prefix('#')?;
        match self.spec.hash {
            Hash::Nothing => None,
            Hash::Directive if !self.at_line_start() => None,
            Hash::Directive | Hash::Anywhere => {
                let blank = after.len() - after.trim_start().len();
                let word = word_end(self.spec, &after[blank..]);
                (word > 0).then_some(1 + blank + word)
            }
            Hash::Bracketed => {
                let bracket = after.strip_prefix('!').unwrap_or(after);
                bracket
                    .starts_with('[')
                    .then(|| rest.len() - bracket.len() + bracket_end(bracket))
            }
        }
    }

    /// The heredoc `<<WORD`, `<<-WORD`, `<<'WORD'`, `<<"WORD"` or `<<\WORD`
    /// at the start of `rest` opens, as how long its operator is up to the
    /// word, how long the word is with its quotes, and what the next line
    /// starts inside. A here-string's `<<<` opens none, nor a shift inside
    /// `((…))`, nor a word that starts with a digit, as `<< 2`.
    fn heredoc_at(&mut self, rest: &str) -> Option<(usize, usize, Within)> {
        let after = rest.strip_prefix("<<")?;
        if after.starts_with('<') || self.line[..self.at].ends_with('<') {
            return None;
        }
        let (tabs, after) = match after.strip_prefix('-') {
            Some(after) => (true, after),
            None => (false, after),
        };
        let blank = after.len() - after.trim_start_matches(is_blank).len();
        let (length, word) = heredoc_word(&after[blank..])?;
        let word = HeredocWord::new(word)?;
        if self.in_arithmetic() {
            return None;
        }
        let operator = rest.len() - after.len() + blank;
        Some((operator, length, Within::Heredoc { word, tabs }))
    }

    /// Whether [`Lexer::at`] is inside a `((…))` the line opened, where
    /// `<<` shifts.
    fn in_arithmetic(&mut self) -> bool {
        let bytes = self.line.as_bytes();
        let parens = &mut self.parens;
        while parens.scanned + 2 <= self.at {
            let pair = bytes.get(parens.scanned..parens.scanned + 2);
            parens.scanned += match pair {
                Some(b"((") => {
                    parens.opens = parens.opens.saturating_add(1);
                    2
                }
                Some(b"))") => {
                    parens.closes = parens.closes.saturating_add(1);
                    2
                }
                _ => 1,
            };
        }
        parens.opens > parens.closes
    }

    /// The string a quote at the start of `rest` opens — directly or after
    /// one of the language's prefixes — as how long the prefix is and which
    /// of the language's quotes it is.
    fn quote_at(&self, rest: &str) -> Option<(usize, u8)> {
        let open = |text: &str| {
            self.spec
                .quotes
                .iter()
                .position(|quote| text.starts_with(quote.open))
                .and_then(|which| u8::try_from(which).ok())
        };
        if let Some(which) = open(rest) {
            return Some((0, which));
        }
        self.spec
            .string_prefixes
            .iter()
            .filter(|prefix| rest.starts_with(**prefix))
            .find_map(|prefix| open(&rest[prefix.len()..]).map(|which| (prefix.len(), which)))
    }

    /// Reads the string opened by the language's quote numbered `which`
    /// after a prefix `prefix` bytes long: a key, where the language's key
    /// mark follows it on its line, as JSON's `"name":`.
    fn string(&mut self, prefix: usize, which: u8) -> bool {
        let Some(quote) = self.spec.quotes.get(usize::from(which)).copied() else {
            return false;
        };
        let (end, within) =
            quote_from(self.line, self.at + prefix + quote.open.len(), quote, which);
        let kind = match within == Within::Code && self.is_key(end) {
            true => TokenKind::Property,
            false => TokenKind::String,
        };
        self.push(end, kind);
        self.within = within;
        true
    }

    /// Where a block comment whose body goes on from `from`, `depth` deep,
    /// ends, and what the line is inside after it.
    fn comment_from(&self, from: usize, which: usize, depth: u8) -> (usize, Within) {
        let Some(&(open, close)) = self.spec.block_comments.get(which) else {
            return (from, Within::Code);
        };
        let mut depth = depth;
        let mut at = from;
        while let Some(rest) = self.line.get(at..).filter(|rest| !rest.is_empty()) {
            if rest.starts_with(close) {
                at += close.len();
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return (at, Within::Code);
                }
            } else if self.spec.nested_comments && rest.starts_with(open) {
                at += open.len();
                depth = depth.saturating_add(1);
            } else {
                at += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        let which = u8::try_from(which).unwrap_or(0);
        (
            self.line.len(),
            Within::Comment {
                which,
                depth: depth.max(1),
            },
        )
    }

    /// `'a'` as a character and `'a` as a lifetime or a label.
    fn lifetime_or_char(&mut self, rest: &str) -> bool {
        let mut chars = rest.chars().skip(1);
        let is_char = matches!(
            (chars.next(), chars.next()),
            (Some('\\'), _) | (Some(_), Some('\''))
        );
        if is_char {
            let end = quoted_end(self.line, self.at + 1, CHAR).unwrap_or(self.line.len());
            self.push(end, TokenKind::String);
            return true;
        }
        let end = 1 + word_end(self.spec, &rest[1..]);
        if end == 1 {
            return false;
        }
        self.push(self.at + end, TokenKind::Attribute);
        true
    }

    /// Reads the word at the start of `rest`, after the word `previous`, and
    /// names it by what it is or by what is next to it.
    fn word(&mut self, rest: &'a str, previous: Option<&'a str>) {
        let mut end = word_end(self.spec, rest);
        let word = &rest[..end];
        let after = &rest[end..];
        let follows =
            |intros: &[&str]| previous.is_some_and(|previous| self.is_listed(intros, previous));
        let kind = if self.is_listed(self.spec.keywords, word) {
            Some(TokenKind::Keyword)
        } else if self.is_constant(word) {
            Some(TokenKind::Constant)
        } else if self.is_listed(self.spec.types, word) {
            Some(TokenKind::Type)
        } else if follows(self.spec.function_intros) {
            Some(TokenKind::Function)
        } else if follows(self.spec.type_intros) {
            Some(TokenKind::Type)
        } else if self.is_key(self.at + end) {
            Some(TokenKind::Property)
        } else if self.spec.bang_macros && after.starts_with('!') && !after.starts_with("!=") {
            end += 1;
            Some(TokenKind::Function)
        } else if self.spec.calls_are_functions && after.starts_with('(') {
            Some(TokenKind::Function)
        } else if self.spec.capitals_are_types && word.starts_with(char::is_uppercase) {
            Some(TokenKind::Type)
        } else {
            None
        };
        match kind {
            Some(kind) => self.push(self.at + end, kind),
            None => self.at += end,
        }
        self.previous_word = Some(word);
    }

    fn is_listed(&self, list: &[&str], word: &str) -> bool {
        match self.spec.case_insensitive {
            true => list.iter().any(|listed| listed.eq_ignore_ascii_case(word)),
            false => list.contains(&word),
        }
    }

    fn is_constant(&self, word: &str) -> bool {
        self.is_listed(self.spec.constants, word)
    }

    /// Whether what ends at `end` is a key: the language's key mark comes
    /// after it, past any blanks, and is not the start of a longer operator
    /// or, after a colon, of a path.
    fn is_key(&self, end: usize) -> bool {
        let after = self.line.get(end..).unwrap_or_default().trim_start();
        match self.spec.keys {
            Keys::Nothing => false,
            Keys::Colon => after
                .strip_prefix(':')
                .is_some_and(|rest| !rest.starts_with(goes_on_from_colon)),
            Keys::Equals => after
                .strip_prefix('=')
                .is_some_and(|rest| !rest.starts_with('=')),
        }
    }
}

// Searched with a named function rather than a char array: a `[char; N]`
// pattern shares its searcher with every other one of that length in the
// crate, and under the release profile's `opt-level = "z"` one more caller
// stopped it being inlined into `text::one_line`, which a megabyte prompt
// runs through on every frame.
/// Whether `c`, right after a colon, makes it something other than a key's:
/// `::`, `:/` as in `http://`, or `:=`.
fn goes_on_from_colon(c: char) -> bool {
    matches!(c, ':' | '/' | '=')
}

/// Whether `line` closes a heredoc whose word is `word`: it is the word
/// alone, after any blanks where the heredoc opened with `<<-`. The shell
/// strips only tabs there, but a transcript expands a code block's tabs
/// before it reads the line, and a body that never closes would colour the
/// rest of the block.
fn closes_heredoc(line: &str, word: HeredocWord, tabs: bool) -> bool {
    let line = match tabs {
        true => line.trim_start_matches(is_blank),
        false => line,
    };
    line.as_bytes() == word.as_bytes()
}

/// The word a heredoc's operator is followed by, at the start of `text`, as
/// how long it is with its quotes and the word the body is closed by:
/// `'WORD'` and `"WORD"` whole, `\WORD` and `WORD` as a name.
fn heredoc_word(text: &str) -> Option<(usize, &str)> {
    let quote = text.chars().next()?;
    if quote == '\'' || quote == '"' {
        let body = &text[1..];
        let close = body.find(quote)?;
        return Some((close + 2, &body[..close]));
    }
    let escaped = usize::from(quote == '\\');
    let name = &text[escaped..];
    let end = match name.chars().next() {
        Some(c) if c.is_alphabetic() || c == '_' => name
            .char_indices()
            .find(|(_, c)| !is_heredoc_name_char(*c))
            .map_or(name.len(), |(at, _)| at),
        _ => return None,
    };
    Some((escaped + end, &name[..end]))
}

fn is_heredoc_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// A character literal: one character, or an escape, between `'`s.
const CHAR: Quote = Quote {
    open: "'",
    close: "'",
    escapes: true,
    multiline: false,
};

/// Where a string written with `quote`, whose body goes on from `from` in
/// `line`, ends, and what the line is inside after it: the string, where it
/// is still open at the end of the line and the language lets it run on.
fn quote_from(line: &str, from: usize, quote: Quote, which: u8) -> (usize, Within) {
    match quoted_end(line, from, quote) {
        Some(end) => (end, Within::Code),
        None if quote.multiline => (line.len(), Within::Quote { which }),
        None => (line.len(), Within::Code),
    }
}

/// Where a raw string whose body goes on from `from` in `line`, closed by a
/// quote and `hashes` hashes, ends, and what the line is inside after it.
fn raw_from(line: &str, from: usize, hashes: u8) -> (usize, Within) {
    let body = line.get(from..).unwrap_or_default();
    let mut search = 0;
    while let Some(found) = body.get(search..).and_then(|rest| rest.find('"')) {
        let quote = search + found;
        let after = &body[quote + 1..];
        let closing = after.len() - after.trim_start_matches('#').len();
        if closing >= usize::from(hashes) {
            return (from + quote + 1 + usize::from(hashes), Within::Code);
        }
        search = quote + 1;
    }
    (line.len(), Within::Raw { hashes })
}

/// Where the string whose body starts at `from` in `line` ends, past its
/// close, or `None` where the line ends first.
fn quoted_end(line: &str, from: usize, quote: Quote) -> Option<usize> {
    let mut at = from;
    while let Some(rest) = line.get(at..).filter(|rest| !rest.is_empty()) {
        if quote.escapes && rest.starts_with('\\') {
            at += 1;
            at += line
                .get(at..)
                .and_then(|after| after.chars().next())
                .map_or(0, char::len_utf8);
            continue;
        }
        if rest.starts_with(quote.close) {
            return Some(at + quote.close.len());
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// A raw string's opening at the start of `rest` — `r"`, `br#"`, `cr##"` —
/// as how long it is and how many hashes close it.
fn raw_open(rest: &str) -> Option<(usize, u8)> {
    let after = rest
        .strip_prefix("br")
        .or_else(|| rest.strip_prefix("cr"))
        .or_else(|| rest.strip_prefix('r'))?;
    let hashes = after.len() - after.trim_start_matches('#').len();
    after[hashes..].starts_with('"').then(|| {
        let prefix = rest.len() - after.len();
        (prefix + hashes + 1, u8::try_from(hashes).unwrap_or(u8::MAX))
    })
}

/// Where a `[…]` at the start of `text` ends: past the `]` that closes it,
/// or at the end of the line.
fn bracket_end(text: &str) -> usize {
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return at + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// Where a section's name, `[name]` or `[[name]]`, at the start of `text`
/// ends.
fn section_end(text: &str) -> usize {
    let Some(close) = text.find(']') else {
        return text.len();
    };
    close + text[close..].len() - text[close..].trim_start_matches(']').len()
}

fn is_word_start(spec: &Spec, c: char) -> bool {
    c.is_alphabetic() || c == '_' || spec.word_start_extra.contains(c)
}

fn is_word_char(spec: &Spec, c: char) -> bool {
    c.is_alphanumeric() || c == '_' || spec.word_extra.contains(c)
}

/// Where the word at the start of `text` ends; nothing where no word starts
/// there.
fn word_end(spec: &Spec, text: &str) -> usize {
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, c)) if is_word_start(spec, c) => {}
        _ => return 0,
    }
    chars
        .find(|(_, c)| !is_word_char(spec, *c))
        .map_or(text.len(), |(at, _)| at)
}

/// Where the dotted name at the start of `text` — `functools.cache` — ends.
fn dotted_word_end(spec: &Spec, text: &str) -> usize {
    let mut end = word_end(spec, text);
    while end > 0 && text[end..].starts_with('.') {
        let next = word_end(spec, &text[end + 1..]);
        if next == 0 {
            break;
        }
        end += 1 + next;
    }
    end
}

/// Where a variable a sigil at the start of `text` marks ends: `$name`,
/// `$1`, `${name}`, and `$(name)` where the brackets hold one word, as a
/// makefile's variable — a shell's `$(…)` runs a command, which is not one.
fn sigil_end(spec: &Spec, text: &str) -> Option<usize> {
    let after = text.get(1..)?;
    if after.starts_with('{') {
        return Some(1 + after.find('}').map_or(after.len(), |close| close + 1));
    }
    if let Some(inside) = after.strip_prefix('(') {
        let word = word_end(spec, inside);
        return (word > 0 && inside[word..].starts_with(')')).then_some(word + 3);
    }
    if after.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '?' | '!' | '#' | '@')) {
        return Some(2);
    }
    let word = word_end(spec, after);
    (word > 0).then_some(1 + word)
}

fn starts_digit(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
}

/// Where the number at the start of `text` ends: its digits in any base,
/// `_` between them, one decimal point followed by a digit — so `0..10` is
/// two numbers and `1.max(2)` a number and a call — an exponent with its
/// sign, and a suffix of letters.
fn number_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let based = text.starts_with("0x") || text.starts_with("0X");
    let mut at = 0;
    let mut point = false;
    while let Some(&b) = bytes.get(at) {
        let next = bytes.get(at + 1).copied();
        if !based && matches!(b, b'e' | b'E') && matches!(next, Some(b'+' | b'-')) {
            if bytes.get(at + 2).is_some_and(u8::is_ascii_digit) {
                at += 2;
                continue;
            }
            break;
        }
        if b.is_ascii_alphanumeric() || b == b'_' {
            at += 1;
        } else if b == b'.' && !point && next.is_some_and(|n| n.is_ascii_digit()) {
            point = true;
            at += 1;
        } else {
            break;
        }
    }
    at.max(1)
}
