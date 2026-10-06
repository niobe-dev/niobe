// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The palette the shell draws in.
//!
//! Every colour the drawing code uses is a field of the one [`Theme`] struct,
//! so that adding a theme is a table entry rather than a sweep through the
//! drawing code. [`THEMES`] is that table, and the order in it is the order
//! the View menu's theme list offers them in.
//!
//! Every theme has a table in the sixteen ANSI names, and it is what the shell
//! draws in unless the terminal says it can do better. Naming the sixteen
//! means the shell is legible over SSH, in `screen` and in a terminal with no
//! truecolor, and that a user's own sixteen-colour scheme is honoured. The
//! price is that a design's near-neighbours collapse: two shades a designer
//! distinguished by a few points of lightness have to become one of the
//! sixteen or a different one, and which of the two moves is a decision made
//! per theme and written down where it was made.
//!
//! `classic` is only that table, because the CGA palette it is drawn from is
//! exactly the sixteen. The designed themes — `neo`, `cyber`, `modern` — also
//! carry their design's own 24-bit values, and a terminal at
//! [`Depth::TrueColour`] draws those instead: their greens and magentas are
//! specific ones, and the sixteen-colour reduction is a different palette
//! rather than the same one approximated. On such a terminal those themes
//! override the user's scheme; `classic` never does.
//!
//! A theme also sets the line its panes are drawn in, because a design's
//! frame is part of its look and the focused pane's line is the one mark of
//! focus a monochrome terminal keeps.

use ratatui::style::Color;
use ratatui::widgets::BorderType;

/// How many colours the terminal can draw, which decides whether a designed
/// theme is drawn in its own 24-bit values or in its sixteen-colour table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Depth {
    /// The sixteen ANSI names, and nothing else. What a terminal is assumed to
    /// be until it says otherwise.
    #[default]
    Sixteen,
    /// 24-bit colour.
    TrueColour,
}

impl Depth {
    /// The depth the `COLORTERM` environment variable announces.
    ///
    /// `truecolor` and `24bit` are the two values terminals that draw 24-bit
    /// colour set; anything else, or nothing, is sixteen. The terminal is not
    /// asked directly: a query waits for a reply that a pipe or a terminal
    /// that does not understand it never sends, and a terminal that says
    /// nothing gets the sixteen, which are legible by construction.
    pub fn from_colorterm(colorterm: Option<&str>) -> Self {
        Self::from_env(colorterm, None)
    }

    /// The depth `COLORTERM` announces, unless `NO_COLOR` is set to anything
    /// but nothing: then the sixteen, whatever `COLORTERM` says. The sixteen
    /// are the operator's own terminal colours, which is the nearest a shell
    /// that marks what it draws by colour can come to using none of its own.
    pub fn from_env(colorterm: Option<&str>, no_color: Option<&str>) -> Self {
        if no_color.is_some_and(|value| !value.is_empty()) {
            return Depth::Sixteen;
        }
        Self::announced(colorterm)
    }

    fn announced(colorterm: Option<&str>) -> Self {
        match colorterm {
            Some(value)
                if value.eq_ignore_ascii_case("truecolor")
                    || value.eq_ignore_ascii_case("24bit") =>
            {
                Depth::TrueColour
            }
            _ => Depth::Sixteen,
        }
    }
}

/// Every colour the shell draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Theme {
    /// The name shown in the menu bar.
    pub name: &'static str,

    /// The line every pane but the focused one is drawn in.
    pub border: BorderType,
    /// The line the pane with the keyboard is drawn in. It differs from
    /// [`Theme::border`] in every theme, because it is the one mark of focus
    /// a terminal with no colour still draws.
    pub border_focus: BorderType,

    /// Behind the panes.
    pub pane_bg: Color,
    /// Pane borders.
    pub frame: Color,
    /// The border of the pane that has the keyboard. It is one of three
    /// marks the focused pane carries — its border is [`Theme::border_focus`]
    /// where the others are [`Theme::border`], and its title is inverted — so
    /// a terminal that collapses this colour into [`Theme::frame`] still shows
    /// which pane it is.
    pub frame_focus: Color,
    /// Pane titles.
    pub title: Color,
    /// Body text: the agent's replies, the brightest text in the transcript.
    pub fg: Color,
    /// Labels and anything secondary, and the machinery under the replies:
    /// what each tool call does, what a command printed, a sub-agent's words.
    pub dim: Color,
    /// The accent: hot keys, the session cost, the selected item.
    pub hot: Color,
    /// Something running low that has not run out: a plan's usage window past
    /// half gone. What has run out is drawn in [`Theme::del`], the theme's
    /// colour for an error, so the two read as one scale.
    pub warn: Color,

    /// The menu bar, and behind an open menu.
    pub menu_bg: Color,
    /// Text on the menu bar.
    pub menu_fg: Color,
    /// The line an open menu is framed in.
    pub menu_border: BorderType,
    /// An open menu's items.
    pub pulldown_fg: Color,
    /// An open menu's frame.
    pub pulldown_frame: Color,
    /// An item of an open menu that can do nothing in this session.
    pub pulldown_dim: Color,
    /// Behind the item an open menu has its cursor on, which Enter chooses.
    /// A field of its own rather than [`Theme::cursor_bg`], because a theme
    /// can draw an open menu in the colour it draws a dialog's cursor row in.
    pub menu_cursor_bg: Color,
    /// The text of the item an open menu has its cursor on.
    pub menu_cursor_fg: Color,
    /// Behind the F-key bar, and the gaps between its keys.
    pub keybar_bg: Color,
    /// An F-key's number, and the Esc in front of them.
    pub keybar_fg: Color,
    /// The label half of an F-key.
    pub fkey_bg: Color,
    /// Text on an F-key label.
    pub fkey_fg: Color,

    /// Added lines.
    pub add: Color,
    /// Removed lines, and errors.
    pub del: Color,
    /// Behind a diff in the transcript, so the block reads as a file's lines
    /// rather than as the session's prose.
    pub diff_bg: Color,
    /// Behind an added line of a diff.
    ///
    /// A design fills changed lines with a wash of the added or removed
    /// colour over the diff's background. A terminal has no alpha, so a wash
    /// is an opaque colour chosen per theme. A truecolor palette mixes it
    /// ([`wash`]); none of the sixteen names is a faint green or red — the
    /// plain ones are as loud as the text drawn on them — so every
    /// sixteen-colour table fills changed lines with [`Theme::diff_bg`] and
    /// leaves the change to the line's colour and its `+`/`-` sign, which is
    /// also what reads on a monochrome terminal.
    pub add_bg: Color,
    /// Behind a removed line of a diff; see [`Theme::add_bg`].
    pub del_bg: Color,

    /// The operator's own messages.
    pub user: Color,
    /// The assistant's messages.
    pub agent: Color,
    /// A tool call's glyph and name: an accent of their own, never the
    /// body text's colour, so a call does not read as a reply.
    pub tool: Color,
    /// Code in the assistant's replies: a code span, a code block.
    pub code: Color,
    /// A `/` command or skill the backend reads where a prompt names it, in
    /// the composer and in a sent prompt, so what will run reads apart from
    /// the words around it.
    pub command: Color,

    /// A dialog's face: the model list, drawn on a
    /// colour of their own so they read as a question laid over the panes
    /// rather than as one more pane.
    pub dialog_bg: Color,
    /// Text in a dialog.
    pub dialog_fg: Color,
    /// A dialog's border, and what its bottom edge says.
    pub dialog_frame: Color,
    /// The title on a dialog's top edge.
    pub dialog_title: Color,
    /// The row a dialog's list has its cursor on, which Enter chooses.
    pub cursor_bg: Color,
    /// The text of the row the cursor is on.
    pub cursor_fg: Color,
    /// Behind what a dialog's shadow falls on.
    pub shadow: Color,
    /// What a dialog's shadow falls on, redrawn in this colour with its own
    /// characters, so the text under it recedes and still reads. Where
    /// [`Theme::shadow`] is the panes' own colour, as on a black desk in
    /// sixteen colours, this dimming is all the shadow is; the dialog's edge
    /// is then told by its own face, which no theme draws in the panes'
    /// colour.
    pub shadow_fg: Color,
}

