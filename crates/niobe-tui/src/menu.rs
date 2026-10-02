// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The menus and the F-key bar: everything the shell can be asked to do, and
//! the key that does each.
//!
//! The menus are the whole catalogue, each item with the key or command that
//! reaches it without the menu; the F-key bar is the ten used most. Both are
//! read from the tables here, so an action cannot be in the bar and missing
//! from the menus, and the menu cannot name a key the bar does something else
//! with.
//!
//! What is here is what each entry is called, where it is drawn and how the
//! cursor moves through an open menu. What an action does is
//! [`crate::app::App`]'s, because it is done to the session.

/// Something the operator can ask of the shell from a menu or an F-key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Says what this program is and which release it is.
    About,
    /// Shows what the session runs under and the config files it was read
    /// from.
    Settings,
    /// Shows the standing answers permission prompts are answered with.
    Permissions,
    /// Signing in or out, which is the backend's own business.
    SignIn,
    /// The backend's own health check.
    Doctor,
    /// Ends the session.
    Quit,
    /// Starts a conversation with empty context in the same session.
    NewSession,
    /// Has the backend summarise the conversation to free context.
    Compact,
    /// Carrying on an earlier session.
    Resume,
    /// Going back to an earlier point in the session.
    Rewind,
    /// Stops the turn that is running.
    Stop,
    /// Copies the transcript to the clipboard.
    Export,
    /// Opens the repository's instructions to the agent.
    Memory,
    /// Starts naming a file for the prompt.
    AddFile,
    /// The backend's report on its MCP servers.
    Mcp,
    /// Gives the keyboard to the pane the sub-agents are listed in.
    SubAgents,
    /// The backend's hooks.
    Hooks,
    /// Opens the list of models the profile names.
    SwitchModel,
    /// Moves the session to the next permission mode.
    CycleMode,
    /// Opens the list of effort levels.
    Effort,
    /// Says what the plan's windows stand at.
    Usage,
    /// Opens every cut diff, or cuts them again.
    Diff,
    /// Groups each turn's sub-agent rows under their agent, or interleaves
    /// them again.
    GroupByAgent,
    /// Shows a pane of the right-hand stack, or hides it.
    Pane(SidePane),
    /// Opens the list of themes.
    Theme,
    /// Lists the keys the shell answers to.
    Shortcuts,
    /// Starts a backend command in the prompt, with the list of them open.
    Commands,
    /// Opens the page each release is described on.
    ReleaseNotes,
    /// Opens the page an issue is filed on.
    ReportBug,
}

/// A pane of the right-hand stack, which the View menu shows and hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SidePane {
    /// The plan's windows, the tokens and the context.
    Usage,
    /// The working tree, the commits and the tests.
    Changes,
    /// The sub-agents and the tools.
    Activity,
}

/// One line of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// What it is called.
    pub label: &'static str,
    /// Which character of the label is its hot key, counted in characters.
    /// Not always the first: two items of one menu may start alike.
    pub hot: usize,
    /// The key or command that does the same without the menu, or nothing.
    pub keys: &'static str,
    /// What it does.
    pub action: Action,
    /// Whether a rule is drawn above it, closing off the group before.
    pub rule_above: bool,
}

impl Item {
    /// Its hot key, lower-cased.
    pub fn hot_key(&self) -> Option<char> {
        self.label
            .chars()
            .nth(self.hot)
            .map(|c| c.to_ascii_lowercase())
    }
}

/// One menu of the bar: its name, whose first letter opens it, and its
/// items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Menu {
    /// What the bar calls it.
    pub name: &'static str,
    /// What it holds, top to bottom.
    pub items: &'static [Item],
}

const fn item(label: &'static str, hot: usize, keys: &'static str, action: Action) -> Item {
    Item {
        label,
        hot,
        keys,
        action,
        rule_above: false,
    }
}

const fn ruled(label: &'static str, hot: usize, keys: &'static str, action: Action) -> Item {
    Item {
        rule_above: true,
        ..item(label, hot, keys, action)
    }
}

