// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The decisions an agent says it made, read out of its own replies.
//!
//! No backend's stream has a record of a decision: what an agent chose, and
//! what it left out, is in its words or nowhere. So Niobe asks for it — the
//! binary appends a request to the backend's system prompt that names
//! [`MARKER`] — and reads every line of a finished reply that opens with it.
//!
//! What comes out is the agent's own account of its reasoning, which it may
//! have wrong. Everything that shows a decision says so; nothing here or
//! downstream counts one as a fact about the code.
//!
//! A line of a code block is code, not a statement, and is never read: a
//! reply that shows a log or a script holding the marker has decided nothing
//! by it. That is a fenced block, whether it stands alone or inside a quote or
//! a list item, where its fences are written behind the `>` or the bullet; and
//! an indented one, as CommonMark and the transcript read it: a line indented
//! four columns or more past the content of the list item it is in, or of its
//! quote where it is in none — a tab reaching the next stop of four — where no
//! paragraph is open, which is at the reply's start and after a blank line, a
//! heading, a rule or a fence. An indented line straight after a paragraph's
//! goes on with that paragraph, as does one outside the paragraph's quote or
//! list item, lazily, and is read. The lines are read by the quotes and the
//! list items they sit in rather than parsed as a whole document, so a block
//! that is not followed, such as an HTML block, reads as text.
//!
//! A marker the model dressed in markdown — a list bullet, or bold around the
//! word — is read past, since the request cannot stop a model from
//! formatting it.

/// The word a reply's line opens with to state a decision, colon included.
pub const MARKER: &str = "DECISION:";

/// The longest decision kept, in characters. A line longer than this is a
/// paragraph the model ran on into, and the rest of it is cut with `…` rather
/// than carried whole into a pane row.
pub const LONGEST: usize = 300;

/// Every decision `reply` states, in the order it states them.
pub fn decisions(reply: &str) -> Vec<String> {
    let mut fence: Option<Fence> = None;
    let mut blocks = Blocks::default();
    let mut found = Vec::new();
    for line in reply.lines() {
        let held = Held::of(line);
        // Where the quote or the list item the fence was opened in has
        // ended, the fence has with it, and the line is read as any other.
        if let Some(open) = fence.take_if(|open| open.goes_on(line, &held)) {
            match open.closed_by(&held) {
                true => blocks.fence_closed(),
                false => fence = Some(open),
            }
            continue;
        }
        let opens = Fence::opened(&held);
        if blocks.code(&held, opens.is_some()) {
            continue;
        }
        fence = opens;
        if fence.is_none() {
            found.extend(stated(line.trim()));
        }
    }
    found
}

/// What the lines before a line outside a fence leave open for it: the
/// paragraph an indented line goes on with, and the list items whose content
/// its indent is measured from.
#[derive(Debug, Default)]
struct Blocks {
    /// How many quotes the last line was in. The list items are those
    /// opened inside as many.
    quotes: usize,
    /// Whether the last line was a paragraph's, which an indented line goes
    /// on with rather than starting code.
    paragraph: bool,
    /// The column each list item open inside the quotes has its content at,
    /// the outermost first.
    items: Vec<usize>,
}

impl Blocks {
    /// Reads `held`, and says whether it is a line of an indented code
    /// block: indented four columns or more past the content of the list
    /// item it is in, or of the quote where it is in none, where no
    /// paragraph is open, as CommonMark reads one. `fenced` says whether the
    /// line opens a fence, which ends a paragraph and starts none.
    fn code(&mut self, held: &Held, fenced: bool) -> bool {
        if held.body.is_empty() {
            self.paragraph = false;
            self.contain(held.quotes);
            return false;
        }
        // The content of the deepest list item the line is indented into,
        // in the quotes it is in.
        let same = held.quotes == self.quotes;
        let base = match same {
            true => self
                .items
                .iter()
                .rev()
                .copied()
                .find(|&content| content <= held.lead),
            false => None,
        }
        .unwrap_or(0);
        let indented = held.lead >= base.saturating_add(4);
        if self.paragraph && held.quotes <= self.quotes && (indented || !interrupts(held, fenced)) {
            // The paragraph's own text, or text that goes on with it lazily
            // from outside its quote or its list item, which leaves both
            // open and cannot be its heading's underline.
            let lazy = !same || base < self.base();
            if !indented && !lazy && underline(held.body) {
                self.paragraph = false;
            }
            return false;
        }
        self.contain(held.quotes);
        self.items.retain(|&content| content <= held.lead);
        if indented {
            self.paragraph = false;
            return true;
        }
        if held.listed && !thematic_break(held.body) {
            self.items.push(held.indent);
        }
        self.paragraph = opens_a_paragraph(held, fenced);
        false
    }