/// The DOS-blue theme: Far Manager's default colours.
///
/// The IBM CGA palette, which is exactly what the sixteen ANSI colours encode,
/// so this theme has no truecolor table: it is drawn in the user's own
/// sixteen at any depth. The darker navy a bar track and a diff background
/// would want is outside those sixteen, so they use [`Color::Black`] and
/// [`Color::Blue`].
///
/// Each role takes the colour Far gives the same role: light cyan text on
/// blue panels, black on cyan for the menu bar, the F-key labels and the
/// cursor bar, white on cyan for an open menu with its item under the cursor
/// white on black, white numbers on a black key bar, black on grey dialogs in
/// a white double frame, and a dark grey shadow on black.
///
/// Three of Far's choices are not taken. Far frames every panel in a double
/// line; here the panes are single and the one with the keyboard double,
/// because the line is the mark of focus a terminal without colour keeps.
/// Far draws its command line grey on black; the composer sits inside the
/// session pane's frame rather than on a line of its own under the panels, so
/// it keeps the pane's blue. And Far's red warning dialog has no counterpart:
/// no dialog here asks before something that cannot be undone.
pub const CLASSIC: Theme = Theme {
    name: "CLASSIC",
    border: BorderType::Plain,
    border_focus: BorderType::Double,

    pane_bg: Color::Blue,
    frame: Color::LightCyan,
    frame_focus: Color::White,
    title: Color::LightYellow,
    fg: Color::LightCyan,
    dim: Color::Gray,
    hot: Color::LightYellow,
    // The one yellow in the sixteen, so a window running low takes the
    // accent's colour here: there is no amber to give it one of its own.
    warn: Color::LightYellow,

    menu_bg: Color::Cyan,
    menu_fg: Color::Black,
    menu_border: BorderType::Double,
    pulldown_fg: Color::White,
    pulldown_frame: Color::White,
    pulldown_dim: Color::DarkGray,
    menu_cursor_bg: Color::Black,
    menu_cursor_fg: Color::White,
    keybar_bg: Color::Black,
    keybar_fg: Color::White,
    fkey_bg: Color::Cyan,
    fkey_fg: Color::Black,

    add: Color::LightGreen,
    del: Color::LightRed,
    // The navy a diff would sit on is not one of the sixteen, so the block
    // drops to black, which is what sets it off from the blue panes.
    diff_bg: Color::Black,
    add_bg: Color::Black,
    del_bg: Color::Black,

    user: Color::LightYellow,
    agent: Color::LightCyan,
    tool: Color::LightMagenta,
    // The body text is Far's light cyan, so code is the white Far's editor
    // keeps for what stands out of it.
    code: Color::White,
    command: Color::LightMagenta,

    dialog_bg: Color::Gray,
    dialog_fg: Color::Black,
    dialog_frame: Color::White,
    dialog_title: Color::Black,
    cursor_bg: Color::Cyan,
    cursor_fg: Color::Black,
    shadow: Color::Black,
    shadow_fg: Color::DarkGray,
};

