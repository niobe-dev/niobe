// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The two-tone bar the Usage pane reads a share with.
//!
//! One function, because every share the pane draws is read the same way: the
//! plan's windows, and the rows that will sit under them. Two bars drawn by
//! two rules would say that two shares are not the same kind of figure.

/// The cell a used share is drawn with.
const FILLED: char = '\u{2593}';

/// The cell the rest of the window is drawn with.
const TRACK: char = '\u{2591}';

/// A share of something, as the solid part of a bar and the dotted rest.
///
/// The two halves come back separately because they are drawn in different
/// colours: the share in what the figure beside it is coloured by, the rest in
/// the pane's dim.
///
/// The bar saturates at `cells` — a track has the length it has, and a share
/// past the end of it cannot be drawn as more. **The figure beside a meter is
/// what carries the overflow**, and it is never clamped: a backend reporting
/// more of a window than there is has reported something the operator has to
/// see. A share that is anything above nothing keeps a cell, so a window that
/// has been touched does not read as untouched; a share that is not a number
/// at all fills nothing.
pub fn meter(share: f64, cells: usize) -> (String, String) {
    let filled = match share.is_finite() && share > 0.0 {
        true => ((share * cells as f64).round().max(1.0) as usize).min(cells),
        false => 0,
    };
    (
        FILLED.to_string().repeat(filled),
        TRACK.to_string().repeat(cells - filled),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untouched_window_is_all_track_and_no_bar() {
        assert_eq!(meter(0.0, 12), (String::new(), "░".repeat(12)));
    }

    #[test]
    fn a_part_used_window_fills_its_share_of_the_cells() {
        assert_eq!(meter(0.51, 12), ("▓".repeat(6), "░".repeat(6)));
        assert_eq!(meter(0.71, 12), ("▓".repeat(9), "░".repeat(3)));
    }

    #[test]
    fn a_window_that_is_gone_leaves_no_track() {
        assert_eq!(meter(1.0, 12), ("▓".repeat(12), String::new()));
    }

    #[test]
    fn a_window_reported_past_its_end_fills_the_track_it_has() {
        assert_eq!(meter(1.3, 12), ("▓".repeat(12), String::new()));
    }

    #[test]
    fn a_window_barely_touched_still_shows_a_cell() {
        assert_eq!(meter(0.01, 12), ("▓".to_owned(), "░".repeat(11)));
    }

    #[test]
    fn a_share_that_is_not_a_share_draws_nothing_filled() {
        assert_eq!(meter(-0.5, 4), (String::new(), "░".repeat(4)));
        assert_eq!(meter(f64::NAN, 4), (String::new(), "░".repeat(4)));
    }

    #[test]
    fn a_meter_with_no_room_is_no_meter() {
        assert_eq!(meter(0.5, 0), (String::new(), String::new()));
    }
}
