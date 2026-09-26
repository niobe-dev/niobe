// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Reading what the terminal sends for keys and the mouse into events.
//!
//! crossterm reads these itself, and does so everywhere the shell does not
//! read the terminal on its own (see [`crate::input`]). Where it does, the bytes
//! are read here instead, for one form crossterm 0.29 has no arm for: xterm's
//! modifyOtherKeys, `CSI 27 ; <modifiers> ; <key> ~`. It is how tmux reports
//! Shift+Enter under `extended-keys`, and how xterm-like terminals the kitty
//! query goes unanswered on report it once asked, and crossterm reads it as a
//! parse error and drops it — Shift+Enter then does nothing at all. crossterm
//! cannot be handed bytes it did not read, so the choice is between reading
//! every sequence here and carrying a fork of it; this is the smaller of the
//! two to keep right.
//!
//! Everything else is read as crossterm reads it, sequence for sequence, so
//! that the shell's keys mean the same on either path: the same modifiers from
//! the same masks, a lone Esc at the end of a read being the Esc key, and a
//! sequence nobody recognises passed over whole rather than typed out.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

/// The longest sequence waited out before the bytes are taken to be none the
/// shell reads. Every sequence a terminal sends for a key or a click is far
/// shorter; this only bounds what a stream of garbage can hold back.
const LONGEST_SEQUENCE: usize = 64;

/// What the bytes read so far are.
#[derive(Debug, PartialEq)]
enum Step {
    /// A key or a click.
    Event(Event),
    /// A whole sequence that is no key: a reply to a query, focus, a paste
    /// marker, or one this reader does not know.
    Skip,
    /// The start of a sequence whose end has not been read yet.
    Wait,
}

/// Turns the bytes read from the terminal into events, holding a sequence
/// that one read cut short until the next read finishes it.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    /// Reads `bytes` into `events`.
    ///
    /// `more` says the read that produced them may have left bytes behind: an
    /// Esc at the very end of a read is the Esc key only when nothing else is
    /// on its way.
    pub(crate) fn feed(&mut self, bytes: &[u8], more: bool, events: &mut Vec<Event>) {
        for (index, byte) in bytes.iter().enumerate() {
            self.pending.push(*byte);
            let more = more || index + 1 < bytes.len();
            match step(&self.pending, more) {
                Step::Event(event) => {
                    events.push(event);
                    self.pending.clear();
                }
                Step::Skip => self.pending.clear(),
                Step::Wait if self.pending.len() > LONGEST_SEQUENCE => self.pending.clear(),
                Step::Wait => {}
            }
        }
    }
}

/// Reads one event off the front of `buffer`, which holds exactly the bytes
/// since the last event.
fn step(buffer: &[u8], more: bool) -> Step {
    match buffer {
        [] => Step::Wait,
        [0x1b] if more => Step::Wait,
        [0x1b] => key(KeyCode::Esc, KeyModifiers::NONE),
        [0x1b, b'O'] => Step::Wait,
        [0x1b, b'O', last, ..] => ss3(*last),
        [0x1b, b'[', ..] => csi(buffer),
        [0x1b, 0x1b, ..] => key(KeyCode::Esc, KeyModifiers::NONE),
        [0x1b, rest @ ..] => alt(step(rest, more)),
        [b'\r', ..] => key(KeyCode::Enter, KeyModifiers::NONE),
        // Raw mode is on whenever these bytes are read, and a line feed is
        // then the key Ctrl+J sends rather than the end of a line.
        [b'\n', ..] => key(KeyCode::Char('j'), KeyModifiers::CONTROL),
        [b'\t', ..] => key(KeyCode::Tab, KeyModifiers::NONE),
        [0x7f, ..] => key(KeyCode::Backspace, KeyModifiers::NONE),
        [control @ 0x01..=0x1a, ..] => key(
            KeyCode::Char(char::from(control - 0x01 + b'a')),
            KeyModifiers::CONTROL,
        ),
        [control @ 0x1c..=0x1f, ..] => key(
            KeyCode::Char(char::from(control - 0x1c + b'4')),
            KeyModifiers::CONTROL,
        ),
        [0x00, ..] => key(KeyCode::Char(' '), KeyModifiers::CONTROL),
        _ => typed(buffer),
    }
}

/// A key pressed with nothing but `modifiers`.
fn key(code: KeyCode, modifiers: KeyModifiers) -> Step {
    Step::Event(Event::Key(KeyEvent::new(code, modifiers)))
}