/// Green on black: the terminal every film puts in front of a hacker.
///
/// This is the sixteen-colour table; [`NEO_TRUE`] is the design's own. Its
/// panes are framed as `classic`'s are.
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
    border: BorderType::Plain,
    border_focus: BorderType::Double,

    pane_bg: Color::Black,
    frame: Color::Green,
    frame_focus: Color::LightGreen,
    title: Color::LightGreen,
    fg: Color::LightGreen,
    dim: Color::Green,
    hot: Color::LightGreen,
    warn: Color::LightYellow,

    menu_bg: Color::Black,
    menu_fg: Color::Green,
    menu_border: BorderType::Plain,
    pulldown_fg: Color::Green,
    pulldown_frame: Color::Green,
    pulldown_dim: Color::Green,
    menu_cursor_bg: Color::LightGreen,
    menu_cursor_fg: Color::Black,
    keybar_bg: Color::Black,
    keybar_fg: Color::LightGreen,
    fkey_bg: Color::Green,
    fkey_fg: Color::Black,

    add: Color::LightGreen,
    del: Color::LightRed,
    diff_bg: Color::Black,
    add_bg: Color::Black,
    del_bg: Color::Black,

    user: Color::White,
    agent: Color::LightGreen,
    // Not the plain green: that is what is secondary, which is what a call's
    // own words are drawn in, and its name has to stand off them. Cyan is the
    // nearest of the sixteen that is not a green, and the plain one, so the
    // name stays quieter than the agent's bright green.
    tool: Color::Cyan,
    // Code stands out from the green prose the way a code span stands out in
    // a rendered page: by being the one thing that is not green.
    code: Color::White,
    command: Color::White,

    // Inverted, so the dialog stands off the black panes: black on green.
    // Nothing is darker than the panes' black, so the shadow only dims what
    // it falls on, as Far's black schemes do: a grey patch beside the dialog
    // would be lighter than the desk, which reads as a glow, not a shadow.
    dialog_bg: Color::Green,
    dialog_fg: Color::Black,
    dialog_frame: Color::Black,
    dialog_title: Color::Black,
    cursor_bg: Color::LightGreen,
    cursor_fg: Color::Black,
    shadow: Color::Black,
    shadow_fg: Color::DarkGray,
};

/// Green and magenta on near-black: the default.
///
/// This is the sixteen-colour table; [`CYBER_TRUE`] is the design's own.
///
/// The design is two accents on a black desk — a green that carries the
/// chrome and everything the session did, and a magenta that carries the
/// operator and the keys they can press. Both survive the reduction intact,
/// because both are on the bright half of the sixteen. What does not survive
/// is the design's third colour, a pale lavender for tool calls: there is no
/// lavender in sixteen colours, so a tool call takes the bright blue, which is
/// the only other hue on screen. Pane background and desk are one near-black
/// apart in the design and are both black here.
///
/// The design draws the frames in a dark green and the focused pane's in the
/// bright one, so the frames take the plain green of the sixteen and the
/// focused frame the bright green: the pane with the keyboard is the one that
/// lights up. Every pane is drawn in the design's single line, and the
/// focused one in a heavy single line, which is the nearest a cell comes to
/// the design's glow.
pub const CYBER: Theme = Theme {
    name: "CYBER",
    border: BorderType::Plain,
    border_focus: BorderType::Thick,

    pane_bg: Color::Black,
    frame: Color::Green,
    frame_focus: Color::LightGreen,
    title: Color::LightMagenta,
    fg: Color::White,
    dim: Color::Green,
    hot: Color::LightGreen,
    warn: Color::LightYellow,

    menu_bg: Color::Black,
    menu_fg: Color::White,
    menu_border: BorderType::Plain,
    pulldown_fg: Color::White,
    pulldown_frame: Color::White,
    pulldown_dim: Color::Green,
    menu_cursor_bg: Color::LightGreen,
    menu_cursor_fg: Color::Black,
    keybar_bg: Color::Black,
    keybar_fg: Color::LightGreen,
    fkey_bg: Color::LightMagenta,
    fkey_fg: Color::Black,

    add: Color::LightGreen,
    del: Color::LightRed,
    diff_bg: Color::Black,
    add_bg: Color::Black,
    del_bg: Color::Black,

    user: Color::LightMagenta,
    agent: Color::LightGreen,
    tool: Color::LightBlue,
    code: Color::LightCyan,
    command: Color::LightMagenta,

    // The dialog takes the magenta the chrome keeps for the operator, so the
    // list reads as the shell asking rather than as the session reporting.
    // Its cursor row goes back to the panes' green.
    dialog_bg: Color::Magenta,
    dialog_fg: Color::White,
    dialog_frame: Color::LightMagenta,
    dialog_title: Color::LightMagenta,
    cursor_bg: Color::LightGreen,
    cursor_fg: Color::Black,
    shadow: Color::Black,
    shadow_fg: Color::DarkGray,
};

/// The palette of a modern editor: grey chrome, a blue accent, and syntax
/// colours for what the session did.
///
/// The design's four greys collapse to two — frame, menu bar and dialog share
/// the dark one, secondary text takes the light one — which is enough,
/// because no two of those are ever drawn on each other.
///
/// The editor's blue bars are the one piece of the design that does not
/// survive. The menu row is where the model, the profile and what the session
/// is doing are told apart by colour, and on sixteen colours a blue bar leaves
/// only white legible on it: the accent and the assistant's own blue both
/// vanish into the background, and the strip goes monochrome. The row takes
/// the panes' black instead, which keeps every one of those readings, and the
/// blue stays where it is still an accent — the F-key labels, and everything
/// the shell wants the operator to look at.
///
/// This is the sixteen-colour table; [`MODERN_TRUE`] is the design's own. Its
/// panes are framed as `cyber`'s are.
pub const MODERN: Theme = Theme {
    name: "MODERN",
    border: BorderType::Plain,
    border_focus: BorderType::Thick,

    pane_bg: Color::Black,
    frame: Color::DarkGray,
    frame_focus: Color::LightBlue,
    title: Color::LightYellow,
    fg: Color::White,
    dim: Color::Gray,
    hot: Color::LightBlue,
    warn: Color::LightYellow,

    menu_bg: Color::DarkGray,
    menu_fg: Color::White,
    menu_border: BorderType::Plain,
    pulldown_fg: Color::White,
    pulldown_frame: Color::White,
    pulldown_dim: Color::Gray,
    menu_cursor_bg: Color::LightBlue,
    menu_cursor_fg: Color::Black,
    keybar_bg: Color::DarkGray,
    keybar_fg: Color::LightBlue,
    fkey_bg: Color::Blue,
    fkey_fg: Color::White,

    add: Color::Cyan,
    del: Color::LightRed,
    diff_bg: Color::Black,
    add_bg: Color::Black,
    del_bg: Color::Black,

    user: Color::LightYellow,
    agent: Color::LightBlue,
    tool: Color::LightMagenta,
    code: Color::LightCyan,
    command: Color::LightMagenta,

    dialog_bg: Color::DarkGray,
    dialog_fg: Color::White,
    dialog_frame: Color::Gray,
    dialog_title: Color::Gray,
    cursor_bg: Color::LightBlue,
    cursor_fg: Color::Black,
    shadow: Color::Black,
    shadow_fg: Color::DarkGray,
};

