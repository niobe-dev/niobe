// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A tool's arguments laid out by key, for a permission question about a call
//! the backend named no target in.
//!
//! Such a question has nothing to show but the arguments, and as the backend
//! sends them they are one JSON string: a page body's line breaks arrive as
//! `\n`, its quotes as `\"`, and a few hundred characters of it wrap mid-word
//! into rows nobody reads before answering. Here each key gets a row, a string
//! is drawn as the lines it holds, a nested object is indented under its key
//! and a list says how many items it has with the items under it.
//!
//! What is approved is still the JSON, so this is never the only form: Ctrl+T
//! draws the arguments as they are sent, character for character, and this
//! layout says so wherever it leaves anything out.
//!
//! Only JSON's own shapes are read. No tool's or server's argument names are,
//! beyond the convention that a key ending in `id` names something rather
//! than saying something.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};

use crate::text;
use crate::theme::Theme;

/// The most rows the layout takes before it is cut, with a row under them
/// saying how many more there are and how to read them all.
const ROWS: usize = 12;

/// The most rows one string's own lines take before the rest of it is
/// counted rather than drawn, so a page body does not push the keys after it
/// out of the question.
const STRING_ROWS: usize = 3;

/// How far a nested key is indented under the key it belongs to.
const INDENT: usize = 2;

/// `input`'s arguments, one key to a row, `width` cells wide; `None` where it
/// is not a JSON object, which is then shown as it is.
pub(crate) fn lines(input: &str, width: usize, theme: &Theme) -> Option<Vec<Line<'static>>> {
    lines_except(input, width, theme, |_| false)
}

/// [`lines`], leaving out the keys `shown` says are already drawn elsewhere.
pub(crate) fn lines_except(
    input: &str,
    width: usize,
    theme: &Theme,
    shown: impl Fn(&str) -> bool,
) -> Option<Vec<Line<'static>>> {
    let fields = fields_without(input, |key, _| shown(key))?;
    Some(rows(&fields, width, theme).0)
}

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

/// `fields` laid out by key, `width` cells wide, cut at [`ROWS`] with rows
/// saying how many more there are: with how many lines those rows stand for,
/// which are not drawn, and how many rows they take, at the end of the rows.
pub(crate) fn rows(
    fields: &Map<String, Value>,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, usize, usize) {
    let mut rows = Vec::new();
    object(fields, 0, width, theme, &mut rows);
    if rows.len() <= ROWS + 1 {
        return (rows, 0, 0);
    }
    let hidden = rows.len() - ROWS;
    rows.truncate(ROWS);
    let said = format!("… {hidden} more lines · Ctrl+T shows the arguments as sent");
    rows.extend(
        text::wrap(&said, width.max(1))
            .into_iter()
            .map(|line| Line::from(Span::styled(line, Style::new().fg(theme.hot)))),
    );
    let marker_rows = rows.len() - ROWS;
    (rows, hidden, marker_rows)
}

/// Where a key is drawn among its siblings, and whether it recedes.
///
/// Text says the most about a call, so it comes first; then figures; then
/// the shapes, whose rows run longest. What only names something — an id —
/// and a flag come last and dim: they decide little an operator reads for.
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

/// An object's keys, by [`Rank`] and then by name.
///
/// By name rather than in the order the backend sent them, because the JSON
/// is read into serde_json's map, which keeps its keys sorted; the order as
/// sent is what Ctrl+T shows.
fn object(
    fields: &Map<String, Value>,
    indent: usize,
    width: usize,
    theme: &Theme,
    rows: &mut Vec<Line<'static>>,
) {
    let mut keys: Vec<(&String, &Value)> = fields.iter().collect();
    keys.sort_by_key(|(key, value)| Rank::of(key, value));
    for (key, value) in keys {
        field(key, value, indent, width, theme, rows);
    }
}