/// The menus, left to right. Each name's first letter opens it after Esc or
/// with Alt, and no two start alike.
///
/// A backend command is named by the command the backend runs it with: it is
/// what the menu sends, and what typing it at the start of a prompt does.
pub const MENUS: [Menu; 6] = [
    Menu {
        name: "Niobe",
        items: &[
            item("About", 0, "", Action::About),
            item("Settings", 0, "F9", Action::Settings),
            item("Permissions", 0, "", Action::Permissions),
            item("Login / Logout", 0, "", Action::SignIn),
            ruled("Doctor", 0, "/doctor", Action::Doctor),
            ruled("Quit", 0, "F10", Action::Quit),
        ],
    },
    Menu {
        name: "Session",
        items: &[
            item("New session", 0, "F3  /clear", Action::NewSession),
            item("Compact context", 0, "F2  /compact", Action::Compact),
            item("Resume…", 0, "", Action::Resume),
            ruled("Rewind to checkpoint", 2, "F6", Action::Rewind),
            item("Stop current run", 0, "Esc", Action::Stop),
            ruled("Export transcript", 0, "", Action::Export),
        ],
    },
    Menu {
        name: "Context",
        items: &[
            item("Memory (CLAUDE.md)", 0, "F8", Action::Memory),
            item("Add file…", 0, "@", Action::AddFile),
            ruled("MCP servers", 1, "F7  /mcp", Action::Mcp),
            item("Sub-agents", 0, "", Action::SubAgents),
            item("Hooks", 0, "", Action::Hooks),
        ],
    },
    Menu {
        name: "Model",
        items: &[
            item("Switch model…", 0, "F4", Action::SwitchModel),
            ruled("Next mode", 0, "⇧Tab", Action::CycleMode),
            item("Effort…", 0, "/effort", Action::Effort),
            ruled("Cost & usage", 0, "", Action::Usage),
        ],
    },
    Menu {
        name: "View",
        items: &[
            item("Diff", 0, "F5  Ctrl+T", Action::Diff),
            item("Group by agent", 0, "a", Action::GroupByAgent),
            item("Usage pane", 0, "", Action::Pane(SidePane::Usage)),
            item("Changes pane", 0, "", Action::Pane(SidePane::Changes)),
            item("Activity pane", 0, "", Action::Pane(SidePane::Activity)),
            ruled("Theme…", 0, "", Action::Theme),
        ],
    },
    Menu {
        name: "Help",
        items: &[
            item("Shortcuts", 0, "F1", Action::Shortcuts),
            item("Commands", 0, "//", Action::Commands),
            ruled("Release notes", 0, "", Action::ReleaseNotes),
            item("Report a bug", 9, "", Action::ReportBug),
        ],
    },
];

/// The F-key bar, F1 to F10: the digit that follows Esc for each, `0` being
/// F10, what the bar calls it and what it does.
pub const FKEYS: [(&str, &str, Action); 10] = [
    ("1", "Help", Action::Shortcuts),
    ("2", "Compact", Action::Compact),
    ("3", "Clear", Action::NewSession),
    ("4", "Model", Action::SwitchModel),
    ("5", "Diff", Action::Diff),
    ("6", "Rewind", Action::Rewind),
    ("7", "MCP", Action::Mcp),
    ("8", "Memory", Action::Memory),
    ("9", "Settings", Action::Settings),
    ("0", "Quit", Action::Quit),
];

/// What the F-key bar starts with: the key that stops a running turn, which
/// is also the key that makes each digit after it the F-key of that number.
///
/// The bar names Esc and a digit rather than F1 to F10 because on a Mac the
/// top row is media keys unless Fn is held or the system is set to send
/// function keys, so an F-key the bar named would not reach the shell on a
/// default setup. Esc and a digit reach every terminal as two plain bytes, and
/// the F-keys still work where they arrive.
pub const STOP: (&str, &str) = ("Esc", "Stop");

/// What F-key `n` does, F1 to F10.
pub fn fkey(n: u8) -> Option<Action> {
    let at = match n {
        10 => 9,
        n => usize::from(n).checked_sub(1)?,
    };
    FKEYS.get(at).map(|(_, _, action)| *action)
}

/// The menu a letter opens, by its name's first letter, ignoring case.
pub fn menu_of(letter: char) -> Option<usize> {
    let letter = letter.to_ascii_lowercase();
    MENUS.iter().position(|menu| {
        menu.name
            .chars()
            .next()
            .is_some_and(|first| first.to_ascii_lowercase() == letter)
    })
}

/// Where each menu's name is drawn on the bar: its first column and its
/// width, a column of space either side of the name included, so an open
/// menu's name is drawn as a block and a click a column off its name still
/// lands on it.
pub fn title_columns() -> [(u16, u16); MENUS.len()] {
    let mut columns = [(0, 0); MENUS.len()];
    let mut at: u16 = 0;
    for (slot, menu) in columns.iter_mut().zip(MENUS) {
        let width = u16::try_from(menu.name.chars().count() + 2).unwrap_or(u16::MAX);
        *slot = (at, width);
        at = at.saturating_add(width);
    }
    columns
}

