// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The palette the shell draws in.
//!
//! Every colour the drawing code uses is a field of the one [`Theme`] struct,
//! so that adding a theme is a table entry rather than a sweep through the
//! drawing code. [`THEMES`] is that table, and the order in it is the order
//! `F9` cycles.
//!
//! Every colour is one of the sixteen ANSI names rather than a hex value.
//! Naming them means the shell looks the same over SSH, in `screen`, and in a
//! terminal with no truecolor — and it means a user's own sixteen-colour
//! scheme is honoured instead of overridden. The price is that a design's
//! near-neighbours collapse: two shades a designer distinguished by a few
//! points of lightness have to become one of the sixteen or a different one,
//! and which of the two moves is a decision made per theme and written down
//! where it was made.

use ratatui::style::Color;

/// Every colour the shell draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Theme {
    /// The name shown in the menu bar.
    pub name: &'static str,

    /// Behind the panes.
    pub pane_bg: Color,
    /// Pane borders.
    pub frame: Color,
    /// Pane titles.
    pub title: Color,
    /// Body text.
    pub fg: Color,
    /// Labels and anything secondary.
    pub dim: Color,
    /// The accent: hot keys, the session cost, the selected item.
    pub hot: Color,

    /// The menu bar, and the F-key bar's background.
    pub menu_bg: Color,
    /// Text on the menu bar.
    pub menu_fg: Color,
    /// The label half of an F-key.
    pub fkey_bg: Color,
    /// Text on an F-key label.
    pub fkey_fg: Color,

    /// The status line.
    pub status_bg: Color,
    /// Text on the status line.
    pub status_fg: Color,

    /// Added lines.
    pub add: Color,
    /// Removed lines, and errors.
    pub del: Color,

    /// The operator's own messages.
    pub user: Color,
    /// The assistant's messages.
    pub agent: Color,
    /// Tool calls.
    pub tool: Color,
    /// Code in the assistant's replies: a code span, a code block.
    pub code: Color,

    /// A dialog's face: the permission prompt and the model list, drawn on a
    /// colour of their own so they read as a question laid over the panes
    /// rather than as one more pane.
    pub dialog_bg: Color,
    /// Text in a dialog.
    pub dialog_fg: Color,
    /// A dialog's border and title.
    pub dialog_frame: Color,
    /// A button.
    pub button_bg: Color,
    /// A button's label.
    pub button_fg: Color,
    /// The button Enter presses.
    pub button_focus_bg: Color,
    /// The label of the button Enter presses.
    pub button_focus_fg: Color,
    /// The letter that presses a button from anywhere in the dialog.
    pub button_hot: Color,
    /// What a dialog and its buttons cast on what is under them.
    pub shadow: Color,
}

/// The DOS-blue theme, and the default.
///
/// The IBM CGA palette, which is exactly what the sixteen ANSI colours encode.
/// The darker navy a bar track and a diff background would want is outside
/// those sixteen, so they use [`Color::Black`] and [`Color::Blue`].
pub const CLASSIC: Theme = Theme {
    name: "CLASSIC",

    pane_bg: Color::Blue,
    frame: Color::LightCyan,
    title: Color::LightYellow,
    fg: Color::White,
    dim: Color::Gray,
    hot: Color::LightYellow,

    menu_bg: Color::Gray,
    menu_fg: Color::Black,
    fkey_bg: Color::Cyan,
    fkey_fg: Color::Black,

    status_bg: Color::Black,
    status_fg: Color::Gray,

    add: Color::LightGreen,
    del: Color::LightRed,

    user: Color::LightYellow,
    agent: Color::LightCyan,
    tool: Color::LightMagenta,
    code: Color::LightCyan,

    // Turbo Vision's: a grey dialog with a white frame, green buttons with
    // yellow hot letters, and a black shadow.
    dialog_bg: Color::Gray,
    dialog_fg: Color::Black,
    dialog_frame: Color::White,
    button_bg: Color::Green,
    button_fg: Color::Black,
    button_focus_bg: Color::LightGreen,
    button_focus_fg: Color::Black,
    button_hot: Color::Yellow,
    shadow: Color::Black,
};

