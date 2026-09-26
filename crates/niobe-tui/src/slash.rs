// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Picking one of the backend's own commands with `/`.
//!
//! The commands are the ones the backend listed, folded into the session; the
//! backend is what runs one, when a prompt starts with `/` and its name. What
//! is here is which word of the prompt names a command and which of the
//! listed commands it could be.
//!
//! A `/` on an empty composer searches the transcript, and a second one puts
//! a `/` in the prompt, so a command is always typed as `//` and then its
//! name. Only the start of the prompt names one: that is the only place the
//! backend reads a command from.

use niobe_core::event::SlashCommand;

/// What has been typed after the `/` that opens the prompt, where the cursor
/// is at the end of that first word.
pub(crate) fn at_cursor(lines: &[String], cursor: (usize, usize)) -> Option<String> {
    let (row, column) = cursor;
    if row != 0 {
        return None;
    }
    let line: Vec<char> = lines.first()?.chars().collect();
    match line.get(..column)? {
        ['/', typed @ ..] if !typed.iter().any(|c| c.is_whitespace()) => {
            Some(typed.iter().collect())
        }
        _ => None,
    }
}

/// The model a prompt asks the session to move to, where the prompt is the
/// backend's `/model` command with one name after it and nothing else.
///
/// Such a prompt is sent as the change the model picker makes, not as a turn.
/// Typed as a turn, the `claude` CLI moves the model and says so only when the
/// next turn starts — recorded from Claude Code 2.1.282, the reply was a line
/// of its own and the next `init` the first to name the new model — so the
/// menu row would name the old model until then. Anything else `/model` is
/// given, a bare `/model` included, goes to the backend as typed: it answers
/// those itself, and a name the picker would have to guess at is not one it
/// should send.
pub(crate) fn model_named(prompt: &str, commands: &[SlashCommand]) -> Option<String> {
    let mut words = prompt.split_whitespace();
    let (Some("/model"), Some(model), None) = (words.next(), words.next(), words.next()) else {
        return None;
    };
    commands
        .iter()
        .any(|command| command.name == "model")
        .then(|| model.to_owned())
}

/// Up to `limit` of `commands` that `typed` could name.
///
/// A command matches when its name holds what was typed, ignoring case. One
/// whose name starts with it comes first; within each, the backend's own
/// order, which is the order it lists them in everywhere else.
pub(crate) fn candidates<'a>(
    commands: &'a [SlashCommand],
    typed: &str,
    limit: usize,
) -> Vec<&'a SlashCommand> {
    let typed = typed.to_lowercase();
    let mut ranked: Vec<(u8, usize, &SlashCommand)> = commands
        .iter()
        .enumerate()
        .filter_map(|(at, command)| {
            let name = command.name.to_lowercase();
            let rank = if name.starts_with(&typed) {
                0
            } else if name.contains(&typed) {
                1
            } else {
                return None;
            };
            Some((rank, at, command))
        })
        .collect();
    ranked.sort_unstable_by_key(|(rank, at, _)| (*rank, *at));
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, command)| command)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn command(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: String::new(),
            argument_hint: None,
        }
    }

    #[test]
    fn a_slash_that_opens_the_prompt_is_a_command_being_named() {
        assert_eq!(at_cursor(&lines("/comp"), (0, 5)), Some("comp".to_owned()));
        assert_eq!(at_cursor(&lines("/"), (0, 1)), Some(String::new()));
        assert_eq!(
            at_cursor(&lines("/code-review-graph:review"), (0, 25)),
            Some("code-review-graph:review".to_owned())
        );
    }

    #[test]
    fn a_slash_anywhere_else_or_behind_the_cursor_is_not() {
        assert_eq!(at_cursor(&lines("see /etc"), (0, 8)), None);
        assert_eq!(at_cursor(&lines("/compact now"), (0, 12)), None);
        assert_eq!(at_cursor(&lines("first\n/second"), (1, 7)), None);
        assert_eq!(at_cursor(&lines("no slash"), (0, 8)), None);
    }

    #[test]
    fn a_command_whose_name_starts_with_what_was_typed_comes_first() {
        let commands = [
            command("context"),
            command("compact"),
            command("autocompact"),
            command("fast"),
        ];
        let names = |typed: &str, limit: usize| -> Vec<String> {
            candidates(&commands, typed, limit)
                .into_iter()
                .map(|command| command.name.clone())
                .collect()
        };

        assert_eq!(names("COMPACT", 10), ["compact", "autocompact"]);
        assert_eq!(names("co", 10), ["context", "compact", "autocompact"]);
        assert_eq!(names("", 2), ["context", "compact"]);
        assert!(names("nothing", 10).is_empty());
    }
}
