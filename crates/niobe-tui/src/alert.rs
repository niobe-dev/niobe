// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How the shell gets the operator back when they have looked away: the
//! terminal's bell, and a desktop notification where the terminal shows one,
//! when the agent is waiting on them and when a turn they sent has ended.
//!
//! Which notification a terminal answers to is the binary's to work out from
//! the environment it was started in; the shell is handed the answer, and a
//! terminal nobody named is sent the bell alone. An escape sequence a terminal
//! does not know is one it should ignore, but not every one does, and a
//! notification shown twice — one terminal answering both forms — is worse
//! than none.
//!
//! tmux takes these sequences for its own pane and does not pass them on to
//! the terminal it runs in; the bell it does pass on, as a bell in the pane.

/// Why the shell is calling the operator back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// A question is on screen that only the operator can answer: a
    /// permission prompt no standing rule covers.
    NeedsYou,
    /// A turn the operator sent from this shell has ended, and did not end
    /// because they stopped it.
    TurnEnded,
}

/// The escape sequence a terminal shows a desktop notification for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notification {
    /// `OSC 9`, which iTerm2 started and WezTerm, Ghostty and kitty answer
    /// too.
    Osc9,
    /// `OSC 777;notify`, which foot and rxvt-unicode answer.
    Osc777,
}

/// What the shell sends when it calls the operator back. [`Alerts::none`],
/// where a shell is built, sends nothing; the binary hands in what the config
/// leaves on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Alerts {
    /// Whether the bell rings.
    pub bell: bool,
    /// The notification the terminal shows, where it shows one and the config
    /// has not turned it off.
    pub notification: Option<Notification>,
}

impl Alerts {
    /// Nothing is sent: a shell nobody asked to call anyone back.
    pub fn none() -> Self {
        Self::default()
    }

    /// The bytes that call the operator back for `alert`, in a session in
    /// the repository called `repo`; empty where nothing is sent.
    ///
    /// The repository's name is the directory the session started in, which
    /// whoever named the clone chose, so it is kept to the characters that
    /// draw and never ends the sequence it sits in.
    pub fn sequence(&self, alert: Alert, repo: &str) -> String {
        let mut bytes = String::new();
        if let Some(notification) = self.notification {
            let what = match alert {
                Alert::NeedsYou => "waiting on you",
                Alert::TurnEnded => "turn ended",
            };
            let repo = printable(repo);
            let title = match repo.is_empty() {
                true => "niobe".to_owned(),
                false => format!("niobe · {repo}"),
            };
            match notification {
                Notification::Osc9 => bytes.push_str(&format!("\x1b]9;{title}: {what}\x07")),
                Notification::Osc777 => {
                    bytes.push_str(&format!("\x1b]777;notify;{title};{what}\x07"));
                }
            }
        }
        if self.bell {
            bytes.push('\x07');
        }
        bytes
    }
}

/// `text` without the characters that would act on the terminal rather than
/// show, or end an `OSC 777` field early.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && *c != ';')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_sent_by_a_shell_nobody_asked_to_call_anyone_back() {
        assert_eq!(Alerts::none().sequence(Alert::NeedsYou, "niobe"), "");
        assert_eq!(Alerts::none().sequence(Alert::TurnEnded, "niobe"), "");
    }

    #[test]
    fn the_bell_alone_is_one_bel() {
        let bell = Alerts {
            bell: true,
            notification: None,
        };
        assert_eq!(bell.sequence(Alert::NeedsYou, "niobe"), "\x07");
        assert_eq!(bell.sequence(Alert::TurnEnded, "niobe"), "\x07");
    }

    #[test]
    fn each_notification_says_which_repository_and_why() {
        let osc9 = Alerts {
            bell: false,
            notification: Some(Notification::Osc9),
        };
        assert_eq!(
            osc9.sequence(Alert::NeedsYou, "niobe"),
            "\x1b]9;niobe · niobe: waiting on you\x07"
        );
        let osc777 = Alerts {
            bell: true,
            notification: Some(Notification::Osc777),
        };
        assert_eq!(
            osc777.sequence(Alert::TurnEnded, "shop"),
            "\x1b]777;notify;niobe · shop;turn ended\x07\x07"
        );
    }

    #[test]
    fn a_repository_name_cannot_end_the_notification_or_act_on_the_terminal() {
        let osc777 = Alerts {
            bell: false,
            notification: Some(Notification::Osc777),
        };
        assert_eq!(
            osc777.sequence(Alert::NeedsYou, "a\x07b\x1b]0;x;c\u{9c}"),
            "\x1b]777;notify;niobe · ab]0xc;waiting on you\x07"
        );
        assert_eq!(
            osc777.sequence(Alert::NeedsYou, "\x1b"),
            "\x1b]777;notify;niobe;waiting on you\x07"
        );
    }
}
