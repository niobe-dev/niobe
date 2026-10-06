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
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(input) else {
        return None;
    };
    let mut rows = Vec::new();
    object(&fields, 0, width, theme, &mut rows);
    if rows.len() > ROWS + 1 {
        let hidden = rows.len() - ROWS;
        rows.truncate(ROWS);
        let said = format!("… {hidden} more lines · Ctrl+T shows the arguments as sent");
        rows.extend(
            text::wrap(&said, width.max(1))
                .into_iter()
                .map(|line| Line::from(Span::styled(line, Style::new().fg(theme.hot)))),
        );
    }
    Some(rows)
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

/// An object's keys, by [`Rank`] and then in the order they were read.
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
            Value::String(said) => line(&format!("- {said}"), indent, width, theme, rows),
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
    let head = format!("{key}:");
    let lead = " ".repeat(indent);
    let room = width.saturating_sub(indent).max(1);
    let said = match value.is_empty() {
        true => head.clone(),
        false => format!("{head} {value}"),
    };
    let value_style = match dim {
        true => Style::new().fg(theme.dim),
        false => Style::new().fg(theme.fg),
    };
    for (at, wrapped) in text::wrap(&said, room).into_iter().enumerate() {
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
    let wrapped = text::wrap(said.trim_end(), room);
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
    for wrapped in text::wrap(said, room) {
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
