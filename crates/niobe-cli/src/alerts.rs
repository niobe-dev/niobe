// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! How the shell calls the operator back: which desktop notification the
//! terminal it runs in shows, read from the environment here because the
//! shell does not read the environment, and what the config leaves on.

use niobe_config::Notify;
use niobe_tui::{Alerts, Notification};

/// The notification the terminal named by `term_program` and `term` shows,
/// where it is one known to show one; inside tmux, none.
///
/// A terminal nobody recognises is sent no notification at all rather than
/// both forms: Ghostty and WezTerm answer both and would show it twice, and
/// ConEmu and Windows Terminal read an `OSC 9` as commands of their own.
/// tmux takes either sequence for its own pane rather than pass it on, and
/// sets `TERM_PROGRAM` to itself, which is how it is told apart; the
/// terminal it runs in is never asked.
pub fn notification_for(term_program: Option<&str>, term: Option<&str>) -> Option<Notification> {
    match (term_program, term) {
        (Some("tmux"), _) => None,
        (Some("iTerm.app" | "WezTerm" | "ghostty"), _) => Some(Notification::Osc9),
        (_, Some("xterm-kitty" | "xterm-ghostty")) => Some(Notification::Osc9),
        (_, Some(term)) if term == "foot" || term.starts_with("foot-") => {
            Some(Notification::Osc777)
        }
        (_, Some(term)) if term.starts_with("rxvt-unicode") => Some(Notification::Osc777),
        _ => None,
    }
}

/// What the shell sends to call the operator back: what `notify` leaves on,
/// of what the terminal shows.
pub fn alerts(notify: Notify, shown: Option<Notification>) -> Alerts {
    Alerts {
        bell: notify.bell(),
        notification: shown.filter(|_| notify.notification()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_that_shows_notifications_is_sent_the_form_it_reads() {
        assert_eq!(
            notification_for(Some("iTerm.app"), Some("xterm-256color")),
            Some(Notification::Osc9)
        );
        assert_eq!(
            notification_for(Some("WezTerm"), None),
            Some(Notification::Osc9)
        );
        assert_eq!(
            notification_for(None, Some("xterm-kitty")),
            Some(Notification::Osc9)
        );
        assert_eq!(
            notification_for(None, Some("foot-extra")),
            Some(Notification::Osc777)
        );
        assert_eq!(
            notification_for(None, Some("rxvt-unicode-256color")),
            Some(Notification::Osc777)
        );
    }

    #[test]
    fn tmux_and_a_terminal_nobody_named_are_sent_the_bell_alone() {
        assert_eq!(notification_for(Some("tmux"), Some("xterm-kitty")), None);
        assert_eq!(notification_for(Some("tmux"), Some("tmux-256color")), None);
        assert_eq!(
            notification_for(Some("Apple_Terminal"), Some("xterm-256color")),
            None
        );
        assert_eq!(notification_for(None, Some("xterm-256color")), None);
        assert_eq!(notification_for(None, None), None);
    }

    #[test]
    fn what_the_config_turns_off_is_not_sent() {
        let on = Notify::default();
        assert_eq!(
            alerts(on, Some(Notification::Osc9)),
            Alerts {
                bell: true,
                notification: Some(Notification::Osc9)
            }
        );
        assert_eq!(
            alerts(on, None),
            Alerts {
                bell: true,
                notification: None
            }
        );
        let off = niobe_config::Config::parse(
            "[notify]\nbell = false\nnotification = false\n",
            std::path::Path::new("/config.toml"),
        )
        .expect("the config is valid")
        .notify();
        assert_eq!(alerts(off, Some(Notification::Osc9)), Alerts::none());
    }
}
