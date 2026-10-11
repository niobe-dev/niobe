// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The keys an operator may rebind, and the chords they may rebind them to.
//!
//! The F-key bar names ten actions. Each has a key by default, F1 to F10, and
//! a terminal that eats one of them — a laptop's media row, a multiplexer's
//! own binding — leaves its action reachable only through a menu. A
//! [`Keymap`] is what a config says instead, validated here so that the file
//! that wrote it, and the shell that draws it, agree on what is allowed.
//!
//! A chord is an F-key or one of the few Ctrl letters nothing else in the
//! shell answers to. Anything else would shadow something the composer or the
//! shell already does with the key, and a binding that silently takes a key
//! from the composer is worse than one that is refused.

use std::fmt;

/// An action of the F-key bar, which a config may give another key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bindable {
    /// F1: the list of keys.
    Help,
    /// F2: have the backend summarise the conversation.
    Compact,
    /// F3: start over with empty context.
    Clear,
    /// F4: pick a model.
    Model,
    /// F5: open every cut diff.
    Diff,
    /// F6: go back to a checkpoint.
    Rewind,
    /// F7: the MCP servers.
    Mcp,
    /// F8: what the agent is told and remembers.
    Memory,
    /// F9: the profile and config files in force.
    Settings,
    /// F10: end the session.
    Quit,
}

impl Bindable {
    /// Every action, in the order the bar draws them.
    pub const ALL: [Self; 10] = [
        Self::Help,
        Self::Compact,
        Self::Clear,
        Self::Model,
        Self::Diff,
        Self::Rewind,
        Self::Mcp,
        Self::Memory,
        Self::Settings,
        Self::Quit,
    ];

    /// The name a config spells it by.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::Compact => "compact",
            Self::Clear => "clear",
            Self::Model => "model",
            Self::Diff => "diff",
            Self::Rewind => "rewind",
            Self::Mcp => "mcp",
            Self::Memory => "memory",
            Self::Settings => "settings",
            Self::Quit => "quit",
        }
    }

    /// The action a config means by `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }

    /// The number of the F-key it has by default, 1 to 10.
    pub const fn slot(self) -> u8 {
        match self {
            Self::Help => 1,
            Self::Compact => 2,
            Self::Clear => 3,
            Self::Model => 4,
            Self::Diff => 5,
            Self::Rewind => 6,
            Self::Mcp => 7,
            Self::Memory => 8,
            Self::Settings => 9,
            Self::Quit => 10,
        }
    }
}

/// A key an action may be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Chord {
    /// F1 to F12.
    F(u8),
    /// Ctrl and one of [`Chord::CTRL_LETTERS`].
    Ctrl(char),
}

impl Chord {
    /// The Ctrl letters nothing else answers to. The composer's editor takes
    /// most of the alphabet for editing, the shell takes find, history, fold,
    /// diff, paste, suspend and quit, and a terminal sends Ctrl+I, Ctrl+J and
    /// Ctrl+M as Tab and Enter: what is left is these.
    pub const CTRL_LETTERS: [char; 2] = ['l', 's'];

    /// The F-keys that may be bound.
    pub const F_KEYS: std::ops::RangeInclusive<u8> = 1..=12;

    /// The chord `text` spells: `F7`, or `ctrl+x`, in any case.
    ///
    /// # Errors
    /// Says what was wrong and what is allowed, in words a person editing a
    /// config reads.
    pub fn parse(text: &str) -> Result<Self, String> {
        let lowered = text.trim().to_ascii_lowercase();
        if let Some(number) = lowered.strip_prefix('f')
            && let Ok(n) = number.parse::<u8>()
            && Self::F_KEYS.contains(&n)
        {
            return Ok(Self::F(n));
        }
        if let Some(letter) = lowered.strip_prefix("ctrl+") {
            let mut chars = letter.chars();
            if let (Some(letter), None) = (chars.next(), chars.next())
                && Self::CTRL_LETTERS.contains(&letter)
            {
                return Ok(Self::Ctrl(letter));
            }
        }
        Err(format!(
            "`{text}` is not a key that can be bound; expected F1 to F12 or one of {}",
            Self::CTRL_LETTERS
                .iter()
                .map(|letter| format!("ctrl+{letter}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// What the F-key bar draws for it: `11`, or `^L`.
    pub fn short(self) -> String {
        match self {
            Self::F(n) => n.to_string(),
            Self::Ctrl(letter) => format!("^{}", letter.to_ascii_uppercase()),
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::F(n) => write!(f, "F{n}"),
            Self::Ctrl(letter) => write!(f, "ctrl+{letter}"),
        }
    }
}

/// Why a set of bindings was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapError {
    /// Two actions were given the same chord.
    Same {
        /// The chord.
        chord: Chord,
        /// The action named first.
        first: Bindable,
        /// The action named second.
        second: Bindable,
    },
    /// An action was given the F-key another action has by default, and that
    /// action was not moved off it.
    Takes {
        /// The chord.
        chord: Chord,
        /// The action that was bound to it.
        bound: Bindable,
        /// The action that has it by default.
        has: Bindable,
    },
}

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Same {
                chord,
                first,
                second,
            } => write!(
                f,
                "`{}` is already {}'s key; it cannot be `{}`'s too",
                chord,
                first.name(),
                second.name()
            ),
            Self::Takes { chord, bound, has } => write!(
                f,
                "`{chord}` is {}'s key; bind {} elsewhere too, or choose another key for {}",
                has.name(),
                has.name(),
                bound.name()
            ),
        }
    }
}

/// The bindings a config makes, each action at most once and each chord at
/// most once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: Vec<(Bindable, Chord)>,
}