/// What an Esc in front of a key makes of it: the same key with Alt held.
fn alt(inner: Step) -> Step {
    match inner {
        Step::Event(Event::Key(mut key)) => {
            key.modifiers |= KeyModifiers::ALT;
            Step::Event(Event::Key(key))
        }
        other => other,
    }
}

/// A character typed, once all of its UTF-8 bytes are in. An upper-case letter
/// is read as typed with Shift, as crossterm reads it.
fn typed(buffer: &[u8]) -> Step {
    match std::str::from_utf8(buffer) {
        Ok(text) => match text.chars().next() {
            Some(c) if c.is_uppercase() => key(KeyCode::Char(c), KeyModifiers::SHIFT),
            Some(c) => key(KeyCode::Char(c), KeyModifiers::NONE),
            None => Step::Wait,
        },
        Err(error) if error.error_len().is_none() => Step::Wait,
        Err(_) => Step::Skip,
    }
}

/// `ESC O <final>`: the arrows, Home, End and F1–F4 in application mode.
fn ss3(last: u8) -> Step {
    let code = match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P'..=b'S' => KeyCode::F(1 + last - b'P'),
        _ => return Step::Skip,
    };
    key(code, KeyModifiers::NONE)
}

/// `ESC [ …`, once it is whole: parameter and intermediate bytes up to one
/// final byte.
fn csi(buffer: &[u8]) -> Step {
    match &buffer[2..] {
        [] => Step::Wait,
        // The Linux console's F1–F5.
        [b'['] => Step::Wait,
        [b'[', last @ b'A'..=b'E', ..] => key(KeyCode::F(1 + last - b'A'), KeyModifiers::NONE),
        [b'[', ..] => Step::Skip,
        // X10 mouse: three raw bytes after the `M`, which may be anything.
        [b'M', rest @ ..] if rest.len() < 3 => Step::Wait,
        [b'M', button, column, row, ..] => x10_mouse(*button, *column, *row),
        [.., last] => match last {
            0x20..=0x3f => Step::Wait,
            0x40..=0x7e => sequence(&buffer[2..buffer.len() - 1], *last),
            _ => Step::Skip,
        },
    }
}

/// A whole CSI sequence: its parameters and its final byte.
fn sequence(parameters: &[u8], last: u8) -> Step {
    let Ok(parameters) = std::str::from_utf8(parameters) else {
        return Step::Skip;
    };
    if let Some(mouse) = parameters.strip_prefix('<') {
        return sgr_mouse(mouse, last == b'm');
    }
    // A reply to a query — the keyboard flags, the device attributes — which
    // is no key; so is focus, which the shell never asks for.
    if parameters.starts_with('?') {
        return Step::Skip;
    }
    match last {
        b'u' => csi_u(parameters),
        b'~' => tilde(parameters),
        b'Z' => key(KeyCode::BackTab, KeyModifiers::SHIFT),
        b'A' | b'B' | b'C' | b'D' | b'F' | b'H' | b'P' | b'Q' | b'R' | b'S' => {
            modified(parameters, last)
        }
        _ => Step::Skip,
    }
}

/// The arrows, Home, End and F1–F4 with modifiers: `1;<modifiers>` before the
/// final byte.
fn modified(parameters: &str, last: u8) -> Step {
    let code = match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'F' => KeyCode::End,
        b'H' => KeyCode::Home,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Step::Skip,
    };
    let mut fields = parameters.split(';');
    fields.next();
    let (modifiers, kind) = match fields.next() {
        Some(field) => match modifiers_and_kind(field) {
            Some(parsed) => parsed,
            None => return Step::Skip,
        },
        // An older form puts the modifiers alone in place of the `1`.
        None => match parameters.bytes().last() {
            Some(digit @ b'0'..=b'9') => (modifiers(digit - b'0'), KeyEventKind::Press),
            Some(_) => return Step::Skip,
            None => (KeyModifiers::NONE, KeyEventKind::Press),
        },
    };
    Step::Event(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)))
}

/// `<number> ; <modifiers> ~`: the editing keys and F5 up — and modifyOtherKeys,
/// `27 ; <modifiers> ; <key> ~`, which names its key by code point as the kitty
/// protocol does, and is read the same way.
fn tilde(parameters: &str) -> Step {
    let mut fields = parameters.split(';');
    let Some(Ok(number)) = fields.next().map(str::parse::<u16>) else {
        return Step::Skip;
    };
    let (modifiers, kind) = fields
        .next()
        .and_then(modifiers_and_kind)
        .unwrap_or((KeyModifiers::NONE, KeyEventKind::Press));
    if number == 27 {
        let Some(Ok(point)) = fields.next().map(str::parse::<u32>) else {
            return Step::Skip;
        };
        return code_point(point, modifiers, kind);
    }
    let code = match number {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F((number - 10) as u8),
        17..=21 => KeyCode::F((number - 11) as u8),
        23..=26 => KeyCode::F((number - 12) as u8),
        28..=29 => KeyCode::F((number - 15) as u8),
        31..=34 => KeyCode::F((number - 17) as u8),
        _ => return Step::Skip,
    };
    Step::Event(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)))
}