/// The menu whose name is drawn at `column` of the bar.
pub fn title_at(column: u16) -> Option<usize> {
    title_columns()
        .iter()
        .position(|(start, width)| (*start..start.saturating_add(*width)).contains(&column))
}

/// Columns the Esc key and its label take at the start of the F-key bar, the
/// column after them included.
pub fn stop_columns() -> u16 {
    let (key, label) = STOP;
    u16::try_from(key.chars().count() + label.chars().count() + 1).unwrap_or(u16::MAX)
}

/// How wide each of the ten keys is drawn on an F-key bar `width` wide.
///
/// Each key is as wide as its digit, its label and a column after them, and
/// whatever the bar has over that is shared out evenly, the first keys taking
/// the columns that do not divide. Sized to their labels rather than in equal
/// slots, all ten are whole on the narrowest terminal the shell draws in,
/// where equal slots would cut the longest. Narrower than that, the keys
/// share what there is and their labels are cut.
pub fn fkey_widths(width: u16) -> [u16; FKEYS.len()] {
    let natural = FKEYS.map(|(digit, label, _)| {
        u16::try_from(digit.chars().count() + label.chars().count() + 1).unwrap_or(u16::MAX)
    });
    let room = width.saturating_sub(stop_columns());
    let count = u16::try_from(FKEYS.len()).unwrap_or(u16::MAX);
    let needed: u16 = natural.iter().sum();
    let Some(spare) = room.checked_sub(needed) else {
        return [room / count; FKEYS.len()];
    };
    let mut widths = natural;
    for (at, width) in (0u16..).zip(widths.iter_mut()) {
        *width += spare / count + u16::from(at < spare % count);
    }
    widths
}

/// What a click at `column` of an F-key bar `width` wide presses.
pub fn fkey_at(width: u16, column: u16) -> Option<Action> {
    let mut start = stop_columns();
    if column < start {
        return Some(Action::Stop);
    }
    for ((_, _, action), key) in FKEYS.iter().zip(fkey_widths(width)) {
        if column < start.saturating_add(key) {
            return Some(*action);
        }
        start = start.saturating_add(key);
    }
    None
}

/// An open menu, and the item its cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open {
    /// Which menu, by its place on the bar.
    pub menu: usize,
    /// Which of its items the cursor is on.
    pub item: usize,
}

impl Open {
    /// Menu `menu`, with the cursor on its first item.
    pub fn at(menu: usize) -> Self {
        Open {
            menu: menu.min(MENUS.len() - 1),
            item: 0,
        }
    }