impl Keymap {
    /// The bindings `bindings` make, in the order given.
    ///
    /// # Errors
    /// The first binding that gives a chord to two actions, or takes the
    /// F-key another action has by default without that action being moved.
    /// An action named twice has its later binding.
    pub fn new(bindings: impl IntoIterator<Item = (Bindable, Chord)>) -> Result<Self, KeymapError> {
        let mut kept: Vec<(Bindable, Chord)> = Vec::new();
        for (action, chord) in bindings {
            kept.retain(|(had, _)| *had != action);
            if let Some((first, _)) = kept.iter().find(|(_, taken)| *taken == chord) {
                return Err(KeymapError::Same {
                    chord,
                    first: *first,
                    second: action,
                });
            }
            kept.push((action, chord));
        }
        for (action, chord) in &kept {
            let Chord::F(n) = chord else { continue };
            let Some(has) = Bindable::ALL.into_iter().find(|owner| owner.slot() == *n) else {
                continue;
            };
            if has != *action && !kept.iter().any(|(moved, _)| *moved == has) {
                return Err(KeymapError::Takes {
                    chord: *chord,
                    bound: *action,
                    has,
                });
            }
        }
        Ok(Self { bindings: kept })
    }

    /// Whether no action has been rebound.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The chord `action` was given, where it was given one.
    pub fn chord_of(&self, action: Bindable) -> Option<Chord> {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == action)
            .map(|(_, chord)| *chord)
    }

    /// The action `chord` was given to, where it was.
    pub fn action_of(&self, chord: Chord) -> Option<Bindable> {
        self.bindings
            .iter()
            .find(|(_, bound)| *bound == chord)
            .map(|(action, _)| *action)
    }

    /// Every binding, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = (Bindable, Chord)> + '_ {
        self.bindings.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_is_an_f_key_or_a_free_ctrl_letter_in_any_case() {
        assert_eq!(Chord::parse("F7"), Ok(Chord::F(7)));
        assert_eq!(Chord::parse("f12"), Ok(Chord::F(12)));
        assert_eq!(Chord::parse(" Ctrl+S "), Ok(Chord::Ctrl('s')));
        assert_eq!(Chord::F(11).to_string(), "F11");
        assert_eq!(Chord::Ctrl('l').to_string(), "ctrl+l");
        assert_eq!(Chord::Ctrl('l').short(), "^L");
        assert_eq!(Chord::F(11).short(), "11");
    }

    #[test]
    fn a_key_something_else_answers_to_is_refused_with_what_is_allowed() {
        for text in [
            "ctrl+c", "ctrl+x", "ctrl+a", "F13", "F0", "alt+x", "enter", "", "ctrl+", "x",
        ] {
            let said = Chord::parse(text).expect_err(text);
            assert!(said.contains("F1 to F12"), "{text}: {said}");
            assert!(said.contains("ctrl+l, ctrl+s"), "{text}: {said}");
        }
    }

    #[test]
    fn every_action_has_a_name_and_the_slot_the_bar_gives_it() {
        for (at, action) in (1u8..).zip(Bindable::ALL) {
            assert_eq!(action.slot(), at);
            assert_eq!(Bindable::from_name(action.name()), Some(action));
        }
        assert_eq!(Bindable::from_name("nope"), None);
    }

    #[test]
    fn a_chord_given_to_two_actions_is_refused_naming_both() {
        let error = Keymap::new([
            (Bindable::Mcp, Chord::F(11)),
            (Bindable::Memory, Chord::F(11)),
        ])
        .expect_err("one key cannot be two actions");

        assert_eq!(
            error,
            KeymapError::Same {
                chord: Chord::F(11),
                first: Bindable::Mcp,
                second: Bindable::Memory
            }
        );
        assert!(error.to_string().contains("mcp"), "{error}");
        assert!(error.to_string().contains("memory"), "{error}");
    }

    #[test]
    fn an_f_key_another_action_has_is_taken_only_by_moving_that_action() {
        let taken = Keymap::new([(Bindable::Mcp, Chord::F(8))]).expect_err("memory has F8");
        assert_eq!(
            taken,
            KeymapError::Takes {
                chord: Chord::F(8),
                bound: Bindable::Mcp,
                has: Bindable::Memory
            }
        );

        let swapped = Keymap::new([
            (Bindable::Mcp, Chord::F(8)),
            (Bindable::Memory, Chord::F(7)),
        ])
        .expect("the two moved together");
        assert_eq!(swapped.chord_of(Bindable::Mcp), Some(Chord::F(8)));
        assert_eq!(swapped.action_of(Chord::F(7)), Some(Bindable::Memory));

        let own = Keymap::new([(Bindable::Mcp, Chord::F(7))]).expect("its own key is fine");
        assert_eq!(own.chord_of(Bindable::Mcp), Some(Chord::F(7)));
    }

    #[test]
    fn an_action_named_twice_has_its_later_binding() {
        let keymap = Keymap::new([
            (Bindable::Mcp, Chord::F(11)),
            (Bindable::Mcp, Chord::Ctrl('s')),
        ])
        .expect("valid");

        assert_eq!(keymap.chord_of(Bindable::Mcp), Some(Chord::Ctrl('s')));
        assert_eq!(keymap.iter().count(), 1);
    }
}
