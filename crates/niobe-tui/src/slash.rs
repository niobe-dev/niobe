// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Picking one of the backend's own commands with `/`.
//!
//! The commands are the ones the backend listed, folded into the session; the
//! backend is what runs one, when a prompt starts with `/` and its name. The
//! `claude` CLI lists its skills among them, a plugin's under the plugin's
//! name, so a skill is picked and run the way a command is. What is here is
//! which word of the prompt names a command and which of the listed commands
//! it could be.
//!
//! A `/` that opens the prompt can name any of them: that is where the backend
//! reads a command from. A `/` that starts a word anywhere else — after a
//! blank, or at the start of a later line — can name only what the backend
//! takes there, which for the `claude` CLI is a skill the model runs through
//! its own `Skill` tool ([`SlashCommand::mid_prompt`]); `/compact` in the middle
//! of a prompt is just text. A `/` inside a word, as in a path, names nothing.
//! The transcript is searched with Ctrl+F rather than `/`, so a prompt opens
//! with a command the way it does in the CLI itself.

use niobe_core::event::SlashCommand;

/// The `/` word the cursor is at the end of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Slash {
    /// The line of the prompt it is on.
    pub(crate) row: usize,
    /// The column of its `/`, in characters.
    pub(crate) at: usize,
    /// What has been typed after the `/`, up to the cursor.
    pub(crate) typed: String,
}

impl Slash {
    /// Whether the `/` opens the prompt, which is the one place a command
    /// the backend runs itself is read from.
    pub(crate) fn opens_the_prompt(&self) -> bool {
        (self.row, self.at) == (0, 0)
    }

    /// Whether `command` can be named where this `/` is.
    pub(crate) fn can_name(&self, command: &SlashCommand) -> bool {
        self.opens_the_prompt() || command.mid_prompt
    }
}

/// The `/` word the cursor at `cursor` — a line and a column in characters —
/// is at the end of, where the `/` starts a word and nothing typed after it
/// is a `/` too.
///
/// Only the word is read, back from the cursor to the blank before it, so a
/// key typed into a long prompt does not walk the whole of it.
pub(crate) fn at_cursor(lines: &[String], cursor: (usize, usize)) -> Option<Slash> {
    let (row, column) = cursor;
    let line = lines.get(row)?;
    let before = line.get(..crate::mention::byte_of_column(line, column)?)?;
    let word = before.rsplit(char::is_whitespace).next()?;
    let typed = word.strip_prefix('/')?;
    if typed.contains('/') {
        return None;
    }
    Some(Slash {
        row,
        at: column - word.chars().count(),
        typed: typed.to_owned(),
    })
}

/// Where `line` names a command the backend reads there, each as the byte
/// range of its `/` and its name.
///
/// A name counts only whole, as the backend lists it, and only where a `/`
/// starts a word: at the start of `line` where it `opens_prompt`, any listed
/// command; at the start of any other word, one the backend takes mid-prompt.
/// So an unlisted `/word` and a path are never named. Where two listed names
/// both fit, as `server:prompt` and `server:prompt (MCP)` would, the longer
/// is the one read.
pub(crate) fn named(
    line: &str,
    opens_prompt: bool,
    commands: &[SlashCommand],
) -> Vec<std::ops::Range<usize>> {
    if commands.is_empty() {
        return Vec::new();
    }
    line.match_indices('/')
        .filter(|(at, _)| {
            line[..*at]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace)
        })
        .filter_map(|(at, _)| {
            let rest = &line[at + 1..];
            let opening = opens_prompt && at == 0;
            let length = commands
                .iter()
                .filter(|command| opening || command.mid_prompt)
                .map(|command| command.name.as_str())
                .filter(|name| {
                    !name.is_empty()
                        && rest.starts_with(name)
                        && rest[name.len()..]
                            .chars()
                            .next()
                            .is_none_or(char::is_whitespace)
                })
                .map(str::len)
                .max()?;
            Some(at..at + 1 + length)
        })
        .collect()
}

/// Where `prompt`, as sent, names a command the backend read: [`named`] for
/// each of its lines, the first of which opens it, as byte ranges of the
/// whole prompt.
pub(crate) fn named_in_prompt(
    prompt: &str,
    commands: &[SlashCommand],
) -> Vec<std::ops::Range<usize>> {
    let mut start = 0;
    let mut ranges = Vec::new();
    for line in prompt.split('\n') {
        ranges.extend(
            named(line, start == 0, commands)
                .into_iter()
                .map(|range| range.start + start..range.end + start),
        );
        start += line.len() + 1;
    }
    ranges
}

/// The model a prompt asks the session to move to, where the prompt is the
/// backend's `/model` command with one name after it and nothing else.
///
/// Such a prompt is sent as the change the model picker makes, not as a turn.
/// Typed as a turn, the `claude` CLI moves the model and says so only when the
/// next turn starts — recorded from Claude Code 2.1.282, the reply was a line
/// of its own and the next `init` the first to name the new model — so the
/// menu row would name the old model until then. A bare `/model` opens the
/// picker, as [`crate::menu::TYPED`] says; anything else `/model` is given
/// goes to the backend as typed: it answers those itself, and a name the
/// picker would have to guess at is not one it should send.
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

