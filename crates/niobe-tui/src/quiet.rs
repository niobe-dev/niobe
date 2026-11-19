// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! A backend that writes nothing for a frame that changed nothing.
//!
//! The loop draws ten times a second while idle and thirty while a reply
//! arrives, and most of those frames change no cell. Ratatui still ends every
//! draw by resetting the colours and showing and placing the cursor, or hiding
//! it, and a terminal repaints for those bytes as for any: iTerm2 spent 18% of
//! a core on a screen that did nothing but that ten times a second, against
//! 5% for a still one. So this remembers what the terminal was last told of
//! its cursor and passes on only what would change it.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// The shell's backend around the terminal's own: every call passed on but
/// those that would leave the terminal as it was.
pub(crate) struct Quiet<B> {
    inner: B,
    /// Whether the cursor was last shown or hidden; `None` where the shell
    /// no longer knows.
    shown: Option<bool>,
    /// Where the cursor was last put; `None` where the shell no longer
    /// knows, as after cells are written, which moves it.
    at: Option<Position>,
}

impl<B> Quiet<B> {
    /// `inner`, quietened.
    pub(crate) fn new(inner: B) -> Self {
        Self {
            inner,
            shown: None,
            at: None,
        }
    }

    /// Forgets what the terminal shows of its cursor, so that the next frame
    /// says it all again: the screen was cleared, and whoever had the
    /// terminal before it may have left the cursor anywhere.
    fn forget(&mut self) {
        self.shown = None;
        self.at = None;
    }
}

impl<B: Backend> Backend for Quiet<B> {
    type Error = B::Error;

    /// Writes the cells that changed, and nothing where none did: the
    /// terminal's own backend resets the colours after every write, cells or
    /// none.
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut content = content.peekable();
        if content.peek().is_none() {
            return Ok(());
        }
        self.at = None;
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.at = None;
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        if self.shown == Some(false) {
            return Ok(());
        }
        self.shown = Some(false);
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        if self.shown == Some(true) {
            return Ok(());
        }
        self.shown = Some(true);
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        if self.at == Some(position) {
            return Ok(());
        }
        self.at = Some(position);
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.forget();
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.forget();
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What a terminal was sent, kept where the test can read it.
    #[derive(Clone, Default)]
    struct Sent(Rc<RefCell<Vec<u8>>>);

    impl std::io::Write for Sent {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sent {
        /// What was sent since the last time this was asked.
        fn take(&self) -> String {
            String::from_utf8_lossy(&std::mem::take(&mut *self.0.borrow_mut())).into_owned()
        }
    }

    /// The screen the tests draw on.
    const SCREEN: Rect = Rect::new(0, 0, 20, 4);

    /// Crossterm's backend writing into `Sent`, its screen [`SCREEN`]:
    /// crossterm asks the process's terminal for its size, which a CI runner
    /// does not have, and ratatui asks for the size whenever it clears a
    /// fixed viewport, as a resize does.
    struct Screen(CrosstermBackend<Sent>);

    impl Backend for Screen {
        type Error = std::io::Error;

        fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.0.draw(content)
        }

        fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
            self.0.append_lines(n)
        }

        fn hide_cursor(&mut self) -> std::io::Result<()> {
            self.0.hide_cursor()
        }

        fn show_cursor(&mut self) -> std::io::Result<()> {
            self.0.show_cursor()
        }

        fn get_cursor_position(&mut self) -> std::io::Result<Position> {
            self.0.get_cursor_position()
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, at: P) -> std::io::Result<()> {
            self.0.set_cursor_position(at)
        }

        fn clear(&mut self) -> std::io::Result<()> {
            self.0.clear()
        }

        fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
            self.0.clear_region(clear_type)
        }

        fn size(&self) -> std::io::Result<Size> {
            Ok(SCREEN.as_size())
        }

        fn window_size(&mut self) -> std::io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: SCREEN.as_size(),
                pixels: Size::default(),
            })
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Backend::flush(&mut self.0)
        }
    }

    fn terminal() -> (Terminal<Quiet<Screen>>, Sent) {
        let sent = Sent::default();
        let terminal = Terminal::with_options(
            Quiet::new(Screen(CrosstermBackend::new(sent.clone()))),
            TerminalOptions {
                viewport: Viewport::Fixed(SCREEN),
            },
        )
        .expect("a terminal over a buffer opens");
        (terminal, sent)
    }

    /// Draws `said` at the top left, with the cursor at `cursor` or none.
    fn draw(terminal: &mut Terminal<Quiet<Screen>>, said: &str, cursor: Option<(u16, u16)>) {
        terminal
            .draw(|frame| {
                frame.render_widget(ratatui::widgets::Paragraph::new(said), frame.area());
                if let Some(at) = cursor {
                    frame.set_cursor_position(at);
                }
            })
            .expect("a draw into a buffer succeeds");
    }

    /// A frame the same as the one before it sends the terminal nothing:
    /// not the cells, not the colours reset, not the cursor shown again
    /// where it already is. A terminal repaints for any of it.
    #[test]
    fn a_frame_that_changed_nothing_sends_nothing() {
        let (mut terminal, sent) = terminal();
        draw(&mut terminal, "hello", Some((5, 0)));
        assert!(sent.take().contains("hello"));

        draw(&mut terminal, "hello", Some((5, 0)));

        assert_eq!(sent.take(), "");
    }

    /// A frame that only moved the cursor sends only the move.
    #[test]
    fn a_frame_that_only_moved_the_cursor_sends_only_the_move() {
        let (mut terminal, sent) = terminal();
        draw(&mut terminal, "hello", Some((5, 0)));
        sent.take();

        draw(&mut terminal, "hello", Some((2, 1)));

        assert_eq!(sent.take(), "\u{1b}[2;3H");
    }

    /// A frame that changed a cell sends it, and puts the cursor back where
    /// the frame wants it, since writing the cell moved it.
    #[test]
    fn a_frame_that_changed_a_cell_puts_the_cursor_back() {
        let (mut terminal, sent) = terminal();
        draw(&mut terminal, "hello", Some((5, 0)));
        sent.take();

        draw(&mut terminal, "hellO", Some((5, 0)));

        let said = sent.take();
        assert!(said.contains('O'), "{said:?}");
        assert!(said.ends_with("\u{1b}[1;6H"), "{said:?}");
        assert!(!said.contains("\u{1b}[?25h"), "already shown: {said:?}");
    }

    /// A cursor hidden is hidden once, and shown again once it is wanted;
    /// hidden, it did not move, so it is shown where it was without a move.
    #[test]
    fn a_cursor_is_hidden_and_shown_once_each() {
        let (mut terminal, sent) = terminal();
        draw(&mut terminal, "hello", Some((5, 0)));
        sent.take();

        draw(&mut terminal, "hello", None);
        assert_eq!(sent.take(), "\u{1b}[?25l");
        draw(&mut terminal, "hello", None);
        assert_eq!(sent.take(), "");

        draw(&mut terminal, "hello", Some((5, 0)));
        assert_eq!(sent.take(), "\u{1b}[?25h");
        draw(&mut terminal, "hello", Some((6, 0)));
        assert_eq!(sent.take(), "\u{1b}[1;7H");
    }

    /// A screen cleared whole — as the shell's resize clears it, which is
    /// also how it takes the terminal back after a stop or an editor, when it
    /// no longer knows what the screen shows — has the cursor shown and
    /// placed again, though the frame wants it where it was.
    #[test]
    fn a_cleared_screen_has_its_cursor_shown_and_placed_again() {
        let (mut terminal, sent) = terminal();
        draw(&mut terminal, "hello", Some((5, 0)));
        terminal
            .backend_mut()
            .clear_region(ClearType::All)
            .expect("a clear of a buffer succeeds");
        // The screen the next frame is drawn on is blank, as ratatui's
        // resize makes it.
        terminal
            .resize(SCREEN)
            .expect("a resize of a buffer succeeds");
        sent.take();

        draw(&mut terminal, "hello", Some((5, 0)));

        let said = sent.take();
        assert!(said.contains("hello"), "{said:?}");
        assert!(said.ends_with("\u{1b}[?25h\u{1b}[1;6H"), "{said:?}");
    }
}
