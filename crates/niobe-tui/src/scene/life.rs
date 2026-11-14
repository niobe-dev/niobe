// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The Game of Life `classic` shows in the session pane before anything has
//! happened in it: Conway's cells living, dying and being born on the DOS
//! blue, a generation every [`GENERATION`] milliseconds, from a random soup
//! that is sown again every [`GENERATIONS`] generations, before it has
//! settled into the still lifes and blinkers a soup ends in.
//!
//! A frame is worked out from the seed and the moment alone, so a generation
//! is the soup stepped on as many times as the moment says; the last one
//! reached is kept only so the next frame need not step from the soup
//! again, and is the same whichever way it is reached. The board is a torus — what leaves one
//! edge comes in at the other — held a row of 64 cells to a word, so a
//! generation of the whole board is a few thousand word operations rather
//! than a pass over every cell, and even the last generation of an era is
//! reached inside a frame. A board wider or taller than
//! [`LARGEST`] is laid over the pane in tiles, which a torus joins without a
//! seam.
//!
//! A cell of the board is half a terminal cell, the upper or lower half
//! block, so the board's cells are about square. A cell born this generation
//! is drawn white, one that has lived longer in the theme's body colour, and
//! one that died this generation leaves a dim ghost for one generation.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{darker, faded, key, rgb};
use crate::theme::Theme;

/// How long a generation lasts, in milliseconds.
const GENERATION: u64 = 120;

/// How many generations a soup lives before another is sown.
const GENERATIONS: u64 = 400;

/// The largest board worked out, in cells across and down: enough for any
/// pane but the largest, which see it in tiles.
const LARGEST: (usize, usize) = (256, 160);

/// The cells of a board, a row at a time, 64 to a word, lowest bit first;
/// the bits past the board's width in a row's last word are always clear.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Board {
    width: usize,
    height: usize,
    /// Words to a row.
    words: usize,
    cells: Vec<u64>,
}

impl Board {
    fn empty(width: usize, height: usize) -> Self {
        let words = width.div_ceil(64).max(1);
        Self {
            width,
            height,
            words,
            cells: vec![0; words * height],
        }
    }

    /// The soup era `era` starts from: about three cells in eight alive.
    fn soup(width: usize, height: usize, seed: u64, era: u64) -> Self {
        let mut board = Self::empty(width, height);
        let salt = key(seed, 0x50_0B, era);
        for (n, word) in (0u64..).zip(board.cells.iter_mut()) {
            let bits = |part: u64| key(salt, part, n);
            *word = bits(1) & (bits(2) | bits(3));
        }
        board.clear_past_the_edge();
        board
    }

    fn clear_past_the_edge(&mut self) {
        let spare = self.words * 64 - self.width;
        if spare == 0 {
            return;
        }
        let keep = u64::MAX >> spare;
        for row in self.cells.chunks_mut(self.words) {
            if let Some(last) = row.last_mut() {
                *last &= keep;
            }
        }
    }

    fn row(&self, y: usize) -> &[u64] {
        let from = y * self.words;
        self.cells.get(from..from + self.words).unwrap_or(&[])
    }

    /// Whether the cell at `(x, y)` is alive, either counted round the torus
    /// as often as it likes.
    fn alive(&self, x: usize, y: usize) -> bool {
        let x = x % self.width.max(1);
        self.row(y % self.height.max(1))
            .get(x / 64)
            .is_some_and(|word| word >> (x % 64) & 1 == 1)
    }

    /// The next generation: a cell with three neighbours alive is born or
    /// lives on, one with two lives on if it was alive, and every other dies.
    fn step(&self) -> Self {
        let (height, words) = (self.height, self.words);
        let mut next = Self::empty(self.width, height);
        if self.width == 0 || height == 0 {
            return next;
        }
        let mut west = vec![0; words * height];
        let mut east = vec![0; words * height];
        for y in 0..height {
            let row = y * words..(y + 1) * words;
            self.shift(y, Shift::West, &mut west[row.clone()]);
            self.shift(y, Shift::East, &mut east[row]);
        }
        let cells = &self.cells;
        for y in 0..height {
            let (up, down) = ((y + height - 1) % height, (y + 1) % height);
            for w in 0..words {
                let (above, here, below) = (up * words + w, y * words + w, down * words + w);
                let neighbours = [
                    west[above],
                    cells[above],
                    east[above],
                    west[here],
                    east[here],
                    west[below],
                    cells[below],
                    east[below],
                ];
                let (mut ones, mut twos, mut fours) = (0u64, 0u64, 0u64);
                for neighbour in neighbours {
                    let carry = ones & neighbour;
                    ones ^= neighbour;
                    let carry_two = twos & carry;
                    twos ^= carry;
                    fours ^= carry_two;
                }
                // Counted to eight and no further, so eight reads as none:
                // both die. Two or three is `twos` without `fours`, and three
                // has `ones` too.
                next.cells[here] = twos & !fours & (ones | cells[here]);
            }
        }
        next.clear_past_the_edge();
        next
    }