    /// The column the content of the list item the next line may be in
    /// starts at: the innermost open, or the quote's edge.
    fn base(&self) -> usize {
        self.items.last().copied().unwrap_or(0)
    }

    /// Moves into `quotes` quotes, where the list items open in others are
    /// not.
    fn contain(&mut self, quotes: usize) {
        if quotes != self.quotes {
            self.quotes = quotes;
            self.items.clear();
        }
    }

    /// A fenced block ended, which no paragraph goes on past.
    fn fence_closed(&mut self) {
        self.paragraph = false;
    }
}

/// Whether `held`, a line that is not blank, starts a block of its own even
/// where a paragraph is open: a list item, a heading, a thematic break or,
/// where `fenced` says it opens one, a fence.
fn interrupts(held: &Held, fenced: bool) -> bool {
    fenced || held.listed || heading(held.content) || rule(held)
}

/// Whether `held`, a line that is not blank and starts a block, starts a
/// paragraph: text, not a heading, a rule or a fence, nor a list marker
/// with nothing after it; `fenced` says whether it opens a fence.
fn opens_a_paragraph(held: &Held, fenced: bool) -> bool {
    !(fenced || held.content.is_empty() || heading(held.content) || rule(held))
}

/// Whether `held` is a thematic break, or a list item holding one.
fn rule(held: &Held) -> bool {
    thematic_break(held.body) || thematic_break(held.content)
}

/// `#` to `######` and a blank or nothing after it.
fn heading(text: &str) -> bool {
    let hashes = text.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes)
        && text
            .get(hashes..)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
}

/// Three or more of `-`, `*` or `_`, one of them alone, with blanks between.
fn thematic_break(text: &str) -> bool {
    let mut marks = text.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(mark) = marks.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    marks
        .try_fold(1usize, |count, c| (c == mark).then_some(count + 1))
        .is_some_and(|count| count >= 3)
}

/// A run of `=` or of `-` alone, which makes the paragraph above it a
/// heading.
fn underline(body: &str) -> bool {
    let marks = body.trim_end();
    marks.bytes().all(|b| b == b'=') || marks.bytes().all(|b| b == b'-')
}

/// A fenced code block that is open: what closes it, and the quote and the
/// list item it was opened in, which close it too where they end.
struct Fence {
    /// The character the fence is a run of, `` ` `` or `~`.
    mark: char,
    /// How long the run that opened it is: a closing run is at least as long.
    run: usize,
    /// The column the opening run starts at inside the quotes: a closing run
    /// is indented no more than three past it, and one further in is code.
    lead: usize,
    quotes: usize,
    /// The column a line must be indented to inside the quotes to be in the
    /// list item the fence opened in; 0 where it opened in none.
    indent: usize,
}

impl Fence {
    /// The fence `held` opens, if its content opens one.
    fn opened(held: &Held) -> Option<Self> {
        let mark = ['`', '~']
            .into_iter()
            .find(|mark| held.content.starts_with(*mark))?;
        let run = run_of(held.content, mark);
        // A backtick run with a backtick after it is code in a line of text,
        // as CommonMark reads it, and opens nothing.
        let info = held.content.get(run..).unwrap_or_default();
        if run < 3 || (mark == '`' && info.contains('`')) {
            return None;
        }
        Some(Self {
            mark,
            run,
            lead: if held.listed { held.indent } else { held.lead },
            quotes: held.quotes,
            indent: if held.listed { held.indent } else { 0 },
        })
    }

    /// Whether `line`, read as `held`, is still inside the quote and the list
    /// item this fence was opened in: a line quoted fewer times has left the
    /// quote, and one that is not blank and is indented less than the item's
    /// content has left the item.
    fn goes_on(&self, line: &str, held: &Held) -> bool {
        held.quotes >= self.quotes && (line.trim().is_empty() || held.lead >= self.indent)
    }