/// A colour written as the design writes it, `0xRRGGBB`.
const fn hex(rgb: u32) -> Color {
    let [_, r, g, b] = rgb.to_be_bytes();
    Color::Rgb(r, g, b)
}

/// `ink` laid over `paper` at `percent` opacity, as one opaque colour: the
/// wash a design puts behind a changed line, which a terminal cell has no
/// alpha to draw.
const fn wash(ink: u32, paper: u32, percent: u32) -> Color {
    let [_, ir, ig, ib] = ink.to_be_bytes();
    let [_, pr, pg, pb] = paper.to_be_bytes();
    Color::Rgb(
        mix(ir, pr, percent),
        mix(ig, pg, percent),
        mix(ib, pb, percent),
    )
}

/// One channel of [`wash`], rounded to the nearest.
const fn mix(ink: u8, paper: u8, percent: u32) -> u8 {
    let mixed = (ink as u32 * percent + paper as u32 * (100 - percent) + 50) / 100;
    // Both inputs are at most 255 and the weights sum to 100, so the mix is too.
    if mixed > 255 { 255 } else { mixed as u8 }
}

/// How much of the added or removed colour a changed line's wash carries:
/// enough to tell the line from its neighbours, little enough that the text
/// on it stays the loudest thing in the row.
const WASH: u32 = 14;

/// How much of the body text a cell under a dialog's shadow keeps, over the
/// shadow's black: about the grey Far redraws a shadowed cell in, tinted by
/// the theme's own text so the shade belongs to the design.
const SHADED: u32 = 40;

/// [`NEO`] as designed, for a terminal that can draw it.
///
/// The fields the design does not name take the nearest thing it does: code
/// in the yellow-green of its highlights, a dialog in its bar colour framed
/// in its bright green.
pub const NEO_TRUE: Theme = Theme {
    pane_bg: hex(0x020803),
    frame: hex(0x0f6b27),
    frame_focus: hex(0x00ff41),
    title: hex(0x00ff41),
    fg: hex(0x9bffb0),
    dim: hex(0x2e8b44),
    hot: hex(0x00ff41),
    // The amber of the other phosphor terminal: the one warm colour on a
    // green screen, short of the red that means it has run out.
    warn: hex(0xffb000),

    menu_bg: hex(0x001a08),
    menu_fg: hex(0x00ff41),
    pulldown_fg: hex(0x00ff41),
    pulldown_frame: hex(0x00ff41),
    pulldown_dim: hex(0x2e8b44),
    menu_cursor_bg: hex(0x00ff41),
    menu_cursor_fg: hex(0x000000),
    keybar_bg: hex(0x001a08),
    keybar_fg: hex(0x00ff41),
    fkey_bg: hex(0x003b12),
    fkey_fg: hex(0x00ff41),

    add: hex(0x00ff41),
    del: hex(0xff3b3b),
    diff_bg: hex(0x010f05),
    add_bg: wash(0x00ff41, 0x010f05, WASH),
    del_bg: wash(0xff3b3b, 0x010f05, WASH),

    user: hex(0xffffff),
    agent: hex(0x00ff41),
    tool: hex(0x7cff9e),
    code: hex(0xd4ff00),
    command: hex(0xffffff),

    dialog_bg: hex(0x0a2211),
    dialog_fg: hex(0x9bffb0),
    dialog_frame: hex(0x00ff41),
    dialog_title: hex(0x00ff41),
    cursor_bg: hex(0x00ff41),
    cursor_fg: hex(0x000000),
    shadow: hex(0x000000),
    shadow_fg: wash(0x9bffb0, 0x000000, SHADED),

    ..NEO
};

/// [`CYBER`] as designed, for a terminal that can draw it: the pale-blue body
/// text, the specific green and magenta, and the dark green frames the
/// sixteen-colour table has to give up.
///
/// A dialog takes the design's violet bar colour and a magenta frame, so it
/// still reads as the shell asking; code takes the design's yellow, the one
/// hue on screen nothing else uses.
pub const CYBER_TRUE: Theme = Theme {
    pane_bg: hex(0x0a0f0c),
    frame: hex(0x1c7a3f),
    frame_focus: hex(0x39ff7a),
    title: hex(0xff2bd6),
    fg: hex(0xe6e9ff),
    dim: hex(0x6b9a7c),
    hot: hex(0x39ff7a),
    // The design's yellow, which code is drawn in too: the two are never on
    // the same row.
    warn: hex(0xffd23f),

    menu_bg: hex(0x0d0b16),
    menu_fg: hex(0xe6e9ff),
    pulldown_fg: hex(0xe6e9ff),
    pulldown_frame: hex(0xe6e9ff),
    pulldown_dim: hex(0x6b9a7c),
    menu_cursor_bg: hex(0x39ff7a),
    menu_cursor_fg: hex(0x07060d),
    keybar_bg: hex(0x0d0b16),
    keybar_fg: hex(0x39ff7a),
    fkey_bg: hex(0xff2bd6),
    fkey_fg: hex(0x07060d),

    add: hex(0x39ff7a),
    del: hex(0xff4d6d),
    diff_bg: hex(0x080c0a),
    add_bg: wash(0x39ff7a, 0x080c0a, WASH),
    del_bg: wash(0xff4d6d, 0x080c0a, WASH),

    user: hex(0xff2bd6),
    agent: hex(0x39ff7a),
    tool: hex(0xb9c1ff),
    code: hex(0xffd23f),
    command: hex(0xff2bd6),

    dialog_bg: hex(0x16121f),
    dialog_fg: hex(0xe6e9ff),
    dialog_frame: hex(0xff2bd6),
    dialog_title: hex(0xff2bd6),
    cursor_bg: hex(0x39ff7a),
    cursor_fg: hex(0x07060d),
    shadow: hex(0x000000),
    shadow_fg: wash(0xe6e9ff, 0x000000, SHADED),

    ..CYBER
};