/// Green on black: the terminal every film puts in front of a hacker.
///
/// Three of the design's greens do not survive the reduction to sixteen
/// colours — a pale mint body text, a saturated green for the chrome and a
/// deep green for what is secondary. Two of the sixteen are left to carry
/// them, so the body text keeps the bright one and the frame moves down to
/// the plain one: a Matrix terminal is bright text on black, and chrome that
/// recedes behind it is the right way round. `title` and `hot` stay bright
/// with it, which is what separates a pane's title from its border.
///
/// The F-key labels are the place the design's near-blacks collapse:
/// black on green rather than green on near-black, because a label the same
/// colour as the bar behind it is not a label.
pub const NEO: Theme = Theme {
    name: "NEO",

    pane_bg: Color::Black,
    frame: Color::Green,
    title: Color::LightGreen,
    fg: Color::LightGreen,
    dim: Color::Green,
    hot: Color::LightGreen,

    menu_bg: Color::Black,
    menu_fg: Color::Green,
    fkey_bg: Color::Green,
    fkey_fg: Color::Black,

    status_bg: Color::Black,
    status_fg: Color::Green,

    add: Color::LightGreen,
    del: Color::LightRed,

    user: Color::White,
    agent: Color::LightGreen,
    tool: Color::Green,
    // Code stands out from the green prose the way a code span stands out in
    // a rendered page: by being the one thing that is not green.
    code: Color::White,

    // Inverted, so the dialog stands off the black panes: black on green, with
    // the buttons back in the panes' green on black. Nothing is darker than the
    // panes' black, so the shadow is the grey the design keeps for chrome.
    dialog_bg: Color::Green,
    dialog_fg: Color::Black,
    dialog_frame: Color::Black,
    button_bg: Color::Black,
    button_fg: Color::LightGreen,
    button_focus_bg: Color::LightGreen,
    button_focus_fg: Color::Black,
    button_hot: Color::White,
    shadow: Color::DarkGray,
};

/// Every theme there is, in the order `F9` cycles them. The first is the
/// default.
pub const THEMES: [Theme; 2] = [CLASSIC, NEO];

impl Theme {
    /// The theme `name` selects — `classic`, `neo` — or `None` where no theme
    /// is called that.
    ///
    /// Case is ignored: the name is written in a config file and on a command
    /// line, and the menu bar shows it in capitals.
    pub fn by_name(name: &str) -> Option<Self> {
        THEMES
            .into_iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
    }

    /// The next theme in the cycle, wrapping at the end.
    pub fn next(self) -> Self {
        let at = THEMES.iter().position(|theme| *theme == self);
        // A theme that is not in the table can only come from a caller that
        // built one by hand; cycling from it starts the cycle rather than
        // failing, because there is nothing for the operator to do about it.
        let next = at.map_or(0, |at| (at + 1) % THEMES.len());
        THEMES.get(next).copied().unwrap_or(CLASSIC)
    }
}