    /// Whether `held`, a line inside this fence, closes it: in the quote it
    /// was opened in, indented no more than three past it, a run of its
    /// character at least as long as the one that opened it, and nothing
    /// after the run but blanks, as CommonMark
    /// reads a closing fence and the transcript draws one. A line quoted
    /// deeper is the code's own, and so is `` ```sh `` inside `` ```` ``.
    fn closed_by(&self, held: &Held) -> bool {
        let run = run_of(held.content, self.mark);
        held.quotes == self.quotes
            && !held.listed
            && held.lead <= self.lead.saturating_add(3)
            && run >= self.run
            && held
                .content
                .get(run..)
                .is_some_and(|rest| rest.trim().is_empty())
    }
}

/// How many of `mark` `text` opens with, in bytes, which for `` ` `` and `~`
/// is characters.
fn run_of(text: &str, mark: char) -> usize {
    text.chars().take_while(|c| *c == mark).count()
}

/// A line read past the containers it sits in, as markdown nests them: the
/// block quotes it is in, and the list item it opens, if any.
///
/// A fence opened inside a quote or a list item is written behind its
/// markers, so it is only seen as one once they are read off.
struct Held<'a> {
    quotes: usize,
    /// The columns of blank before the content, or before the list marker
    /// where the line opens a list item, inside the quotes.
    lead: usize,
    /// Whether the line opens a list item.
    listed: bool,
    /// The column the content starts at inside the quotes.
    indent: usize,
    /// The line inside the quotes from its first character that is not
    /// blank, list marker and all.
    body: &'a str,
    content: &'a str,
}

impl<'a> Held<'a> {
    fn of(line: &'a str) -> Self {
        let mut rest = line;
        let mut quotes = 0;
        // The column the quotes' content starts at, and how much of a tab
        // straight after the last `>` is the blank the `>` takes with it:
        // a tab is as wide as the columns to the next stop of four from
        // where it is, which inside a quote is not where the quote starts.
        let mut edge = 0;
        let mut taken = 0;
        while let Some(inside) = rest.trim_start().strip_prefix('>') {
            quotes += 1;
            edge = past(edge, &rest[..rest.len() - rest.trim_start().len()]) + 1;
            taken = 0;
            rest = match inside.strip_prefix(' ') {
                Some(after) => {
                    edge += 1;
                    after
                }
                None => {
                    taken = usize::from(inside.starts_with('\t'));
                    inside
                }
            };
        }
        let inside = |column: usize| column.saturating_sub(edge).saturating_sub(taken);
        let body = rest.trim_start();
        let start = past(edge, &rest[..rest.len() - body.len()]);
        let lead = inside(start);
        let Some((marker, item)) = list_marker(body) else {
            return Self {
                quotes,
                lead,
                listed: false,
                indent: lead,
                body,
                content: body,
            };
        };
        let content = item.trim_start();
        Self {
            quotes,
            lead,
            listed: true,
            indent: inside(past(start + marker, &item[..item.len() - content.len()])),
            body,
            content,
        }
    }
}

/// The length of the list marker `body` opens with — a bullet, or a number
/// with its `.` or `)` — and what follows it, where a space follows it.
fn list_marker(body: &str) -> Option<(usize, &str)> {
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    let marker = match digits {
        0 => body
            .bytes()
            .next()
            .filter(|b| b"-*+".contains(b))
            .map(|_| 1),
        1..=9 => body
            .as_bytes()
            .get(digits)
            .filter(|b| b".)".contains(b))
            .map(|_| digits + 1),
        _ => None,
    }?;
    let item = &body[marker..];
    item.starts_with([' ', '\t']).then_some((marker, item))
}

/// The column `blank`, written from `column`, ends at, a tab moving to the
/// next stop of four.
fn past(column: usize, blank: &str) -> usize {
    blank.chars().fold(column, |column, c| match c {
        '\t' => column + 4 - column % 4,
        _ => column + 1,
    })
}

/// The decision `line` states, where it opens with the marker.
fn stated(line: &str) -> Option<String> {
    let line = ["- ", "* ", "+ ", "> "]
        .iter()
        .find_map(|bullet| line.strip_prefix(bullet))
        .unwrap_or(line)
        .trim_start();
    let rest = line
        .strip_prefix(MARKER)
        .or_else(|| line.strip_prefix("**DECISION:**"))
        .or_else(|| line.strip_prefix("**DECISION**:"))?;
    let summary = rest.trim();
    match summary.is_empty() {
        true => None,
        false => Some(bounded(summary)),
    }
}

