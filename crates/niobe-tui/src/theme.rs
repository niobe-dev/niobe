// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The palette the shell draws in.
//!
//! Only `classic` exists. Every colour the drawing code uses is a field of the
//! one [`Theme`] struct, so that adding a theme is a table entry rather than a
//! sweep through the drawing code.
//!
//! `classic` is the IBM CGA palette, which is exactly what the sixteen ANSI
//! colours encode. Naming them rather than their hex values means the shell
//! looks the same over SSH, in `screen`, and in a terminal with no truecolor —
//! and it means a user's own sixteen-colour scheme is honoured instead of
//! overridden. The darker navy a bar track and a diff background would want is
//! outside those sixteen, so they use [`Color::Black`] and [`Color::Blue`].

use ratatui::style::Color;

/// Every colour the shell draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// The unfilled part of a bar.
    pub bar_bg: Color,
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
}

/// The DOS-blue theme, and the default.
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

    bar_bg: Color::Black,
    add: Color::LightGreen,
    del: Color::LightRed,

    user: Color::LightYellow,
    agent: Color::LightCyan,
    tool: Color::LightMagenta,
};

impl Default for Theme {
    fn default() -> Self {
        CLASSIC
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_theme_is_classic() {
        assert_eq!(Theme::default(), CLASSIC);
        assert_eq!(CLASSIC.name, "CLASSIC");
    }

    #[test]
    fn every_colour_is_one_of_the_sixteen_ansi_names() {
        // A truecolor value here would override a user's own scheme and would
        // degrade to an approximation on a terminal without it.
        let t = CLASSIC;
        for colour in [
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
            t.bar_bg,
            t.add,
            t.del,
            t.user,
            t.agent,
            t.tool,
        ] {
            assert!(
                !matches!(colour, Color::Rgb(..) | Color::Indexed(_)),
                "{colour:?} is not one of the sixteen named ANSI colours"
            );
        }
    }
}