/// The kitty protocol's `<code point>[:<shifted>] ; <modifiers>[:<kind>] u`.
fn csi_u(parameters: &str) -> Step {
    let mut fields = parameters.split(';');
    let mut points = fields.next().unwrap_or_default().split(':');
    let Some(Ok(point)) = points.next().map(str::parse::<u32>) else {
        return Step::Skip;
    };
    let (modifiers, kind) = fields
        .next()
        .and_then(modifiers_and_kind)
        .unwrap_or((KeyModifiers::NONE, KeyEventKind::Press));
    let step = code_point(point, modifiers, kind);
    // The shifted key, where the terminal reports it, stands for the key and
    // the Shift together.
    let shifted = points
        .next()
        .and_then(|point| point.parse::<u32>().ok())
        .and_then(char::from_u32);
    match (step, shifted) {
        (Step::Event(Event::Key(mut key)), Some(shifted))
            if key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            key.code = KeyCode::Char(shifted);
            key.modifiers.remove(KeyModifiers::SHIFT);
            Step::Event(Event::Key(key))
        }
        (step, _) => step,
    }
}

/// The key a code point names, as the kitty protocol and modifyOtherKeys
/// both name them.
fn code_point(point: u32, modifiers: KeyModifiers, kind: KeyEventKind) -> Step {
    let (code, state) = if let Some(keypad) = keypad(point) {
        (keypad, KeyEventState::KEYPAD)
    } else if let 57376..=57398 = point {
        (KeyCode::F((point - 57376 + 13) as u8), KeyEventState::NONE)
    } else {
        let code = match char::from_u32(point) {
            Some('\x1b') => KeyCode::Esc,
            Some('\r') => KeyCode::Enter,
            Some('\t') if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            Some('\t') => KeyCode::Tab,
            Some('\x7f') => KeyCode::Backspace,
            // The private-use code points the protocol gives the keys it
            // names beyond these — media keys, the modifiers themselves —
            // are sent only to a terminal asked for every key, which the
            // shell never asks.
            Some('\u{e000}'..='\u{f8ff}') | None => return Step::Skip,
            Some(c) => KeyCode::Char(c),
        };
        (code, KeyEventState::NONE)
    };
    Step::Event(Event::Key(KeyEvent::new_with_kind_and_state(
        code, modifiers, kind, state,
    )))
}

/// The keypad keys the kitty protocol reports apart from the keys they copy.
fn keypad(point: u32) -> Option<KeyCode> {
    let code = match point {
        57399..=57408 => KeyCode::Char(char::from_digit(point - 57399, 10)?),
        57409 => KeyCode::Char('.'),
        57410 => KeyCode::Char('/'),
        57411 => KeyCode::Char('*'),
        57412 => KeyCode::Char('-'),
        57413 => KeyCode::Char('+'),
        57414 => KeyCode::Enter,
        57415 => KeyCode::Char('='),
        57416 => KeyCode::Char(','),
        57417 => KeyCode::Left,
        57418 => KeyCode::Right,
        57419 => KeyCode::Up,
        57420 => KeyCode::Down,
        57421 => KeyCode::PageUp,
        57422 => KeyCode::PageDown,
        57423 => KeyCode::Home,
        57424 => KeyCode::End,
        57425 => KeyCode::Insert,
        57426 => KeyCode::Delete,
        57427 => KeyCode::KeypadBegin,
        _ => return None,
    };
    Some(code)
}