/// One key and what it holds.
fn field(
    key: &str,
    value: &Value,
    indent: usize,
    width: usize,
    theme: &Theme,
    rows: &mut Vec<Line<'static>>,
) {
    let dim = Rank::of(key, value) == Rank::Detail;
    match value {
        // Quoted as JSON, since nothing else would show it: a bare `key:` is
        // an object's header with its keys under it.
        Value::String(said) if said.trim().is_empty() => {
            pair(key, &value.to_string(), indent, width, dim, theme, rows);
        }
        Value::String(said) if said.contains('\n') => {
            pair(key, "", indent, width, dim, theme, rows);
            string(said, indent + INDENT, width, theme, rows);
        }
        Value::String(said) => pair(key, said, indent, width, dim, theme, rows),
        Value::Object(fields) if fields.is_empty() => {
            pair(key, "nothing", indent, width, true, theme, rows);
        }
        Value::Object(fields) => {
            pair(key, "", indent, width, dim, theme, rows);
            object(fields, indent + INDENT, width, theme, rows);
        }
        Value::Array(items) => {
            let count = match items.len() {
                1 => "1 item".to_owned(),
                n => format!("{n} items"),
            };
            pair(key, &count, indent, width, dim, theme, rows);
            list(items, indent + INDENT, width, theme, rows);
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {
            pair(key, &value.to_string(), indent, width, dim, theme, rows);
        }
    }
}

/// A list's items under its key: an object's keys where the list holds one
/// item, each item numbered where it holds more, and a plain value after a
/// dash.
fn list(
    items: &[Value],
    indent: usize,
    width: usize,
    theme: &Theme,
    rows: &mut Vec<Line<'static>>,
) {
    let numbered = items.len() > 1;
    for (at, item) in items.iter().enumerate() {
        match item {
            Value::Object(fields) if numbered => {
                pair(
                    &format!("#{}", at + 1),
                    "",
                    indent,
                    width,
                    true,
                    theme,
                    rows,
                );
                object(fields, indent + INDENT, width, theme, rows);
            }
            Value::Object(fields) => object(fields, indent, width, theme, rows),
            Value::Array(inner) => {
                pair(
                    "-",
                    &format!("{} items", inner.len()),
                    indent,
                    width,
                    true,
                    theme,
                    rows,
                );
                list(inner, indent + INDENT, width, theme, rows);
            }
            Value::String(said) if said.contains('\n') => {
                string(said, indent, width, theme, rows);
            }
            Value::String(said) => line(
                &format!("- {}", text::controls_shown_in_rows(said)),
                indent,
                width,
                theme,
                rows,
            ),
            Value::Number(_) | Value::Bool(_) | Value::Null => {
                line(&format!("- {item}"), indent, width, theme, rows);
            }
        }
    }
}

/// `key: value`, wrapped under itself, with the key dim and the value in the
/// body colour — or dim too, where the key is a [`Rank::Detail`].
#[allow(clippy::too_many_arguments)] // Each is one part of the row; a struct would only name them again.
fn pair(
    key: &str,
    value: &str,
    indent: usize,
    width: usize,
    dim: bool,
    theme: &Theme,
    rows: &mut Vec<Line<'static>>,
) {
    let head = format!("{}:", text::controls_shown(key));
    let lead = " ".repeat(indent);
    let room = width.saturating_sub(indent).max(1);
    let said = match value.is_empty() {
        true => head.clone(),
        false => format!("{head} {}", text::controls_shown_in_rows(value)),
    };
    let value_style = match dim {
        true => Style::new().fg(theme.dim),
        false => Style::new().fg(theme.fg),
    };
    for (at, wrapped) in text::wrap_keeping_spaces(&said, room)
        .into_iter()
        .enumerate()
    {
        let spans = match (at, wrapped.strip_prefix(&head)) {
            (0, Some(rest)) => vec![
                Span::raw(lead.clone()),
                Span::styled(head.clone(), Style::new().fg(theme.dim)),
                Span::styled(rest.to_owned(), value_style),
            ],
            _ => vec![Span::raw(lead.clone()), Span::styled(wrapped, value_style)],
        };
        rows.push(Line::from(spans));
    }
}

/// A string's own lines, each wrapped, at most [`STRING_ROWS`] rows of them
/// and then how many rows are left.
fn string(said: &str, indent: usize, width: usize, theme: &Theme, rows: &mut Vec<Line<'static>>) {
    let room = width.saturating_sub(indent).max(1);
    let wrapped = text::wrap_keeping_spaces(&text::controls_shown_in_rows(said.trim_end()), room);
    let lead = " ".repeat(indent);
    let shown = match wrapped.len() > STRING_ROWS + 1 {
        true => STRING_ROWS,
        false => wrapped.len(),
    };
    for part in wrapped.iter().take(shown) {
        rows.push(Line::from(vec![
            Span::raw(lead.clone()),
            Span::styled(part.clone(), Style::new().fg(theme.fg)),
        ]));
    }
    let hidden = wrapped.len() - shown;
    if hidden > 0 {
        rows.push(Line::from(vec![
            Span::raw(lead),
            Span::styled(
                format!("… {hidden} more lines"),
                Style::new().fg(theme.dim).italic(),
            ),
        ]));
    }
}

/// One plain row, wrapped under itself.
fn line(said: &str, indent: usize, width: usize, theme: &Theme, rows: &mut Vec<Line<'static>>) {
    let room = width.saturating_sub(indent).max(1);
    let lead = " ".repeat(indent);
    for wrapped in text::wrap_keeping_spaces(said, room) {
        rows.push(Line::from(vec![
            Span::raw(lead.clone()),
            Span::styled(wrapped, Style::new().fg(theme.fg)),
        ]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::CYBER;

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
                "filter:",
                "  q: x",
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

    /// An empty object says it holds nothing, dim, rather than a key with
    /// nothing after it.
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
    fn an_empty_or_blank_string_is_drawn_quoted_and_not_as_a_header() {
        assert_eq!(drawn(r#"{"a":""}"#, 40), [r#"a: """#]);
        assert_eq!(drawn(r#"{"a":"\n"}"#, 40), [r#"a: "\n""#]);
        assert_eq!(drawn(r#"{"a":{}}"#, 40), ["a: nothing"]);
        assert_eq!(drawn(r#"{"a":{"b":1}}"#, 40), ["a:", "  b: 1"]);
    }

    #[test]
    fn a_key_holding_a_line_break_is_drawn_on_one_row() {
        assert_eq!(drawn(r#"{"a\nb":"x"}"#, 40), ["a\u{2424}b: x"]);
    }

    #[test]
    fn a_string_is_drawn_as_its_lines_and_a_long_one_says_how_many_are_left() {
        let rows = drawn(r#"{"body":"one\ntwo\nthree\nfour\nfive"}"#, 40);
        assert_eq!(
            rows,
            ["body:", "  one", "  two", "  three", "  … 2 more lines"]
        );
        // One row over is drawn rather than said, as the saying takes a row.
        let rows = drawn(r#"{"body":"one\ntwo\nthree\nfour"}"#, 40);
        assert_eq!(rows, ["body:", "  one", "  two", "  three", "  four"]);
    }

    #[test]
    fn a_list_says_how_many_items_it_holds_and_numbers_them_where_there_are_more() {
        assert_eq!(
            drawn(r#"{"pages":[{"title":"a"}]}"#, 40),
            ["pages: 1 item", "  title: a"]
        );
        assert_eq!(
            drawn(
                r#"{"pages":[{"title":"a"},{"title":"b"}],"tags":["x"]}"#,
                40
            ),
            [
                "pages: 2 items",
                "  #1:",
                "    title: a",
                "  #2:",
                "    title: b",
                "tags: 1 item",
                "  - x",
            ]
        );
    }

    #[test]
    fn a_layout_taller_than_its_rows_is_cut_with_a_row_saying_what_shows_the_rest() {
        let fields: Vec<String> = (0..20).map(|at| format!(r#""k{at:02}":"v""#)).collect();
        let rows = drawn(&format!("{{{}}}", fields.join(",")), 80);
        assert_eq!(rows.len(), ROWS + 1);
        assert_eq!(
            rows[ROWS],
            "… 8 more lines · Ctrl+T shows the arguments as sent"
        );
        // One row over is drawn whole: the row saying so would take its place.
        let fields: Vec<String> = (0..13).map(|at| format!(r#""k{at:02}":"v""#)).collect();
        assert_eq!(drawn(&format!("{{{}}}", fields.join(",")), 80).len(), 13);
    }

    #[test]
    fn a_dim_key_and_a_detail_are_drawn_dim_and_text_in_the_body_colour() {
        let rows = lines(r#"{"query":"etag","page_id":"abc"}"#, 40, &CYBER)
            .expect("the input is an object");
        let colours = |row: &Line<'_>| -> Vec<_> {
            row.spans.iter().skip(1).map(|span| span.style.fg).collect()
        };
        assert_eq!(colours(&rows[0]), [Some(CYBER.dim), Some(CYBER.fg)]);
        assert_eq!(colours(&rows[1]), [Some(CYBER.dim), Some(CYBER.dim)]);
    }
}