    /// Row `y` with every cell moved one place, wrapping round the torus,
    /// written to `out`, a row's words long: `West` gives each cell the state
    /// of the cell to its west, `East` of the one to its east.
    fn shift(&self, y: usize, shift: Shift, out: &mut [u64]) {
        let row = self.row(y);
        let last = self.width - 1;
        let bit = |x: usize| row.get(x / 64).map_or(0, |word| word >> (x % 64) & 1);
        for (w, slot) in out.iter_mut().enumerate() {
            let word = row.get(w).copied().unwrap_or(0);
            *slot = match shift {
                Shift::West => {
                    let carried = w.checked_sub(1).and_then(|before| row.get(before));
                    word << 1 | carried.map_or(0, |before| before >> 63)
                }
                Shift::East => {
                    let carried = row.get(w + 1);
                    word >> 1 | carried.map_or(0, |after| after << 63)
                }
            };
        }
        match shift {
            Shift::West => {
                if let Some(first) = out.first_mut() {
                    *first |= bit(last);
                }
            }
            Shift::East => {
                if let Some(word) = out.get_mut(last / 64) {
                    *word |= bit(0) << (last % 64);
                }
            }
        }
        let spare = self.words * 64 - self.width;
        if let Some(word) = out.last_mut().filter(|_| spare > 0) {
            *word &= u64::MAX >> spare;
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Shift {
    West,
    East,
}

/// What a board's cell is at the generation drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Empty,
    /// Died this generation.
    Ghost,
    /// Alive before and still.
    Living,
    /// Born this generation.
    Born,
}

impl Fate {
    fn of(before: bool, now: bool) -> Self {
        match (before, now) {
            (false, false) => Self::Empty,
            (true, false) => Self::Ghost,
            (true, true) => Self::Living,
            (false, true) => Self::Born,
        }
    }
}

/// The generation `millis` in and the one before it, on a board `width` by
/// `height`, from `seed`: the soup of the era `millis` falls in, stepped on.
///
/// The last generation worked out on this thread is kept, and a later one in
/// the same era is stepped on from it rather than from the soup: a frame
/// comes a thirtieth of a second after the last, so it steps at most one
/// generation. What is kept changes no frame — the generation is the same
/// whichever way it was reached — and a moment before it, another era, seed
/// or size, starts from the soup again.
fn generations(width: usize, height: usize, seed: u64, millis: u64) -> (Board, Board) {
    let generation = millis / GENERATION;
    let (era, into) = (generation / GENERATIONS, generation % GENERATIONS);
    let found = (seed, era, width, height);
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        let mut reached = match last.take() {
            Some(kept) if kept.found == found && kept.into <= into => kept,
            _ => Reached {
                found,
                into: 0,
                // The soup is sown out of nothing, so its first generation
                // has no ghosts: the board before it is empty.
                before: Board::empty(width, height),
                now: Board::soup(width, height, seed, era),
            },
        };
        while reached.into < into {
            let next = reached.now.step();
            reached.before = std::mem::replace(&mut reached.now, next);
            reached.into += 1;
        }
        let boards = (reached.before.clone(), reached.now.clone());
        *last = Some(reached);
        boards
    })
}

/// A generation worked out: what it was worked out from — the seed, the
/// era and the board's size — how far into the era it is, and the boards.
#[derive(Debug)]
struct Reached {
    found: (u64, u64, usize, usize),
    into: u64,
    before: Board,
    now: Board,
}

thread_local! {
    static LAST: std::cell::RefCell<Option<Reached>> = const { std::cell::RefCell::new(None) };
}