/// `<modifiers>[:<kind>]`, the field every modified key carries.
fn modifiers_and_kind(field: &str) -> Option<(KeyModifiers, KeyEventKind)> {
    let mut parts = field.split(':');
    let mask = parts.next()?.parse::<u8>().ok()?;
    let kind = match parts.next().and_then(|kind| kind.parse::<u8>().ok()) {
        Some(2) => KeyEventKind::Repeat,
        Some(3) => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    Some((modifiers(mask), kind))
}

/// The modifiers a mask names. The mask is one more than the bits, so that
/// no modifiers at all is `1` rather than a missing field.
fn modifiers(mask: u8) -> KeyModifiers {
    let bits = mask.saturating_sub(1);
    [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ]
    .into_iter()
    .filter(|(bit, _)| bits & bit != 0)
    .fold(KeyModifiers::NONE, |held, (_, modifier)| held | modifier)
}

/// SGR mouse: `< <button> ; <column> ; <row>`, pressed at `M` and released
/// at `m`.
fn sgr_mouse(parameters: &str, released: bool) -> Step {
    let mut fields = parameters.split(';').map(str::parse::<u16>);
    let (Some(Ok(button)), Some(Ok(column)), Some(Ok(row))) =
        (fields.next(), fields.next(), fields.next())
    else {
        return Step::Skip;
    };
    let Ok(button) = u8::try_from(button) else {
        return Step::Skip;
    };
    mouse(button, column, row, released)
}

/// X10 mouse: each of the three bytes offset by 32.
fn x10_mouse(button: u8, column: u8, row: u8) -> Step {
    let Some(button) = button.checked_sub(32) else {
        return Step::Skip;
    };
    mouse(
        button,
        u16::from(column.saturating_sub(32)),
        u16::from(row.saturating_sub(32)),
        false,
    )
}

/// A mouse report, from its button byte and its one-based cell.
fn mouse(button: u8, column: u16, row: u16, released: bool) -> Step {
    let number = (button & 0b0000_0011) | ((button & 0b1100_0000) >> 4);
    let dragging = button & 0b0010_0000 != 0;
    let kind = match (number, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3..=5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return Step::Skip,
    };
    let kind = match kind {
        MouseEventKind::Down(button) if released => MouseEventKind::Up(button),
        kind => kind,
    };
    let modifiers = [
        (4, KeyModifiers::SHIFT),
        (8, KeyModifiers::ALT),
        (16, KeyModifiers::CONTROL),
    ]
    .into_iter()
    .filter(|(bit, _)| button & bit != 0)
    .fold(KeyModifiers::NONE, |held, (_, modifier)| held | modifier);
    // A terminal counts cells from one; one that sends zero is read as the
    // first cell rather than wrapping round to the last.
    Step::Event(Event::Mouse(MouseEvent {
        kind,
        column: column.saturating_sub(1),
        row: row.saturating_sub(1),
        modifiers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything one read of `bytes` produces, with nothing more on its way.
    fn read(bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        Decoder::default().feed(bytes, false, &mut events);
        events
    }

    fn pressed(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn shift_enter_in_modify_other_keys_is_enter_with_shift() {
        assert_eq!(
            read(b"\x1b[27;2;13~"),
            [pressed(KeyCode::Enter, KeyModifiers::SHIFT)]
        );
    }

    #[test]
    fn modify_other_keys_names_any_key_by_its_code_point_with_its_modifiers() {
        assert_eq!(
            read(b"\x1b[27;3;13~\x1b[27;5;106~\x1b[27;6;65~"),
            [
                pressed(KeyCode::Enter, KeyModifiers::ALT),
                pressed(KeyCode::Char('j'), KeyModifiers::CONTROL),
                pressed(
                    KeyCode::Char('A'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
            ]
        );
    }

    #[test]
    fn shift_enter_in_the_kitty_protocol_is_enter_with_shift() {
        assert_eq!(
            read(b"\x1b[13;2u"),
            [pressed(KeyCode::Enter, KeyModifiers::SHIFT)]
        );
    }

    #[test]
    fn a_kitty_release_is_read_as_a_release() {
        let [Event::Key(key)] = read(b"\x1b[97;1:3u")[..] else {
            panic!("one key was sent");
        };
        assert_eq!(key.kind, KeyEventKind::Release);
    }

    #[test]
    fn the_legacy_bytes_are_the_keys_crossterm_reads_them_as() {
        assert_eq!(
            read(b"a\rA\n\t\x7f\x08\x11\x1c\x00"),
            [
                pressed(KeyCode::Char('a'), KeyModifiers::NONE),
                pressed(KeyCode::Enter, KeyModifiers::NONE),
                pressed(KeyCode::Char('A'), KeyModifiers::SHIFT),
                pressed(KeyCode::Char('j'), KeyModifiers::CONTROL),
                pressed(KeyCode::Tab, KeyModifiers::NONE),
                pressed(KeyCode::Backspace, KeyModifiers::NONE),
                pressed(KeyCode::Char('h'), KeyModifiers::CONTROL),
                pressed(KeyCode::Char('q'), KeyModifiers::CONTROL),
                pressed(KeyCode::Char('4'), KeyModifiers::CONTROL),
                pressed(KeyCode::Char(' '), KeyModifiers::CONTROL),
            ]
        );
    }

    #[test]
    fn a_character_of_several_bytes_is_one_key() {
        assert_eq!(
            read("é→".as_bytes()),
            [
                pressed(KeyCode::Char('é'), KeyModifiers::NONE),
                pressed(KeyCode::Char('→'), KeyModifiers::NONE),
            ]
        );
    }

    #[test]
    fn an_esc_at_the_end_of_a_read_is_the_esc_key_unless_more_is_coming() {
        assert_eq!(read(b"\x1b"), [pressed(KeyCode::Esc, KeyModifiers::NONE)]);

        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        decoder.feed(b"\x1b", true, &mut events);
        assert_eq!(events, []);
        decoder.feed(b"[A", false, &mut events);
        assert_eq!(events, [pressed(KeyCode::Up, KeyModifiers::NONE)]);
    }

    #[test]
    fn an_esc_before_a_key_holds_alt_and_two_escs_are_one_esc() {
        assert_eq!(
            read(b"\x1b\r\x1b1\x1b\x1b"),
            [
                pressed(KeyCode::Enter, KeyModifiers::ALT),
                pressed(KeyCode::Char('1'), KeyModifiers::ALT),
                pressed(KeyCode::Esc, KeyModifiers::NONE),
            ]
        );
    }

    #[test]
    fn a_sequence_cut_between_two_reads_is_read_whole() {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        decoder.feed(b"\x1b[27;2", false, &mut events);
        assert_eq!(events, []);
        decoder.feed(b";13~", false, &mut events);
        assert_eq!(events, [pressed(KeyCode::Enter, KeyModifiers::SHIFT)]);
    }

    #[test]
    fn the_editing_keys_arrows_and_function_keys_are_read() {
        assert_eq!(
            read(b"\x1b[A\x1bOB\x1b[1;5C\x1b[3~\x1b[5;2~\x1b[20~\x1bOP\x1b[Z\x1b[[A"),
            [
                pressed(KeyCode::Up, KeyModifiers::NONE),
                pressed(KeyCode::Down, KeyModifiers::NONE),
                pressed(KeyCode::Right, KeyModifiers::CONTROL),
                pressed(KeyCode::Delete, KeyModifiers::NONE),
                pressed(KeyCode::PageUp, KeyModifiers::SHIFT),
                pressed(KeyCode::F(9), KeyModifiers::NONE),
                pressed(KeyCode::F(1), KeyModifiers::NONE),
                pressed(KeyCode::BackTab, KeyModifiers::SHIFT),
                pressed(KeyCode::F(1), KeyModifiers::NONE),
            ]
        );
    }

    #[test]
    fn replies_and_unknown_sequences_are_passed_over_whole() {
        assert_eq!(
            read(b"\x1b[?0u\x1b[?62;22cx\x1b[Ix\x1b[99~x\x1b[200~x"),
            [
                pressed(KeyCode::Char('x'), KeyModifiers::NONE),
                pressed(KeyCode::Char('x'), KeyModifiers::NONE),
                pressed(KeyCode::Char('x'), KeyModifiers::NONE),
                pressed(KeyCode::Char('x'), KeyModifiers::NONE),
            ]
        );
    }

    #[test]
    fn the_wheel_and_a_click_are_read_in_either_mouse_encoding() {
        assert_eq!(
            read(b"\x1b[<64;10;5M\x1b[<0;3;4M\x1b[<0;3;4m\x1b[M`!!"),
            [
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: 9,
                    row: 4,
                    modifiers: KeyModifiers::NONE,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 2,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: 2,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                }),
            ]
        );
    }

    #[test]
    fn a_mouse_report_at_cell_zero_is_the_first_cell_rather_than_a_wrap() {
        let [Event::Mouse(mouse)] = read(b"\x1b[<0;0;0M")[..] else {
            panic!("one click was sent");
        };
        assert_eq!((mouse.column, mouse.row), (0, 0));
    }

    #[test]
    fn the_keypad_reported_apart_is_read_as_the_key_it_copies() {
        let [Event::Key(key)] = read(b"\x1b[57414u")[..] else {
            panic!("one key was sent");
        };
        assert_eq!(key.code, KeyCode::Enter);
        assert_eq!(key.state, KeyEventState::KEYPAD);
    }

    #[test]
    fn garbage_that_never_ends_a_sequence_is_not_held_for_ever() {
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', LONGEST_SEQUENCE));
        long.push(b'x');
        assert_eq!(
            read(&long).last(),
            Some(&pressed(KeyCode::Char('x'), KeyModifiers::NONE))
        );
    }
}