/// Up to `limit` of the `commands` the `/` word `slash` could name.
///
/// A command matches when its name holds what was typed, ignoring case. One
/// whose name is what was typed comes first, so that Enter on a name typed in
/// full never takes a longer one it starts; then one whose name starts with
/// it; within each, the backend's own order, which is the order it lists them
/// in everywhere else.
pub(crate) fn candidates<'a>(
    commands: &'a [SlashCommand],
    slash: &Slash,
    limit: usize,
) -> Vec<&'a SlashCommand> {
    let typed = slash.typed.to_lowercase();
    let mut ranked: Vec<(u8, usize, &SlashCommand)> = commands
        .iter()
        .enumerate()
        .filter(|(_, command)| slash.can_name(command))
        .filter_map(|(at, command)| {
            let name = command.name.to_lowercase();
            let rank = if name == typed {
                0
            } else if name.starts_with(&typed) {
                1
            } else if name.contains(&typed) {
                2
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
            mid_prompt: false,
        }
    }

    fn typed(text: &str, cursor: (usize, usize)) -> Option<String> {
        at_cursor(&lines(text), cursor).map(|slash| slash.typed)
    }

    fn opening(typed: &str) -> Slash {
        Slash {
            row: 0,
            at: 0,
            typed: typed.to_owned(),
        }
    }

    #[test]
    fn a_slash_that_opens_the_prompt_is_a_command_being_named() {
        assert_eq!(at_cursor(&lines("/comp"), (0, 5)), Some(opening("comp")));
        assert_eq!(typed("/", (0, 1)), Some(String::new()));
        assert_eq!(
            typed("/code-review-graph:review", (0, 25)),
            Some("code-review-graph:review".to_owned())
        );
    }

    #[test]
    fn a_slash_that_starts_a_word_further_on_is_named_where_it_stands() {
        let slash = at_cursor(&lines("fix it with /rev"), (0, 16)).expect("a slash word");
        assert_eq!((slash.row, slash.at, slash.typed.as_str()), (0, 12, "rev"));
        assert!(!slash.opens_the_prompt());

        let slash = at_cursor(&lines("first\n/second"), (1, 7)).expect("a slash word");
        assert_eq!((slash.row, slash.at), (1, 0));
        assert!(
            !slash.opens_the_prompt(),
            "a later line is the middle of the prompt"
        );
        assert_eq!(typed("see\t/etc", (0, 8)), Some("etc".to_owned()));
    }

    #[test]
    fn a_slash_inside_a_word_or_behind_the_cursor_is_not() {
        assert_eq!(typed("src/a/b", (0, 7)), None);
        assert_eq!(typed("see /etc/hosts", (0, 14)), None, "a path");
        assert_eq!(typed("/compact now", (0, 12)), None);
        assert_eq!(typed("no slash", (0, 8)), None);
    }

    #[test]
    fn a_command_is_read_up_to_the_cursor_in_characters() {
        assert_eq!(typed("/rév now", (0, 3)), Some("ré".to_owned()));
        assert_eq!(typed("/rév", (0, 0)), None);
        assert_eq!(typed("/ré", (0, 4)), None, "past the line");
        assert_eq!(typed("é /rév", (0, 5)), Some("ré".to_owned()));
    }

    #[test]
    fn mid_prompt_only_what_the_backend_takes_there_is_offered() {
        let commands = [
            command("compact"),
            SlashCommand {
                mid_prompt: true,
                ..command("compare")
            },
        ];
        let names = |slash: &Slash| -> Vec<&str> {
            candidates(&commands, slash, 10)
                .into_iter()
                .map(|command| command.name.as_str())
                .collect()
        };
        assert_eq!(names(&opening("comp")), ["compact", "compare"]);
        let further = Slash {
            at: 4,
            ..opening("comp")
        };
        assert_eq!(names(&further), ["compare"]);
    }

    #[test]
    fn a_command_whose_name_starts_with_what_was_typed_comes_first() {
        let commands = [
            command("autocompact"),
            command("context"),
            command("compact"),
            command("fast"),
        ];
        let names = |typed: &str, limit: usize| -> Vec<String> {
            candidates(&commands, &opening(typed), limit)
                .into_iter()
                .map(|command| command.name.clone())
                .collect()
        };

        assert_eq!(names("COMPACT", 10), ["compact", "autocompact"]);
        assert_eq!(names("co", 10), ["context", "compact", "autocompact"]);
        assert_eq!(names("", 2), ["autocompact", "context"]);
        assert!(names("nothing", 10).is_empty());
    }

    #[test]
    fn a_command_named_in_full_comes_before_the_longer_ones_it_starts() {
        let commands = [
            command("review-pr"),
            command("Review"),
            command("pre-review"),
        ];
        let names: Vec<&str> = candidates(&commands, &opening("review"), 10)
            .into_iter()
            .map(|command| command.name.as_str())
            .collect();

        assert_eq!(names, ["Review", "review-pr", "pre-review"]);
    }

    #[test]
    fn a_listed_command_is_named_where_the_backend_reads_it_and_nowhere_else() {
        let commands = [
            command("compact"),
            SlashCommand {
                mid_prompt: true,
                ..command("review")
            },
            command("server:prompt (MCP)"),
        ];
        assert_eq!(named("/compact now", true, &commands), vec![(0..8)]);
        assert_eq!(
            named("/compact now", false, &commands),
            [],
            "a later line is not where a command is read"
        );
        assert_eq!(named("then /compact", true, &commands), []);
        assert_eq!(
            named("use /review and /review", true, &commands),
            [4..11, 16..23]
        );
        assert_eq!(
            named("/server:prompt (MCP) go", true, &commands),
            vec![(0..20)]
        );
        for unnamed in [
            "/comp",
            "/compacted",
            "/usr/bin",
            "src/review",
            "/review/x",
            "see /etc",
        ] {
            assert_eq!(named(unnamed, true, &commands), [], "{unnamed}");
        }
        assert_eq!(
            named_in_prompt("/compact\n/compact and /review", &commands),
            [0..8, 22..29]
        );
    }
}