/// The colours each fate is drawn in.
struct Ink {
    pane: Color,
    living: Color,
    born: Color,
    ghost: Color,
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        let born = match rgb(theme.pane_bg) {
            Some(_) => Color::Rgb(255, 255, 255),
            None => Color::White,
        };
        Self {
            pane: theme.pane_bg,
            living: theme.fg,
            born,
            ghost: faded(theme.fg, theme.pane_bg, 40, darker(theme.fg)),
        }
    }

    fn colour(&self, fate: Fate) -> Option<Color> {
        match fate {
            Fate::Empty => None,
            Fate::Ghost => Some(self.ghost),
            Fate::Living => Some(self.living),
            Fate::Born => Some(self.born),
        }
    }

    /// The glyph and style of a terminal cell holding `upper` over `lower`.
    fn cell(&self, upper: Fate, lower: Fate) -> (char, Style) {
        let paper = Style::new().bg(self.pane);
        match (self.colour(upper), self.colour(lower)) {
            (None, None) => (' ', paper),
            (Some(top), None) => ('▀', paper.fg(top)),
            (None, Some(bottom)) => ('▄', paper.fg(bottom)),
            (Some(top), Some(bottom)) if top == bottom => ('█', paper.fg(top)),
            (Some(top), Some(bottom)) => ('▀', Style::new().fg(top).bg(bottom)),
        }
    }
}