/// [`MODERN`] as designed, for a terminal that can draw it: an editor's greys,
/// its blue for focus and the accent, and its syntax colours for what the
/// session did. The blue that did not survive the sixteen is back on the
/// focused frame; the menu row keeps the design's grey.
pub const MODERN_TRUE: Theme = Theme {
    pane_bg: hex(0x252526),
    frame: hex(0x3c3c3c),
    frame_focus: hex(0x007acc),
    title: hex(0xdcdcaa),
    fg: hex(0xd4d4d4),
    dim: hex(0x858585),
    hot: hex(0x4fc1ff),
    // The editor's own warning colour, as its error colour is `del`.
    warn: hex(0xcca700),

    menu_bg: hex(0x323233),
    menu_fg: hex(0xcccccc),
    pulldown_fg: hex(0xcccccc),
    pulldown_frame: hex(0xcccccc),
    pulldown_dim: hex(0x858585),
    menu_cursor_bg: hex(0x04395e),
    menu_cursor_fg: hex(0xffffff),
    keybar_bg: hex(0x323233),
    keybar_fg: hex(0x4fc1ff),
    fkey_bg: hex(0x0e639c),
    fkey_fg: hex(0xffffff),

    add: hex(0x4ec9b0),
    del: hex(0xf14c4c),
    diff_bg: hex(0x1e1e1e),
    add_bg: wash(0x4ec9b0, 0x1e1e1e, WASH),
    del_bg: wash(0xf14c4c, 0x1e1e1e, WASH),

    user: hex(0xdcdcaa),
    agent: hex(0x4fc1ff),
    tool: hex(0xc586c0),
    code: hex(0x9cdcfe),
    command: hex(0xc586c0),

    dialog_bg: hex(0x323233),
    dialog_fg: hex(0xcccccc),
    dialog_frame: hex(0x007acc),
    dialog_title: hex(0x007acc),
    cursor_bg: hex(0x04395e),
    cursor_fg: hex(0xffffff),
    shadow: hex(0x000000),
    shadow_fg: wash(0xd4d4d4, 0x000000, SHADED),

    ..MODERN
};

/// Every theme there is, as its sixteen-colour table, in the order the theme
/// list offers them. The first is the default.
pub const THEMES: [Theme; 4] = [CYBER, CLASSIC, NEO, MODERN];