/// `summary` cut to [`LONGEST`] characters, marked where it was cut.
fn bounded(summary: &str) -> String {
    match summary.char_indices().nth(LONGEST) {
        Some((at, _)) => format!("{}…", summary[..at].trim_end()),
        None => summary.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_opening_with_the_marker_is_a_decision_and_the_rest_of_the_reply_is_not() {
        let reply = "Fixed the parser.\n\
                     DECISION: left the CSV exporter alone; it is out of scope.\n\
                     The tests pass.";

        assert_eq!(
            decisions(reply),
            ["left the CSV exporter alone; it is out of scope."]
        );
    }

    #[test]
    fn several_decisions_are_read_in_the_order_the_reply_states_them() {
        let reply = "DECISION: reused the cache\nwork\n  DECISION:  kept the old flag  \n";

        assert_eq!(decisions(reply), ["reused the cache", "kept the old flag"]);
    }

    #[test]
    fn a_marker_inside_a_fenced_block_is_code_and_not_a_decision() {
        let reply = "Here is the log:\n```\nDECISION: from a fixture\n```\n\
                     ~~~text\nDECISION: also quoted\n~~~\nDECISION: the real one";

        assert_eq!(decisions(reply), ["the real one"]);
    }

    #[test]
    fn a_marker_inside_a_fenced_block_in_a_quote_or_a_list_item_is_code_and_not_a_decision() {
        let reply = "The log said:\n> ```\n> DECISION: from a quoted log\n> ```\n\
                     - the script:\n- ```sh\n  echo 'DECISION: from a listed script'\n  \
                     DECISION: from a listed script\n  ```\n\
                     DECISION: the real one";

        assert_eq!(decisions(reply), ["the real one"]);
    }

    /// As CommonMark reads a fence, and as the transcript draws one: closed
    /// only by a run of its own character at least as long as the one that
    /// opened it, with nothing after it.
    #[test]
    fn a_fence_is_closed_only_by_a_bare_run_at_least_as_long_as_its_own() {
        let nested = "````markdown\n```sh\nDECISION: from the quoted script\n```\n````\nDECISION: the real one";
        assert_eq!(decisions(nested), ["the real one"]);

        let info = "```\n```rust\nDECISION: still code\n```\nDECISION: after";
        assert_eq!(decisions(info), ["after"]);

        let tildes = "~~~~\n~~~\nDECISION: still code\n~~~~~\nDECISION: after";
        assert_eq!(decisions(tildes), ["after"]);

        let other = "```\n~~~\nDECISION: still code\n```\nDECISION: after";
        assert_eq!(decisions(other), ["after"]);
    }

    /// A line indented four columns or more past the fence is code inside
    /// it, however it reads, as CommonMark and the transcript read it; and
    /// a backtick fence's info string holds no backtick.
    #[test]
    fn a_fence_is_not_closed_by_a_line_indented_past_it_and_not_opened_by_a_backtick_run_in_text() {
        let indented = "```\n    ```\nDECISION: still code\n```\nDECISION: after";
        assert_eq!(decisions(indented), ["after"]);

        let listed = "```markdown\n- Install:\n\n    ```sh\n    cargo install x\n    ```\n```\n\nDECISION: documented it";
        assert_eq!(decisions(listed), ["documented it"]);

        let in_an_item = "- Run:\n\n    ```sh\n    DECISION: code\n    ```\n\nDECISION: after";
        assert_eq!(decisions(in_an_item), ["after"]);

        let not_a_fence = "```js` is how it is written\nDECISION: read";
        assert_eq!(decisions(not_a_fence), ["read"]);
    }

    /// As CommonMark reads an indented code block, and as the transcript
    /// draws one: a line indented four columns or more past its container
    /// that does not go on with a paragraph.
    #[test]
    fn a_marker_in_an_indented_code_block_is_code_and_not_a_decision() {
        let after_a_blank = "The log said:\n\n    DECISION: from the fixture\n";
        assert_eq!(decisions(after_a_blank), [] as [&str; 0]);

        let tabbed = "The log said:\n\n\tDECISION: from the fixture\n\nDECISION: after";
        assert_eq!(decisions(tabbed), ["after"]);

        let first = "    DECISION: the reply opens with code\nDECISION: after";
        assert_eq!(decisions(first), ["after"]);

        let after_a_heading = "## Log\n    DECISION: code under a heading\n";
        assert_eq!(decisions(after_a_heading), [] as [&str; 0]);

        let after_a_fence = "```\nx\n```\n    DECISION: code after a fence\n";
        assert_eq!(decisions(after_a_fence), [] as [&str; 0]);

        let quoted = "> The log said:\n>\n>     DECISION: quoted code\n";
        assert_eq!(decisions(quoted), [] as [&str; 0]);

        // A tab after a quote's `>` reaches the next stop of four from
        // where it is, which is two columns past the quote's own blank.
        let tab_in_a_quote = "> The log said:\n>\n> \tDECISION: two columns in\n";
        assert_eq!(decisions(tab_in_a_quote), ["two columns in"]);

        let after_a_rule = "---\n    DECISION: code after a rule\n";
        assert_eq!(decisions(after_a_rule), [] as [&str; 0]);

        let after_an_underline = "Title\n===\n    DECISION: code under a heading\n";
        assert_eq!(decisions(after_an_underline), [] as [&str; 0]);

        // Blank lines inside the block are the block's; a line indented
        // less ends it.
        let long = "\n    one\n\n    DECISION: still code\nDECISION: after";
        assert_eq!(decisions(long), ["after"]);
    }

    /// A list item's content starts past its marker, so code in it is
    /// indented four further than that, and a line indented as far as the
    /// content is the item's text.
    #[test]
    fn code_in_a_list_item_is_indented_four_past_the_items_content() {
        let in_an_item = "- Run it:\n\n      DECISION: code in the item\n\nDECISION: after";
        assert_eq!(decisions(in_an_item), ["after"]);

        let item_text = "- Ran it.\n\n    DECISION: the item's own text\n";
        assert_eq!(decisions(item_text), ["the item's own text"]);

        let numbered = "1. Ran it.\n\n     DECISION: the item's own text\n";
        assert_eq!(decisions(numbered), ["the item's own text"]);

        // Out of a nested item, the outer item's content is what the indent
        // is measured from.
        let nested = "- a\n  - b\n\n  text of a\n\n    DECISION: still a's text\n";
        assert_eq!(decisions(nested), ["still a's text"]);

        let after_the_list = "- a\n\nText.\n\n    DECISION: code after the list\n";
        assert_eq!(decisions(after_the_list), [] as [&str; 0]);
    }

    /// An indented line cannot start code where a paragraph is open: it goes
    /// on with the paragraph, however far it is indented.
    #[test]
    fn an_indented_line_that_goes_on_with_a_paragraph_is_read() {
        let continued = "work\n    DECISION: kept the flag\n";
        assert_eq!(decisions(continued), ["kept the flag"]);

        let in_an_item = "- work\n      DECISION: kept the flag\n";
        assert_eq!(decisions(in_an_item), ["kept the flag"]);

        let lazy = "> quoted work\n    DECISION: kept the flag\n";
        assert_eq!(decisions(lazy), ["kept the flag"]);

        // A line that goes on lazily is text, never the underline that
        // would make the paragraph a heading and end it.
        let lazy_underline = "> quoted work\n===\n    DECISION: kept the flag\n";
        assert_eq!(decisions(lazy_underline), ["kept the flag"]);

        let indented_rule = "work\n    ---\n    DECISION: kept the flag\n";
        assert_eq!(decisions(indented_rule), ["kept the flag"]);
    }

    #[test]
    fn a_quoted_fence_inside_a_fenced_block_is_the_codes_own_and_closes_nothing() {
        let reply = "```\n> ```\nDECISION: still code\n```\nDECISION: after the block";

        assert_eq!(decisions(reply), ["after the block"]);
    }

    #[test]
    fn a_fence_left_open_ends_with_the_quote_or_the_list_item_it_was_opened_in() {
        let reply = "> ```\n> DECISION: quoted code\n\nDECISION: after the quote\n\
                     - ```\n  DECISION: listed code\nDECISION: after the item";

        assert_eq!(decisions(reply), ["after the quote", "after the item"]);
    }

    #[test]
    fn a_marker_the_model_put_in_a_bullet_or_in_bold_is_still_read() {
        let reply = "- DECISION: one\n* **DECISION:** two\n> **DECISION**: three";

        assert_eq!(decisions(reply), ["one", "two", "three"]);
    }

    #[test]
    fn the_marker_mid_line_in_another_case_or_with_nothing_after_it_states_nothing() {
        let reply =
            "We reached a DECISION: none.\nDecision: lower case\nDECISION:   \nDECISIONS: plural";

        assert!(decisions(reply).is_empty());
    }

    #[test]
    fn a_run_on_decision_is_cut_at_the_longest_kept_and_says_so() {
        let long = "é".repeat(LONGEST + 50);
        let found = decisions(&format!("DECISION: {long}"));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chars().count(), LONGEST + 1);
        assert!(found[0].ends_with('…'));
    }
}