/// Draws the board into `area` of `buffer`, cleared to the pane's colour, as
/// it stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let width = usize::from(area.width).min(LARGEST.0);
    let height = (usize::from(area.height) * 2).min(LARGEST.1);
    let (before, now) = generations(width, height, seed, millis);
    let ink = Ink::of(theme);
    let fate = |x: usize, y: usize| Fate::of(before.alive(x, y), now.alive(x, y));
    for row in 0..area.height {
        for column in 0..area.width {
            let (x, y) = (usize::from(column), usize::from(row) * 2);
            let (glyph, style) = ink.cell(fate(x, y), fate(x, y + 1));
            buffer[(area.x + column, area.y + row)]
                .set_char(glyph)
                .set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::theme::CLASSIC;

    fn board(width: usize, height: usize, alive: &[(usize, usize)]) -> Board {
        let mut board = Board::empty(width, height);
        for (x, y) in alive {
            if let Some(word) = board.cells.get_mut(y * board.words + x / 64) {
                *word |= 1 << (x % 64);
            }
        }
        board
    }

    fn living(board: &Board) -> Vec<(usize, usize)> {
        (0..board.height)
            .flat_map(|y| (0..board.width).map(move |x| (x, y)))
            .filter(|(x, y)| board.alive(*x, *y))
            .collect()
    }

    /// The rules worked out a cell at a time, against which the word-wide
    /// step is checked.
    fn stepped_by_hand(board: &Board) -> Vec<(usize, usize)> {
        let (width, height) = (board.width, board.height);
        let mut alive = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let mut neighbours = 0;
                // By place rather than by value: on a board one cell wide
                // the cell west of a cell and the one east of it are itself.
                for (row, dy) in [height - 1, 0, 1].into_iter().enumerate() {
                    for (column, dx) in [width - 1, 0, 1].into_iter().enumerate() {
                        if (row, column) != (1, 1) && board.alive(x + dx, y + dy) {
                            neighbours += 1;
                        }
                    }
                }
                if neighbours == 3 || (neighbours == 2 && board.alive(x, y)) {
                    alive.push((x, y));
                }
            }
        }
        alive
    }

    #[test]
    fn a_step_follows_conways_rules_on_boards_of_every_width_a_word_splits() {
        for (width, height) in [(1, 1), (3, 3), (7, 5), (63, 9), (64, 8), (65, 6), (130, 4)] {
            for era in 0..4 {
                let mut soup = Board::soup(width, height, 11, era);
                for _ in 0..6 {
                    let next = soup.step();
                    assert_eq!(
                        living(&next),
                        stepped_by_hand(&soup),
                        "{width}x{height}, era {era}"
                    );
                    soup = next;
                }
            }
        }
    }

    #[test]
    fn a_blinker_turns_a_quarter_each_generation_and_back() {
        let across = board(5, 5, &[(1, 2), (2, 2), (3, 2)]);
        let upright = across.step();
        assert_eq!(living(&upright), vec![(2, 1), (2, 2), (2, 3)]);
        assert_eq!(upright.step(), across);
    }

    /// A glider that flies off one edge comes in at the other, and after as
    /// many generations as it takes to cross the board is where it began.
    #[test]
    fn a_glider_flies_round_the_torus_and_comes_back_to_where_it_began() {
        let glider = board(8, 8, &[(1, 0), (2, 1), (0, 2), (1, 2), (2, 2)]);
        let mut board = glider.clone();
        for _ in 0..32 {
            board = board.step();
            assert_eq!(living(&board).len(), 5);
        }
        assert_eq!(board, glider);
    }

    #[test]
    fn a_generation_holds_until_the_next_and_a_new_soup_is_sown_each_era() {
        let at = |millis: u64| generations(40, 20, 7, millis).1;
        assert_eq!(at(GENERATION * 10), at(GENERATION * 10 + GENERATION - 1));
        assert_ne!(at(GENERATION * 10), at(GENERATION * 11));
        let era = GENERATION * GENERATIONS;
        assert_eq!(at(era), Board::soup(40, 20, 7, 1));
        assert_ne!(at(era), Board::soup(40, 20, 7, 0));
        let alive = living(&at(0)).len();
        assert!(
            (200..400).contains(&alive),
            "{alive} of 800 alive in a soup"
        );
    }

    /// The soup stepped on `into` times by hand, with nothing kept.
    fn from_the_soup(width: usize, height: usize, seed: u64, millis: u64) -> Board {
        let generation = millis / GENERATION;
        let mut board = Board::soup(width, height, seed, generation / GENERATIONS);
        for _ in 0..generation % GENERATIONS {
            board = board.step();
        }
        board
    }

    #[test]
    fn a_generation_is_the_same_reached_from_the_last_frame_or_from_the_soup() {
        let moments = [5_000, 5_033, 9_000, 4_000, 47_000, 49_000, 47_100];
        for (width, height) in [(40, 20), (41, 20), (40, 20)] {
            for millis in moments {
                for seed in [1, 2] {
                    assert_eq!(
                        generations(width, height, seed, millis).1,
                        from_the_soup(width, height, seed, millis),
                        "{width}x{height} at {millis} ms from seed {seed}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_soup_is_still_alive_when_its_era_ends() {
        for seed in 0..8 {
            let (before, last) = generations(120, 60, seed, GENERATION * (GENERATIONS - 1));
            assert!(living(&last).len() > 60, "seed {seed} died out");
            assert_ne!(before, last, "seed {seed} stood still");
        }
    }

    #[test]
    fn two_board_cells_share_a_terminal_cell_one_above_the_other() {
        let ink = Ink::of(&CLASSIC);
        assert_eq!(ink.cell(Fate::Empty, Fate::Empty).0, ' ');
        assert_eq!(ink.cell(Fate::Living, Fate::Empty).0, '▀');
        assert_eq!(ink.cell(Fate::Empty, Fate::Born).0, '▄');
        assert_eq!(ink.cell(Fate::Living, Fate::Living).0, '█');
        let (glyph, style) = ink.cell(Fate::Born, Fate::Ghost);
        assert_eq!(glyph, '▀');
        assert_eq!(
            (style.fg, style.bg),
            (Some(Color::White), Some(Color::Cyan))
        );
    }

    #[test]
    fn the_cells_are_drawn_in_classics_light_colours_on_its_blue() {
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        super::super::draw(
            &mut buffer,
            area,
            std::time::Duration::from_millis(GENERATION * 50),
            7,
            &CLASSIC,
        );
        let colours = [Color::Blue, Color::LightCyan, Color::White, Color::Cyan];
        let mut seen = std::collections::HashSet::new();
        for cell in buffer.content() {
            assert!(
                colours.contains(&cell.fg) && colours.contains(&cell.bg),
                "{cell:?}"
            );
            seen.insert(cell.fg);
        }
        assert!(seen.contains(&Color::LightCyan) && seen.contains(&Color::White));
    }

    /// A pane larger than the largest board sees it in tiles, which the
    /// torus joins without a seam.
    #[test]
    fn a_pane_wider_than_the_largest_board_repeats_it() {
        let area = Rect::new(0, 0, 300, 90);
        let mut buffer = Buffer::empty(area);
        draw(&mut buffer, area, 5_000, 3, &CLASSIC);
        let wide = u16::try_from(LARGEST.0).expect("the board fits a pane");
        for y in 0..90 {
            for x in 0..300 - wide {
                assert_eq!(buffer[(x, y)], buffer[(x + wide, y)], "({x}, {y})");
            }
        }
    }
}