    /// The menu that is open.
    pub fn menu(&self) -> &'static Menu {
        &MENUS[self.menu.min(MENUS.len() - 1)]
    }

    /// The item the cursor is on.
    pub fn item(&self) -> Option<&'static Item> {
        self.menu().items.get(self.item)
    }

    /// The menu to the left, or the last from the first.
    pub fn left(self) -> Self {
        Self::at((self.menu + MENUS.len() - 1) % MENUS.len())
    }

    /// The menu to the right, or the first from the last.
    pub fn right(self) -> Self {
        Self::at((self.menu + 1) % MENUS.len())
    }

    /// The cursor one item up, round to the last from the first.
    pub fn up(self) -> Self {
        let count = self.menu().items.len().max(1);
        Open {
            item: (self.item + count - 1) % count,
            ..self
        }
    }

    /// The cursor one item down, round to the first from the last.
    pub fn down(self) -> Self {
        let count = self.menu().items.len().max(1);
        Open {
            item: (self.item + 1) % count,
            ..self
        }
    }

    /// The cursor on the item whose hot key is `letter`, or where it was if
    /// none is.
    ///
    /// The letter moves the cursor and runs nothing: Enter does. A menu
    /// opens on Esc and a letter, and an operator who pressed Esc and went on
    /// typing a word would otherwise run whatever its letters named — a
    /// `/clear` of the conversation among them.
    pub fn to_letter(self, letter: char) -> Self {
        let letter = letter.to_ascii_lowercase();
        match self
            .menu()
            .items
            .iter()
            .position(|item| item.hot_key() == Some(letter))
        {
            Some(item) => Open { item, ..self },
            None => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_menus_open_on_the_same_letter() {
        let letters: Vec<char> = MENUS
            .iter()
            .filter_map(|menu| menu.name.chars().next())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let mut unique = letters.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), letters.len(), "{letters:?}");
    }

    #[test]
    fn no_two_items_of_a_menu_share_a_hot_key() {
        for menu in MENUS {
            let mut keys: Vec<char> = menu.items.iter().filter_map(Item::hot_key).collect();
            let count = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), count, "{}: {:?}", menu.name, menu.items);
        }
    }

    #[test]
    fn every_hot_key_is_a_letter_of_its_label() {
        for menu in MENUS {
            for item in menu.items {
                assert!(
                    item.hot_key().is_some_and(|c| c.is_alphanumeric()),
                    "{}: {}",
                    menu.name,
                    item.label
                );
            }
        }
    }

    #[test]
    fn every_f_key_is_in_a_menu_under_its_own_number() {
        for (n, (digit, label, action)) in (1u8..).zip(FKEYS) {
            let named = format!("F{n}");
            let item = MENUS
                .iter()
                .flat_map(|menu| menu.items)
                .find(|item| item.action == action)
                .unwrap_or_else(|| panic!("{label} is in no menu"));
            assert!(
                item.keys.split_whitespace().any(|key| key == named),
                "{} says {:?}, the bar has it as {digit} {label}",
                item.label,
                item.keys
            );
        }
    }

    #[test]
    fn no_menu_names_an_f_key_the_bar_gives_to_something_else() {
        for item in MENUS.iter().flat_map(|menu| menu.items) {
            for key in item.keys.split_whitespace() {
                if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
                    assert_eq!(fkey(n), Some(item.action), "{} names {key}", item.label);
                }
            }
        }
    }

    #[test]
    fn a_digit_and_its_f_key_are_the_same_key() {
        assert_eq!(fkey(1), Some(Action::Shortcuts));
        assert_eq!(fkey(10), Some(Action::Quit));
        assert_eq!(fkey(0), None);
        assert_eq!(fkey(11), None);
    }

    #[test]
    fn a_menus_letter_opens_it_whatever_its_case() {
        assert_eq!(menu_of('n'), Some(0));
        assert_eq!(menu_of('V'), Some(4));
        assert_eq!(menu_of('x'), None);
    }

    #[test]
    fn the_titles_sit_side_by_side_from_the_first_column() {
        let columns = title_columns();
        assert_eq!(columns[0], (0, 7), "\" Niobe \"");
        for pair in columns.windows(2) {
            assert_eq!(pair[0].0 + pair[0].1, pair[1].0);
        }
        assert_eq!(title_at(0), Some(0));
        assert_eq!(title_at(7), Some(1));
        assert_eq!(title_at(200), None);
    }

    #[test]
    fn the_cursor_walks_round_a_menu_and_the_bar() {
        let open = Open::at(0);
        assert_eq!(open.up().item, MENUS[0].items.len() - 1);
        assert_eq!(open.down().item, 1);
        assert_eq!(open.left().menu, MENUS.len() - 1);
        assert_eq!(open.right().menu, 1);
        assert_eq!(open.down().right().item, 0, "a new menu opens on its top");
    }

    #[test]
    fn a_letter_moves_the_cursor_to_its_item_and_runs_nothing() {
        let session = Open::at(1);
        let moved = session.to_letter('W');
        assert_eq!(moved.item().map(|item| item.action), Some(Action::Rewind));
        assert_eq!(session.to_letter('z'), session);
    }

    #[test]
    fn a_click_on_the_bar_presses_the_key_drawn_there() {
        let width = 120;
        let widths = fkey_widths(width);
        let lead = stop_columns();
        assert_eq!(fkey_at(width, 0), Some(Action::Stop));
        assert_eq!(fkey_at(width, lead), Some(Action::Shortcuts));
        let tenth = lead + widths[..9].iter().sum::<u16>();
        assert_eq!(fkey_at(width, tenth), Some(Action::Quit));
        assert_eq!(fkey_at(width, width), None);
    }

    #[test]
    fn every_key_is_whole_on_the_narrowest_terminal_and_the_bar_fills_any_width() {
        for width in [80, 81, 99, 120, 200] {
            let widths = fkey_widths(width);
            assert_eq!(
                stop_columns() + widths.iter().sum::<u16>(),
                width,
                "{width}"
            );
            for ((digit, label, _), key) in FKEYS.iter().zip(widths) {
                assert!(
                    usize::from(key) > digit.len() + label.len(),
                    "{digit}{label} is cut at {width} columns"
                );
            }
        }
    }
}
