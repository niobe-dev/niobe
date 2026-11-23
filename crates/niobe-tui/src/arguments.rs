// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A tool's arguments, an argument to a row, for a permission question about
//! a call the backend named no target in, or for the arguments what the
//! backend read out of a call says nothing of.
//!
//! As the backend sends them the arguments are one JSON string, and a page
//! body of a few hundred characters wraps into rows nobody reads before
//! answering. Here each argument is one row, `key: value`, cut at the frame
//! as Claude Code's question cuts a tool's arguments: a string's first line,
//! and a list or an object as the start of the JSON it is sent as.
//!
//! What is approved is still the JSON, so this is never the only form: Ctrl+T
//! draws the arguments as they are sent, character for character, and a row
//! that leaves anything out ends in `…`.
//!
//! Only JSON's own shapes are read. No tool's or server's argument names are,
//! beyond the convention that a key ending in `id` names something rather
//! than saying something.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};

use crate::text;
use crate::theme::Theme;

/// The most arguments drawn before the rest are counted, with a row under
/// them saying how many more there are and how to read them all.
const ROWS: usize = 6;

/// `input`'s arguments but for the fields `shown` says are drawn elsewhere,
/// or `None` where it is not a JSON object.
///
/// Read once and kept by a caller that draws on every frame: the input can be
/// the whole of a file being written.
pub(crate) fn fields_without(
    input: &str,
    shown: impl Fn(&str, &Value) -> bool,
) -> Option<Map<String, Value>> {
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(input) else {
        return None;
    };
    fields.retain(|key, value| !shown(key, value));
    Some(fields)
}

/// `fields` an argument to a row, `width` cells wide, cut at [`ROWS`] with a
/// row saying how many more there are: with how many arguments that row
/// stands for, which are not drawn, and how many rows it takes, at the end
/// of the rows.
pub(crate) fn rows(
    fields: &Map<String, Value>,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, usize, usize) {
    let mut keys: Vec<(&String, &Value)> = fields.iter().collect();
    keys.sort_by_key(|(key, value)| Rank::of(key, value));
    // One argument over is drawn rather than counted: the row saying so
    // would take its place.
    let shown = match keys.len() > ROWS + 1 {
        true => ROWS,
        false => keys.len(),
    };
    let mut rows: Vec<Line<'static>> = keys
        .iter()
        .take(shown)
        .map(|(key, value)| row(key, value, width, theme))
        .collect();
    let over = keys.len() - shown;
    if over == 0 {
        return (rows, 0, 0);
    }
    let marker: Vec<Line<'static>> = text::wrap(&crate::proposed::more_lines(over), width.max(1))
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::new().fg(theme.hot))))
        .collect();
    let marker_rows = marker.len();
    rows.extend(marker);
    (rows, over, marker_rows)
}

/// Where a key is drawn among its siblings, and whether it recedes.
///
/// Text says the most about a call, so it comes first; then figures; then
/// the shapes. What only names something — an id — and a flag come last and
/// dim: they decide little an operator reads for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Text,
    Figure,
    Shape,
    Detail,
}

impl Rank {
    fn of(key: &str, value: &Value) -> Self {
        match value {
            Value::Bool(_) => Rank::Detail,
            Value::String(_) if names_something(key) => Rank::Detail,
            Value::String(_) => Rank::Text,
            Value::Number(_) | Value::Null => Rank::Figure,
            Value::Array(_) | Value::Object(_) => Rank::Shape,
        }
    }
}

/// Whether a key's value is an identifier rather than words: `id`,
/// `page_id`, `dataSourceId`, `ids`.
fn names_something(key: &str) -> bool {
    matches!(key, "id" | "ids")
        || ["_id", "_ids", "Id", "Ids"]
            .iter()
            .any(|end| key.ends_with(end))
}

/// One argument as `key: value` on one row `width` cells wide, ending in `…`
/// where anything of it is not drawn, with the key dim and the value in the
/// body colour — or dim too, where the key is a [`Rank::Detail`] or the value
/// says there is nothing.
fn row(key: &str, value: &Value, width: usize, theme: &Theme) -> Line<'static> {
    let head = format!("{}:", text::controls_shown(key));
    let (said, dim) = summary(value, width);
    let dim = dim || Rank::of(key, value) == Rank::Detail;
    let whole = text::truncate(&format!("{head} {said}"), width);
    let value_style = match dim {
        true => Style::new().fg(theme.dim),
        false => Style::new().fg(theme.fg),
    };
    match whole.strip_prefix(&head) {
        Some(rest) => Line::from(vec![
            Span::styled(head.clone(), Style::new().fg(theme.dim)),
            Span::styled(rest.to_owned(), value_style),
        ]),
        None => Line::from(Span::styled(whole, Style::new().fg(theme.dim))),
    }
}