/// The theme names as a config file and `--theme` spell them, in a sentence
/// that lists them all, so an unknown name is reported with the known ones.
pub fn listed() -> String {
    let names: Vec<String> = THEMES
        .iter()
        .map(|theme| format!("`{}`", theme.name.to_lowercase()))
        .collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

impl Default for Theme {
    fn default() -> Self {
        CLASSIC
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every colour of one theme, so that a field added to [`Theme`] and left
    /// out of a check here is a field the checks below do not cover.
    fn colours(t: &Theme) -> [Color; 27] {
        [
            t.pane_bg,
            t.frame,
            t.title,
            t.fg,
            t.dim,
            t.hot,
            t.menu_bg,
            t.menu_fg,
            t.fkey_bg,
            t.fkey_fg,
            t.status_bg,
            t.status_fg,
            t.add,
            t.del,
            t.user,
            t.agent,
            t.tool,
            t.code,
            t.dialog_bg,
            t.dialog_fg,
            t.dialog_frame,
            t.button_bg,
            t.button_fg,
            t.button_focus_bg,
            t.button_focus_fg,
            t.button_hot,
            t.shadow,
        ]
    }

    #[test]
    fn the_default_theme_is_classic() {
        assert_eq!(Theme::default(), CLASSIC);
        assert_eq!(CLASSIC.name, "CLASSIC");
        assert_eq!(THEMES.first(), Some(&CLASSIC));
    }

    #[test]
    fn every_colour_of_every_theme_is_one_of_the_sixteen_ansi_names() {
        // A truecolor value here would override a user's own scheme and would
        // degrade to an approximation on a terminal without it.
        for theme in THEMES {
            for colour in colours(&theme) {
                assert!(
                    !matches!(colour, Color::Rgb(..) | Color::Indexed(_)),
                    "{}: {colour:?} is not one of the sixteen named ANSI colours",
                    theme.name
                );
            }
        }
    }

    /// Readability in sixteen colours is the whole reason the palettes are
    /// named rather than written in hex, and the way to lose it is to put a
    /// colour on top of itself. Every pairing the drawing code makes is
    /// checked, including the one that is not a foreground on a background:
    /// an F-key's label block has to stand out from the bar it sits in.
    #[test]
    fn nothing_any_theme_draws_is_drawn_on_top_of_its_own_colour() {
        for t in THEMES {
            for (on, over, what) in [
                (t.fg, t.pane_bg, "body text"),
                (t.dim, t.pane_bg, "secondary text"),
                (t.frame, t.pane_bg, "a pane border"),
                (t.title, t.pane_bg, "a pane title"),
                (t.hot, t.pane_bg, "the accent"),
                (t.add, t.pane_bg, "an added line"),
                (t.del, t.pane_bg, "a removed line"),
                (t.user, t.pane_bg, "the operator's own message"),
                (t.agent, t.pane_bg, "an assistant message"),
                (t.tool, t.pane_bg, "a tool call"),
                (t.code, t.pane_bg, "code in a reply"),
                (t.menu_fg, t.menu_bg, "the menu bar"),
                (t.hot, t.menu_bg, "a menu hot key"),
                (t.fkey_fg, t.fkey_bg, "an F-key label"),
                (t.fkey_bg, t.menu_bg, "an F-key label block"),
                (t.status_fg, t.status_bg, "the status line"),
                (t.dialog_bg, t.pane_bg, "a dialog over the panes"),
                (t.dialog_fg, t.dialog_bg, "dialog text"),
                (t.dialog_frame, t.dialog_bg, "a dialog's border"),
                (t.button_bg, t.dialog_bg, "a button"),
                (t.button_fg, t.button_bg, "a button's label"),
                (t.button_hot, t.button_bg, "a button's hot letter"),
                (t.button_focus_bg, t.button_bg, "the focused button"),
                (
                    t.button_focus_fg,
                    t.button_focus_bg,
                    "the focused button's label",
                ),
                (t.shadow, t.dialog_bg, "a dialog's shadow"),
            ] {
                assert_ne!(on, over, "{}: {what} is invisible", t.name);
            }
        }
    }

    /// The transcript colours a glyph by what produced the line, so two kinds
    /// the operator has to tell apart cannot share a colour.
    #[test]
    fn every_theme_tells_the_operator_their_own_messages_from_the_agents() {
        for t in THEMES {
            assert_ne!(t.user, t.agent, "{}", t.name);
            assert_ne!(t.agent, t.del, "{}", t.name);
        }
    }

    #[test]
    fn a_theme_is_selected_by_its_name_however_it_is_capitalised() {
        assert_eq!(Theme::by_name("classic"), Some(CLASSIC));
        assert_eq!(Theme::by_name("neo"), Some(NEO));
        assert_eq!(Theme::by_name("NEO"), Some(NEO));
        assert_eq!(Theme::by_name("Neo"), Some(NEO));
        assert_eq!(Theme::by_name("matrix"), None);
        assert_eq!(Theme::by_name(""), None);
    }

    #[test]
    fn cycling_reaches_every_theme_and_comes_back_to_where_it_started() {
        let mut theme = Theme::default();
        let mut seen = vec![theme];
        for _ in 1..THEMES.len() {
            theme = theme.next();
            assert!(!seen.contains(&theme), "{} came round twice", theme.name);
            seen.push(theme);
        }
        assert_eq!(seen.len(), THEMES.len());
        assert_eq!(theme.next(), Theme::default());
    }

    #[test]
    fn the_names_are_listed_the_way_a_sentence_lists_them() {
        assert_eq!(listed(), "`classic` or `neo`");
    }
}