impl Theme {
    /// The theme `name` selects — `cyber`, `classic`, `neo`, `modern` — as its
    /// sixteen-colour table, or `None` where no theme is called that.
    ///
    /// Case is ignored: the name is written in a config file and on a command
    /// line, and the menu bar shows it in capitals.
    pub fn by_name(name: &str) -> Option<Self> {
        THEMES
            .into_iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(name))
    }

    /// This theme drawn at `depth`: its design's own values on a terminal
    /// that can draw them and the theme has them, its sixteen-colour table
    /// otherwise.
    pub fn at(self, depth: Depth) -> Self {
        let sixteen = Self::by_name(self.name).unwrap_or(self);
        match depth {
            Depth::Sixteen => sixteen,
            Depth::TrueColour => [NEO_TRUE, CYBER_TRUE, MODERN_TRUE]
                .into_iter()
                .find(|deep| deep.name == sixteen.name)
                .unwrap_or(sixteen),
        }
    }

    /// The next theme in the cycle, as its sixteen-colour table, wrapping at
    /// the end.
    ///
    /// A theme is found in the cycle by its name, so a palette drawn at any
    /// depth moves to the theme after it.
    pub fn next(self) -> Self {
        let at = THEMES.iter().position(|theme| theme.name == self.name);
        // A theme that is not in the table can only come from a caller that
        // built one by hand; cycling from it starts the cycle rather than
        // failing, because there is nothing for the operator to do about it.
        let next = at.map_or(0, |at| (at + 1) % THEMES.len());
        THEMES.get(next).copied().unwrap_or(CYBER)
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
        CYBER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every colour of one theme, so that a field added to [`Theme`] and left
    /// out of a check here is a field the checks below do not cover.
    fn colours(t: &Theme) -> [Color; 37] {
        [
            t.pane_bg,
            t.frame,
            t.frame_focus,
            t.title,
            t.fg,
            t.dim,
            t.hot,
            t.warn,
            t.menu_bg,
            t.menu_fg,
            t.pulldown_fg,
            t.pulldown_frame,
            t.pulldown_dim,
            t.menu_cursor_bg,
            t.menu_cursor_fg,
            t.keybar_bg,
            t.keybar_fg,
            t.fkey_bg,
            t.fkey_fg,
            t.add,
            t.del,
            t.diff_bg,
            t.add_bg,
            t.del_bg,
            t.user,
            t.agent,
            t.tool,
            t.code,
            t.command,
            t.dialog_bg,
            t.dialog_fg,
            t.dialog_frame,
            t.dialog_title,
            t.cursor_bg,
            t.cursor_fg,
            t.shadow,
            t.shadow_fg,
        ]
    }

    #[test]
    fn the_default_theme_is_cyber() {
        assert_eq!(Theme::default(), CYBER);
        assert_eq!(CYBER.name, "CYBER");
        assert_eq!(THEMES.first(), Some(&CYBER));
    }

    #[test]
    fn every_colour_of_every_sixteen_colour_table_is_one_of_the_sixteen_names() {
        // These are what a terminal that cannot draw 24-bit colour gets. A
        // truecolor value here would be drawn as an approximation, or not at
        // all, and would override a user's own scheme.
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

    /// Readability in sixteen colours is the whole reason every theme has a
    /// named table, and the way to lose it is to put a colour on top of
    /// itself. Every pairing the drawing code makes is checked, at both
    /// depths, including the one that is not a foreground on a background: an
    /// F-key's label block has to stand out from the bar it sits in.
    #[test]
    fn nothing_any_theme_draws_is_drawn_on_top_of_its_own_colour() {
        for t in every_palette() {
            for (on, over, what) in [
                (t.fg, t.pane_bg, "body text"),
                (t.dim, t.pane_bg, "secondary text"),
                (t.frame, t.pane_bg, "a pane border"),
                (t.frame_focus, t.pane_bg, "the focused pane's border"),
                // The focused pane's title is inverted: the pane background
                // on the title colour.
                (t.pane_bg, t.title, "the focused pane's title"),
                (t.title, t.pane_bg, "a pane title"),
                (t.hot, t.pane_bg, "the accent"),
                (t.warn, t.pane_bg, "a usage window running low"),
                (t.add, t.pane_bg, "an added line"),
                (t.del, t.pane_bg, "a removed line"),
                (t.fg, t.diff_bg, "a diff's unchanged line"),
                (t.dim, t.diff_bg, "a diff's line numbers"),
                (t.add, t.add_bg, "a diff's added line"),
                (t.del, t.del_bg, "a diff's removed line"),
                (t.user, t.pane_bg, "the operator's own message"),
                (t.agent, t.pane_bg, "an assistant message"),
                (t.tool, t.pane_bg, "a tool call"),
                (t.code, t.pane_bg, "code in a reply"),
                (t.command, t.pane_bg, "a command named in a prompt"),
                (t.menu_fg, t.menu_bg, "the menu bar"),
                (t.hot, t.menu_bg, "a menu hot key"),
                (t.pulldown_fg, t.menu_bg, "an open menu's item"),
                (t.pulldown_frame, t.menu_bg, "an open menu's frame"),
                (
                    t.pulldown_dim,
                    t.menu_bg,
                    "an open menu's item that can do nothing",
                ),
                (t.hot, t.menu_bg, "an open menu's hot key"),
                (t.menu_cursor_bg, t.menu_bg, "an open menu's cursor row"),
                (
                    t.menu_cursor_fg,
                    t.menu_cursor_bg,
                    "an open menu's cursor row's text",
                ),
                (t.keybar_fg, t.keybar_bg, "an F-key's number"),
                (t.fkey_fg, t.fkey_bg, "an F-key label"),
                (t.fkey_bg, t.keybar_bg, "an F-key label block"),
                // The menu row carries the session's identity beside the
                // menus, so what it says there is checked against the menu
                // background as well.
                (t.del, t.menu_bg, "an error count in the menu row"),
                (t.dialog_bg, t.pane_bg, "a dialog over the panes"),
                (t.dialog_fg, t.dialog_bg, "dialog text"),
                (t.dialog_frame, t.dialog_bg, "a dialog's border"),
                (t.dialog_title, t.dialog_bg, "a dialog's title"),
                (t.cursor_bg, t.dialog_bg, "a dialog's cursor row"),
                (t.cursor_fg, t.cursor_bg, "the cursor row's text"),
                // A question's selected answer is drawn inverted, in the pane
                // background on the title colour.
                (t.pane_bg, t.title, "a question's selected answer"),
                (t.shadow, t.dialog_bg, "a dialog's shadow"),
                (t.shadow_fg, t.shadow, "what a dialog's shadow falls on"),
            ] {
                assert_ne!(on, over, "{}: {what} is invisible", t.name);
            }
        }
    }

    /// The transcript colours a glyph by what produced the line, so two kinds
    /// the operator has to tell apart cannot share a colour.
    #[test]
    fn every_theme_tells_the_operator_their_own_messages_from_the_agents() {
        for t in every_palette() {
            assert_ne!(t.user, t.agent, "{}", t.name);
            assert_ne!(t.agent, t.del, "{}", t.name);
        }
    }

    /// The transcript has four voices — the operator, the agent, the calls
    /// it made and everything secondary — and a reader scrolling back tells
    /// them apart by colour alone, so no two share one. A tool's name is the
    /// machinery's accent and is never drawn as the reply's own text colour.
    #[test]
    fn every_theme_draws_the_transcripts_four_voices_in_four_colours() {
        for t in every_palette() {
            let voices = [
                ("user", t.user),
                ("agent", t.agent),
                ("tool", t.tool),
                ("dim", t.dim),
            ];
            for (at, (one, a)) in voices.iter().enumerate() {
                for (other, b) in &voices[at + 1..] {
                    assert_ne!(a, b, "{}: {one} and {other} are one colour", t.name);
                }
            }
            assert_ne!(t.tool, t.fg, "{}: a tool reads as a reply", t.name);
        }
    }

    /// On a deep terminal "not the reply's colour" is not enough: a tool's
    /// name a shade off the body text still reads as loud as it. It is
    /// drawn darker than the reply, so what the agent ran recedes behind
    /// what it said.
    #[test]
    fn every_truecolor_palette_draws_a_tool_darker_than_a_reply() {
        for theme in [CYBER, NEO, MODERN] {
            let t = theme.at(Depth::TrueColour);
            assert!(
                contrast(t.tool, t.pane_bg) < contrast(t.fg, t.pane_bg),
                "{}: a tool is as bright as a reply",
                t.name
            );
        }
    }

    /// The focused pane's border is brighter than the others'. The double
    /// border and the inverted title say which pane it is on their own; the
    /// colour is what makes it the first thing the eye finds.
    #[test]
    fn every_theme_draws_the_focused_pane_in_a_frame_of_its_own() {
        for t in every_palette() {
            assert_ne!(t.frame_focus, t.frame, "{}", t.name);
        }
    }

    /// Every theme in both depths it can be drawn in, which is what every
    /// check of a palette has to cover.
    fn every_palette() -> Vec<Theme> {
        THEMES
            .into_iter()
            .flat_map(|t| [t.at(Depth::Sixteen), t.at(Depth::TrueColour)])
            .collect()
    }

    #[test]
    fn classic_is_the_sixteen_colours_however_deep_the_terminal_is() {
        // CGA is the sixteen: a user's own scheme is honoured at any depth.
        assert_eq!(CLASSIC.at(Depth::TrueColour), CLASSIC);
        assert_eq!(CLASSIC.at(Depth::Sixteen), CLASSIC);
    }

    /// `classic` is Far Manager's default colour table, role for role, where
    /// the shell has the role: each pair here is the foreground and
    /// background Far draws that part of its screen in.
    #[test]
    fn classic_draws_each_part_of_the_screen_in_far_managers_colours() {
        let t = CLASSIC;
        for (ours, far, what) in [
            (
                (t.fg, t.pane_bg),
                (Color::LightCyan, Color::Blue),
                "panel text",
            ),
            (
                (t.frame, t.pane_bg),
                (Color::LightCyan, Color::Blue),
                "a panel's frame",
            ),
            (
                (t.menu_fg, t.menu_bg),
                (Color::Black, Color::Cyan),
                "the menu bar",
            ),
            (
                (t.hot, t.menu_bg),
                (Color::LightYellow, Color::Cyan),
                "a menu's hot key",
            ),
            (
                (t.pulldown_fg, t.menu_bg),
                (Color::White, Color::Cyan),
                "an open menu",
            ),
            (
                (t.pulldown_frame, t.menu_bg),
                (Color::White, Color::Cyan),
                "an open menu's frame",
            ),
            (
                (t.pulldown_dim, t.menu_bg),
                (Color::DarkGray, Color::Cyan),
                "a greyed item",
            ),
            (
                (t.menu_cursor_fg, t.menu_cursor_bg),
                (Color::White, Color::Black),
                "the selected item",
            ),
            (
                (t.keybar_fg, t.keybar_bg),
                (Color::White, Color::Black),
                "an F-key's number",
            ),
            (
                (t.fkey_fg, t.fkey_bg),
                (Color::Black, Color::Cyan),
                "an F-key's label",
            ),
            (
                (t.cursor_fg, t.cursor_bg),
                (Color::Black, Color::Cyan),
                "the cursor bar",
            ),
            (
                (t.dialog_fg, t.dialog_bg),
                (Color::Black, Color::Gray),
                "dialog text",
            ),
            (
                (t.dialog_frame, t.dialog_bg),
                (Color::White, Color::Gray),
                "a dialog's frame",
            ),
            (
                (t.dialog_title, t.dialog_bg),
                (Color::Black, Color::Gray),
                "a dialog's title",
            ),
            (
                (t.shadow_fg, t.shadow),
                (Color::DarkGray, Color::Black),
                "a shadow",
            ),
        ] {
            assert_eq!(ours, far, "{what}");
        }
        assert_eq!(t.menu_border, BorderType::Double, "an open menu's frame");
        assert_eq!(t.border_focus, BorderType::Double, "a dialog's frame");
    }

    #[test]
    fn the_designed_themes_draw_their_own_colours_where_the_terminal_can() {
        let cyber = CYBER.at(Depth::TrueColour);
        assert_eq!(cyber.name, "CYBER");
        assert_eq!(cyber.hot, Color::Rgb(0x39, 0xff, 0x7a));
        assert_eq!(cyber.title, Color::Rgb(0xff, 0x2b, 0xd6));
        assert_eq!(cyber.fg, Color::Rgb(0xe6, 0xe9, 0xff));
        assert_eq!(NEO.at(Depth::TrueColour).fg, Color::Rgb(0x9b, 0xff, 0xb0));
        assert_eq!(
            MODERN.at(Depth::TrueColour).frame_focus,
            Color::Rgb(0x00, 0x7a, 0xcc)
        );
        // And the way back is the sixteen-colour table itself.
        assert_eq!(cyber.at(Depth::Sixteen), CYBER);
    }

    #[test]
    fn a_designed_theme_on_a_deep_terminal_is_drawn_in_nothing_but_its_design() {
        for theme in [CYBER, NEO, MODERN] {
            let deep = theme.at(Depth::TrueColour);
            for colour in colours(&deep) {
                assert!(
                    matches!(colour, Color::Rgb(..)),
                    "{}: {colour:?} is a named colour in the truecolor palette, \
                     so the user's own scheme would repaint part of the design",
                    theme.name
                );
            }
        }
    }

    /// A 24-bit palette can put two colours a few points apart, so for it
    /// "not the same colour" is not enough: text has to stand off what it is
    /// drawn on by a contrast a reader can see. Three to one is the WCAG floor
    /// for large text; a terminal cell is small, but the check is against a
    /// palette choosing near-neighbours, not a certification.
    #[test]
    fn every_truecolor_palette_draws_its_text_legibly() {
        for theme in [CYBER, NEO, MODERN] {
            let t = theme.at(Depth::TrueColour);
            for (on, over, what) in [
                (t.fg, t.pane_bg, "body text"),
                (t.dim, t.pane_bg, "secondary text"),
                (t.title, t.pane_bg, "a pane title"),
                (t.hot, t.pane_bg, "the accent"),
                (t.warn, t.pane_bg, "a usage window running low"),
                (t.add, t.pane_bg, "an added line"),
                (t.del, t.pane_bg, "a removed line"),
                (t.user, t.pane_bg, "the operator's own message"),
                (t.agent, t.pane_bg, "an assistant message"),
                (t.tool, t.pane_bg, "a tool call"),
                (t.code, t.pane_bg, "code in a reply"),
                (t.command, t.pane_bg, "a command named in a prompt"),
                (t.fg, t.diff_bg, "a diff's unchanged line"),
                (t.add, t.add_bg, "a diff's added line"),
                (t.del, t.del_bg, "a diff's removed line"),
                (t.menu_fg, t.menu_bg, "the menu bar"),
                (t.hot, t.menu_bg, "a menu hot key"),
                (t.fkey_fg, t.fkey_bg, "an F-key label"),
                (t.keybar_fg, t.keybar_bg, "an F-key's number"),
                (t.pulldown_fg, t.menu_bg, "an open menu's item"),
                (
                    t.menu_cursor_fg,
                    t.menu_cursor_bg,
                    "an open menu's cursor row's text",
                ),
                (t.dialog_fg, t.dialog_bg, "dialog text"),
                (t.cursor_fg, t.cursor_bg, "the cursor row's text"),
            ] {
                let ratio = contrast(on, over);
                assert!(
                    ratio >= 3.0,
                    "{}: {what} is {ratio:.2} to 1 against its background",
                    t.name
                );
            }
        }
    }

    /// WCAG's contrast ratio of two 24-bit colours.
    fn contrast(a: Color, b: Color) -> f64 {
        let luminance = |colour: Color| {
            let Color::Rgb(r, g, b) = colour else {
                panic!("{colour:?} is not a 24-bit colour");
            };
            let channel = |c: u8| {
                let c = f64::from(c) / 255.0;
                if c <= 0.039_28 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The pairs the panes tell apart by colour as well as by a mark. The
    /// mark — a glyph, a sign, a line — is what a monochrome terminal keeps
    /// (`the_pane_with_the_keyboard_is_the_one_drawn_in_the_focus_line` and
    /// the glyph tests beside each pane prove that); these are the same
    /// distinctions checked in colour, at both depths, so no palette drops
    /// one of them to the mark alone.
    #[test]
    fn every_palette_keeps_the_distinctions_the_panes_rely_on() {
        for t in every_palette() {
            for (one, other, what) in [
                (t.frame_focus, t.frame, "focused and unfocused panes"),
                (t.add, t.del, "added and removed lines"),
                (t.hot, t.dim, "unpushed and pushed commits"),
                (t.agent, t.del, "a running agent and a failed one"),
                (t.tool, t.del, "a tool call and a failed one"),
                (t.fg, t.warn, "a usage window and one running low"),
                (t.command, t.fg, "a command named in a prompt and its words"),
                (t.warn, t.del, "a usage window running low and one run out"),
            ] {
                assert_ne!(one, other, "{}: {what} are one colour", t.name);
            }
        }
    }

    /// The border line is the mark of the pane with the keyboard that a
    /// monochrome terminal keeps, so no theme may draw it in the line the
    /// other panes are drawn in.
    #[test]
    fn every_theme_draws_the_focused_pane_in_a_line_of_its_own() {
        for t in every_palette() {
            assert_ne!(t.border_focus, t.border, "{}", t.name);
        }
    }

    #[test]
    fn the_border_is_the_designs_turbo_vision_doubles_and_editor_single_lines() {
        for t in [CLASSIC, NEO] {
            assert_eq!(
                (t.border, t.border_focus),
                (BorderType::Plain, BorderType::Double)
            );
        }
        for t in [CYBER, MODERN] {
            assert_eq!(
                (t.border, t.border_focus),
                (BorderType::Plain, BorderType::Thick)
            );
        }
    }

    #[test]
    fn cycling_from_a_truecolor_palette_moves_to_the_next_theme() {
        assert_eq!(CYBER.at(Depth::TrueColour).next(), CLASSIC);
        assert_eq!(MODERN.at(Depth::TrueColour).next(), CYBER);
    }

    #[test]
    fn a_terminal_is_deep_when_colorterm_says_so_and_sixteen_otherwise() {
        assert_eq!(Depth::from_colorterm(Some("truecolor")), Depth::TrueColour);
        assert_eq!(Depth::from_colorterm(Some("24bit")), Depth::TrueColour);
        assert_eq!(Depth::from_colorterm(Some("TrueColor")), Depth::TrueColour);
        assert_eq!(Depth::from_colorterm(Some("")), Depth::Sixteen);
        assert_eq!(Depth::from_colorterm(Some("yes")), Depth::Sixteen);
        assert_eq!(Depth::from_colorterm(None), Depth::Sixteen);
    }

    #[test]
    fn a_theme_is_selected_by_its_name_however_it_is_capitalised() {
        assert_eq!(Theme::by_name("classic"), Some(CLASSIC));
        assert_eq!(Theme::by_name("cyber"), Some(CYBER));
        assert_eq!(Theme::by_name("modern"), Some(MODERN));
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
        assert_eq!(listed(), "`cyber`, `classic`, `neo` or `modern`");
    }

    #[test]
    fn no_color_draws_the_sixteen_whatever_colorterm_says() {
        assert_eq!(Depth::from_env(Some("truecolor"), None), Depth::TrueColour);
        assert_eq!(
            Depth::from_env(Some("truecolor"), Some("1")),
            Depth::Sixteen
        );
        assert_eq!(Depth::from_env(Some("24bit"), Some("")), Depth::TrueColour);
        assert_eq!(Depth::from_env(None, Some("1")), Depth::Sixteen);
    }
}