/// What a row says of `value`, read no further than a row `width` cells wide
/// could show — a value can be megabytes, and the row is laid out again with
/// every question — and whether it says there is nothing.
fn summary(value: &Value, width: usize) -> (String, bool) {
    // Past this many bytes a row is full whatever the characters are: none
    // takes fewer than one cell per four bytes.
    let most = width.saturating_mul(4).saturating_add(4);
    match value {
        // Quoted as JSON, since nothing else would show it.
        Value::String(said) if said.trim().is_empty() => (value.to_string(), false),
        Value::String(said) => {
            let said = said.trim();
            let (first, more) = match said.split_once('\n') {
                Some((first, _)) => (first.trim_end(), true),
                None => (said, false),
            };
            let first = match first.char_indices().nth(most) {
                Some((at, _)) => &first[..at],
                None => first,
            };
            let first = text::controls_shown_in_rows(first);
            match more {
                true => (format!("{first} …"), false),
                false => (first.into_owned(), false),
            }
        }
        Value::Object(fields) if fields.is_empty() => ("nothing".to_owned(), true),
        Value::Array(_) | Value::Object(_) => (
            text::controls_shown_in_rows(&json_prefix(value, most)).into_owned(),
            false,
        ),
        Value::Number(_) | Value::Bool(_) | Value::Null => (value.to_string(), false),
    }
}

