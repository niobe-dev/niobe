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
//! A line inside a fenced code block is code, not a statement, and is never
//! read: a reply that shows a log or a script holding the marker has decided
//! nothing by it, whether the block stands alone or inside a quote or a list
//! item, where its fences are written behind the `>` or the bullet. A marker the model dressed in markdown — a list bullet, or
//! bold around the word — is read past, since the request cannot stop a model
//! from formatting it.

/// The word a reply's line opens with to state a decision, colon included.
pub const MARKER: &str = "DECISION:";

/// The longest decision kept, in characters. A line longer than this is a
/// paragraph the model ran on into, and the rest of it is cut with `…` rather
/// than carried whole into a pane row.
pub const LONGEST: usize = 300;

/// Every decision `reply` states, in the order it states them.
pub fn decisions(reply: &str) -> Vec<String> {
    let mut fence: Option<Fence> = None;
    let mut found = Vec::new();
    for line in reply.lines() {
        let held = Held::of(line);
        // Where the quote or the list item the fence was opened in has
        // ended, the fence has with it, and the line is read as any other.
        if let Some(open) = fence.take_if(|open| open.goes_on(line, &held)) {
            if !open.closed_by(&held) {
                fence = Some(open);
            }
            continue;
        }
        fence = Fence::opened(&held);
        if fence.is_none() {
            found.extend(stated(line.trim()));
        }
    }
    found
}

/// A fenced code block that is open: what closes it, and the quote and the
/// list item it was opened in, which close it too where they end.
struct Fence {
    marker: &'static str,
    quotes: usize,
    /// The column a line must be indented to inside the quotes to be in the
    /// list item the fence opened in; 0 where it opened in none.
    indent: usize,
}

impl Fence {
    /// The fence `held` opens, if its content opens one.
    fn opened(held: &Held) -> Option<Self> {
        let marker = ["```", "~~~"]
            .into_iter()
            .find(|marker| held.content.starts_with(marker))?;
        Some(Self {
            marker,
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

    /// Whether `held`, a line inside this fence, closes it: its marker, in
    /// the quote it was opened in. A line quoted deeper is the code's own.
    fn closed_by(&self, held: &Held) -> bool {
        held.quotes == self.quotes && !held.listed && held.content.starts_with(self.marker)
    }
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
    content: &'a str,
}

impl<'a> Held<'a> {
    fn of(line: &'a str) -> Self {
        let mut rest = line;
        let mut quotes = 0;
        while let Some(inside) = rest.trim_start().strip_prefix('>') {
            quotes += 1;
            rest = inside.strip_prefix(' ').unwrap_or(inside);
        }
        let body = rest.trim_start();
        let lead = columns(&rest[..rest.len() - body.len()]);
        let Some((marker, item)) = list_marker(body) else {
            return Self {
                quotes,
                lead,
                listed: false,
                indent: lead,
                content: body,
            };
        };
        let content = item.trim_start();
        Self {
            quotes,
            lead,
            listed: true,
            indent: lead + marker + columns(&item[..item.len() - content.len()]),
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

/// The columns `blank` takes, a tab moving to the next stop of four.
fn columns(blank: &str) -> usize {
    blank.chars().fold(0, |column, c| match c {
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