/// The first `most` bytes or fewer of `value` as compact JSON, ending on a
/// whole character: written no further, since a list can hold a whole file.
fn json_prefix(value: &Value, most: usize) -> String {
    let mut written = Bounded {
        bytes: Vec::new(),
        most,
    };
    // Running out of room is the writer's error, and it is how the JSON is
    // cut: what was written up to it is the prefix.
    let _ = serde_json::to_writer(&mut written, value);
    match String::from_utf8(written.bytes) {
        Ok(said) => said,
        Err(cut) => {
            let valid = cut.utf8_error().valid_up_to();
            let mut bytes = cut.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
    }
}

/// A writer that keeps its first `most` bytes and refuses the rest.
struct Bounded {
    bytes: Vec<u8>,
    most: usize,
}

impl std::io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = self.most.saturating_sub(self.bytes.len());
        if room == 0 {
            return Err(std::io::Error::other("the row is full"));
        }
        let taken = room.min(buf.len());
        self.bytes.extend_from_slice(&buf[..taken]);
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::CYBER;

    /// `input`'s arguments, an argument to a row, `width` cells wide; `None`
    /// where it is not a JSON object.
    fn lines(input: &str, width: usize, theme: &Theme) -> Option<Vec<Line<'static>>> {
        let fields = fields_without(input, |_, _| false)?;
        Some(rows(&fields, width, theme).0)
    }

    fn drawn(input: &str, width: usize) -> Vec<String> {
        lines(input, width, &CYBER)
            .expect("the input is an object")
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn what_is_not_an_object_is_left_to_be_shown_as_it_is() {
        for input in ["", "not json", "[1, 2]", "\"text\"", "42"] {
            assert!(lines(input, 40, &CYBER).is_none(), "{input}");
        }
    }

    #[test]
    fn text_comes_before_figures_shapes_and_then_what_only_names_something() {
        let rows = drawn(
            r#"{"allow_async":true,"page_id":"abc","size":10,"filter":{"q":"x"},"query":"etag"}"#,
            40,
        );
        assert_eq!(
            rows,
            [
                "query: etag",
                "size: 10",
                r#"filter: {"q":"x"}"#,
                "allow_async: true",
                "page_id: abc",
            ]
        );
    }

    /// A camelCase identifier is one as much as a snake_case one is.
    #[test]
    fn a_camel_case_id_or_ids_names_something() {
        let rows = lines(
            r#"{"dataSourceId":"abc","pageIds":"x,y","query":"etag","Idle":"no"}"#,
            40,
            &CYBER,
        )
        .expect("the input is an object");
        let drawn: Vec<(String, Option<_>)> = rows
            .iter()
            .map(|row| {
                let said: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
                (said, row.spans.last().and_then(|span| span.style.fg))
            })
            .collect();
        assert_eq!(
            drawn,
            [
                ("Idle: no".to_owned(), Some(CYBER.fg)),
                ("query: etag".to_owned(), Some(CYBER.fg)),
                ("dataSourceId: abc".to_owned(), Some(CYBER.dim)),
                ("pageIds: x,y".to_owned(), Some(CYBER.dim)),
            ]
        );
    }

    /// An empty object says it holds nothing, dim, rather than `{}`.
    #[test]
    fn an_empty_object_says_it_holds_nothing() {
        let rows = lines(r#"{"filter":{}}"#, 40, &CYBER).expect("the input is an object");
        let said: Vec<String> = rows
            .iter()
            .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        assert_eq!(said, ["filter: nothing"]);
        assert_eq!(
            rows[0].spans.last().and_then(|span| span.style.fg),
            Some(CYBER.dim)
        );
    }

    #[test]
    fn keys_of_one_rank_are_drawn_in_the_order_of_their_names() {
        let rows = drawn(r#"{"subject":"hi","body":"there"}"#, 40);
        assert_eq!(rows, ["body: there", "subject: hi"]);
    }

    #[test]
    fn an_empty_or_blank_string_is_drawn_quoted() {
        assert_eq!(drawn(r#"{"a":""}"#, 40), [r#"a: """#]);
        assert_eq!(drawn(r#"{"a":"\n"}"#, 40), [r#"a: "\n""#]);
    }

    #[test]
    fn a_key_holding_a_line_break_is_drawn_on_one_row() {
        assert_eq!(drawn(r#"{"a\nb":"x"}"#, 40), ["a\u{2424}b: x"]);
    }

    /// A string of several lines is its first, marked as having more.
    #[test]
    fn a_string_of_several_lines_is_drawn_as_its_first_and_says_there_is_more() {
        assert_eq!(drawn(r#"{"body":"one\ntwo\nthree"}"#, 40), ["body: one …"]);
    }

    /// A list or an object is the start of the JSON it is sent as, cut at the
    /// row's edge.
    #[test]
    fn a_list_or_an_object_is_the_json_it_is_sent_as_cut_at_the_row() {
        assert_eq!(drawn(r#"{"tags":["x","y"]}"#, 40), [r#"tags: ["x","y"]"#]);
        assert_eq!(
            drawn(r#"{"pages":[{"title":"a long title for a page"}]}"#, 24),
            [r#"pages: [{"title":"a lon…"#]
        );
    }

    /// Every row is one row, however long the value: a page body as long as a
    /// file is cut at the row's edge rather than wrapped.
    #[test]
    fn a_value_wider_than_the_row_is_cut_at_its_edge() {
        let long = "word ".repeat(200_000);
        for input in [
            serde_json::json!({ "body": long }),
            serde_json::json!({ "pages": [{ "content": long }] }),
        ] {
            let rows = drawn(&input.to_string(), 30);
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(text::width(&rows[0]), 30, "{rows:?}");
            assert!(rows[0].ends_with('…'), "{rows:?}");
        }
    }

    /// The JSON is cut on a whole character, never inside one.
    #[test]
    fn a_list_cut_inside_a_character_keeps_only_whole_ones() {
        let said = json_prefix(&serde_json::json!(["ééééé"]), 6);
        assert_eq!(said, "[\"éé");
    }

    #[test]
    fn a_dim_key_and_a_detail_are_drawn_dim_and_text_in_the_body_colour() {
        let rows = lines(r#"{"query":"etag","page_id":"abc"}"#, 40, &CYBER)
            .expect("the input is an object");
        let colours =
            |row: &Line<'_>| -> Vec<_> { row.spans.iter().map(|span| span.style.fg).collect() };
        assert_eq!(colours(&rows[0]), [Some(CYBER.dim), Some(CYBER.fg)]);
        assert_eq!(colours(&rows[1]), [Some(CYBER.dim), Some(CYBER.dim)]);
    }

    /// One argument more than the cut would leave is drawn, not counted: a
    /// marker row would take the room that argument needed.
    #[test]
    fn an_argument_over_the_cut_is_drawn_and_two_over_are_counted() {
        let object = |keys: usize| {
            let fields: Vec<String> = (0..keys).map(|i| format!("\"k{i:02}\":\"v\"")).collect();
            format!("{{{}}}", fields.join(","))
        };

        let one_over = drawn(&object(ROWS + 1), 60);
        assert_eq!(one_over.len(), ROWS + 1, "{one_over:?}");
        assert!(
            !one_over.iter().any(|row| row.contains("more")),
            "{one_over:?}"
        );

        let two_over = drawn(&object(ROWS + 2), 60);
        assert_eq!(two_over.len(), ROWS + 1, "{two_over:?}");
        assert_eq!(
            two_over[ROWS],
            "… 2 more lines · Ctrl+T shows the whole call"
        );
    }
}
