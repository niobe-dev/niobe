// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The black hole `stellar` shows in the session pane before anything has
//! happened in it: a hole seen from just above the plane of its accretion
//! disk, the disk's far side bent up over the shadow and down under it, a
//! thin ring of light hugging the shadow's edge, the stars behind it pulled
//! into arcs, and the disk's gas flowing round, faster the nearer it is.
//!
//! The picture is ray traced. From the camera, a ray for every pixel is
//! followed through the hole's gravity — a non-rotating hole, whose light
//! bends as `u'' + u = 3Mu²` says, integrated in three dimensions as
//! `x'' = −(3/2)h²x/r⁵` with the horizon at `r = 1` — until it falls in, meets
//! the disk or leaves for the stars. Nothing a ray does depends on the
//! moment, so where every pixel's ray ends is worked out once for a pane's
//! size and kept ([`chart`]); a frame only shades what each ray met, which is
//! what keeps the scene inside a frame's budget.
//!
//! The disk runs from the innermost stable orbit, three times the horizon's
//! radius, out to ten. It glows as a thin disk does, brightest a little
//! outside its inner edge, and each point of it is seen shifted by how fast
//! it moves and how deep in the hole's well it sits: the side coming toward
//! the camera brighter and hotter, the side going away and the gas nearest
//! the hole dimmer and redder. The film this theme takes its mood from left
//! that shift out; it is kept here, softened ([`BEAMING`]), because it is
//! what makes the disk read as turning. The gas is streaks of noise that go
//! round at the speed of an orbit at their radius, so the inner edge laps
//! the outer one.
//!
//! A pixel is half a terminal cell, the upper or lower half block, so the
//! pixels are about square and the hole is drawn in shaded colour rather
//! than in characters. Every light is one of a few steps, so a frame
//! rewrites only the cells whose step changed. In the sixteen names the disk
//! is the theme's red, darker gold, gold and white by how bright it is, and
//! the stars white and grey.

use std::cell::RefCell;
use std::f32::consts::TAU;
use std::rc::Rc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{darker, key, rgb};
use crate::theme::Theme;

/// How far the camera is from the hole, in the horizon's radius.
const DISTANCE: f32 = 26.0;

/// How far above the plane of the disk the camera looks from, in radians:
/// low enough that the disk is a band across the shadow and its far side is
/// bent into a halo over and under it.
const ELEVATION: f32 = 0.11;

/// Where the disk starts and ends, in the horizon's radius: the innermost
/// orbit gas can keep round a non-rotating hole, and out to ten.
const INNER: f32 = 3.0;
const OUTER: f32 = 10.0;

/// How much of the scene the pane holds at the least, either side of the
/// hole and above and below it, at the hole's distance: the disk across, and
/// the halo its far side makes over and under the shadow.
const HALF_WIDE: f32 = 10.6;
const HALF_TALL: f32 = 5.4;

/// How far a ray goes in one step, as a share of how far it is from the
/// hole: short steps where the light bends hard, long ones where it hardly
/// bends.
const STEP: f32 = 0.06;

/// How far from the hole, in the horizon's radius, a step grows from a
/// share of the distance to that share of its square over this: out there
/// the light hardly bends, and the long way in from the camera and out to
/// the stars is most of a ray's steps.
const FAR: f32 = 4.0;

/// How many steps a ray is followed for before it is taken as caught: one
/// that has not fallen in or left by then has gone round the photon sphere
/// a few times, and what it ends on is a sliver too thin for a pixel.
const MOST_STEPS: u32 = 800;

/// How long the gas at the disk's inner edge takes to go round once, in
/// milliseconds. Gas further out takes longer, as an orbit does.
const LAP: f64 = 16_000.0;

/// The power the shift of a point of the disk raises its brightness to.
/// The light of a point coming toward the camera at a speed is brighter by
/// the shift to the fourth; at that, one side of the disk outshines the
/// other twentyfold and the far side all but vanishes on a terminal's few
/// steps of light, and at two the near side still burns white while the far
/// one is ash. One keeps the disk whole, as the film drew it, and still
/// shows which way it turns.
const BEAMING: i32 = 1;

/// How bright a point of the disk at its brightest radius, unshifted and of
/// average gas, is drawn: past one is white-hot.
const EXPOSURE: f32 = 1.25;

/// How many steps a light is rounded to: enough that the disk reads as
/// smooth, few enough that its flow moves only the cells at a step's edge.
const STEPS: f32 = 16.0;

/// How close to the hole, in the horizon's radius, a ray that got away came
/// to be drawn as the photon ring: from the photon sphere at one and a half
/// out to here, which is about a pixel across the ring at any pane size.
const RING_REACH: f32 = 2.0;

/// How many of the sky's cells, one to a pixel, hold a star, in a thousand.
const STARS: u64 = 14;

type Vector = [f32; 3];

fn add(a: Vector, b: Vector) -> Vector {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Vector, by: f32) -> Vector {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn dot(a: Vector, b: Vector) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vector, b: Vector) -> Vector {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: Vector) -> Vector {
    let length = dot(a, a).sqrt();
    match length > f32::EPSILON {
        true => scale(a, 1.0 / length),
        false => [0.0, 0.0, 1.0],
    }
}

/// `a` and `b` mixed, `share` of the way from `a` to `b`.
fn lerp(a: Vector, b: Vector, share: f32) -> Vector {
    add(scale(a, 1.0 - share), scale(b, share))
}

/// What a pixel's ray met.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Seen {
    /// The hole: the ray fell in, or circles it still.
    Hole,
    /// The sky, at the cell of it the ray left toward: one cell a pixel
    /// across, so that a star is a pixel where the light did not bend and
    /// an arc where it did.
    Sky { cell: (i32, i32) },
    /// The disk.
    Disk(Gas),
    /// The photon ring: light that grazed the sphere where light can circle
    /// the hole, `glow` from nought to one by how close it came. What such a
    /// ray ends on is a thin, many-times-folded image of the disk and the
    /// sky that a pixel's single ray only samples, which is a dotted line;
    /// drawn as the glow it sums to, it is the unbroken ring round the
    /// shadow a camera sees.
    Ring { glow: f32 },
}

/// The point of the disk a ray met, and what its light looks like from the
/// camera before the gas's own streaks are laid on it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Gas {
    /// How far from the hole, in the horizon's radius.
    radius: f32,
    /// Where round the disk, in turns from the side on the camera's right,
    /// going away from the camera.
    turn: f32,
    /// How many times faster than the outer edge's the gas here goes round.
    pace: f32,
    /// How bright the disk is here, shifted, before the streaks.
    shine: f32,
    /// How hot it looks, shifted, from nought to about one.
    heat: f32,
    /// How the light is shifted: above one coming toward the camera, below
    /// one going away or climbing out of the hole's well.
    shift: f32,
}

/// The camera: where it is and the way it faces, right and up.
#[derive(Debug, Clone, Copy)]
struct Camera {
    at: Vector,
    forward: Vector,
    right: Vector,
    up: Vector,
}

impl Camera {
    fn new() -> Self {
        let at = [0.0, DISTANCE * ELEVATION.sin(), -DISTANCE * ELEVATION.cos()];
        let forward = scale(at, -1.0 / DISTANCE);
        let right = unit(cross([0.0, 1.0, 0.0], forward));
        let up = cross(forward, right);
        Self {
            at,
            forward,
            right,
            up,
        }
    }

    /// The way a ray leaves the camera toward the point `across` and `up`
    /// of the hole, at the hole's distance, in the horizon's radius.
    fn way(&self, across: f32, up: f32) -> Vector {
        unit(add(
            scale(self.forward, DISTANCE),
            add(scale(self.right, across), scale(self.up, up)),
        ))
    }
}

/// How the light of a thin disk falls off with its radius — the flux of a
/// disk whose inner edge exerts no torque — as a share of its brightest.
fn glow(radius: f32) -> f32 {
    // The flux, `x³(1 − √x)` in `x = INNER / r`, is greatest at √x = 6/7.
    let brightest = (36.0_f32 / 49.0).powi(3) / 7.0;
    let x = INNER / radius;
    let flux = x.powi(3) * (1.0 - x.sqrt());
    // The outer edge fades rather than stops.
    let edge = ((OUTER - radius) / (0.25 * OUTER)).clamp(0.0, 1.0);
    (flux / brightest).max(0.0) * edge
}

/// Where a ray from `from` going `way` ends, followed through the hole's
/// gravity; `bends` is false only for a test that wants to know where the
/// same ray would end were there no hole to bend it.
fn trace(from: Vector, way: Vector, bends: bool, per_radian: f32) -> Seen {
    let held = cross(from, way);
    let pull = match bends {
        true => 1.5 * dot(held, held),
        false => 0.0,
    };
    let accel = |x: Vector| {
        let r2 = dot(x, x);
        scale(x, -pull / (r2 * r2 * r2.sqrt()))
    };
    let (mut x, mut v) = (from, way);
    let mut a = accel(x);
    let mut closest = f32::MAX;
    for _ in 0..MOST_STEPS {
        let r = dot(x, x).sqrt();
        closest = closest.min(r);
        if r < 1.0 {
            return Seen::Hole;
        }
        if r > DISTANCE * 1.2 && dot(x, v) > 0.0 {
            return ringed(closest).unwrap_or(Seen::Sky {
                cell: sky_cell(v, per_radian),
            });
        }
        let dt = STEP * r * (1.0 + r / FAR);
        let next = add(add(x, scale(v, dt)), scale(a, 0.5 * dt * dt));
        if let Some(gas) = crossing(x, next, held) {
            return ringed(closest).unwrap_or(Seen::Disk(gas));
        }
        let then = accel(next);
        v = add(v, scale(add(a, then), 0.5 * dt));
        (x, a) = (next, then);
    }
    Seen::Hole
}

/// The photon ring, where a ray that came `closest` to the hole grazed the
/// photon sphere.
fn ringed(closest: f32) -> Option<Seen> {
    let glow = (RING_REACH - closest) / (RING_REACH - 1.5);
    (glow > 0.0).then(|| Seen::Ring {
        glow: stepped(glow.min(1.0)),
    })
}

/// The point of the disk the step from `was` to `now` passes through, where
/// it crosses the disk's plane within the disk; `held` is the ray's angular
/// momentum, which says how the gas's motion shifts its light.
fn crossing(was: Vector, now: Vector, held: Vector) -> Option<Gas> {
    if (was[1] > 0.0) == (now[1] > 0.0) {
        return None;
    }
    let share = was[1] / (was[1] - now[1]);
    let x = was[0] + (now[0] - was[0]) * share;
    let z = was[2] + (now[2] - was[2]) * share;
    let radius = x.hypot(z);
    if !(INNER..=OUTER).contains(&radius) {
        return None;
    }
    // The gas goes round the upright axis, carried from the camera's right
    // toward it, at the speed of a circular orbit, `Ω = √(M / r³)` with the
    // mass half the horizon's radius.
    let spin = (0.5 / radius.powi(3)).sqrt();
    // The light reaching the camera travels against the traced ray, so its
    // angular momentum about that axis is the ray's, turned round.
    let about = -held[1];
    let well = (1.0 - 1.5 / radius).max(0.0).sqrt();
    let shift = well / (1.0 - spin * about).max(0.05);
    let flux = glow(radius);
    Some(Gas {
        radius,
        turn: (z.atan2(x) / TAU).rem_euclid(1.0),
        pace: (OUTER / radius).powf(1.5),
        shine: flux * shift.powi(BEAMING),
        // Hot enough at its hottest to reach the ramp's gold, and white only
        // where the shift heats it further.
        heat: 0.8 * flux.powf(0.25) * shift,
        shift,
    })
}

/// The cell of the sky a ray leaving along `way` ends in, `per_radian`
/// cells to a radian: about a pixel's worth of sky each.
fn sky_cell(way: Vector, per_radian: f32) -> (i32, i32) {
    let way = unit(way);
    let east = way[0].atan2(way[2]);
    let north = way[1].clamp(-1.0, 1.0).asin();
    // Both within a turn, times at most a few thousand pixels to a radian.
    (
        (east * per_radian).floor() as i32,
        (north * per_radian).floor() as i32,
    )
}

/// What every pixel of a pane `width` columns by `height` rows sees, two
/// pixels to a cell one over the other, a row of pixels at a time.
fn survey(width: u16, height: u16) -> Vec<Seen> {
    let (columns, rows) = (f32::from(width), 2.0 * f32::from(height));
    let per = (columns / 2.0 / HALF_WIDE).min(rows / 2.0 / HALF_TALL);
    if per <= 0.0 {
        return Vec::new();
    }
    let camera = Camera::new();
    let per_radian = per * DISTANCE;
    (0..2 * height)
        .flat_map(|row| (0..width).map(move |column| (row, column)))
        .map(|(row, column)| {
            let across = (f32::from(column) + 0.5 - columns / 2.0) / per;
            let up = (rows / 2.0 - f32::from(row) - 0.5) / per;
            trace(camera.at, camera.way(across, up), true, per_radian)
        })
        .collect()
}

/// The surveys of the last few pane sizes drawn.
#[derive(Debug)]
struct Charted {
    size: (u16, u16),
    seen: Rc<[Seen]>,
}

/// How many pane sizes' surveys are kept: a pane being resized passes
/// through many, and going back to one is what the few kept are for.
const KEPT: usize = 3;

thread_local! {
    static CHARTS: RefCell<Vec<Charted>> = const { RefCell::new(Vec::new()) };
}

/// What every pixel of a pane `width` by `height` sees: surveyed the first
/// time a pane is that size, and kept, since nothing a ray does moves.
fn chart(width: u16, height: u16) -> Rc<[Seen]> {
    CHARTS.with(|charts| {
        let mut charts = charts.borrow_mut();
        if let Some(at) = charts.iter().position(|kept| kept.size == (width, height)) {
            let kept = charts.remove(at);
            let seen = Rc::clone(&kept.seen);
            charts.push(kept);
            return seen;
        }
        let seen: Rc<[Seen]> = survey(width, height).into();
        if charts.len() >= KEPT {
            charts.remove(0);
        }
        charts.push(Charted {
            size: (width, height),
            seen: Rc::clone(&seen),
        });
        seen
    })
}

/// `light` rounded to one of [`STEPS`].
fn stepped(light: f32) -> f32 {
    (light.max(0.0) * STEPS).round() / STEPS
}

/// Smooth noise from nought to one at `(along, round)`, wrapping every
/// `around` in `round` so it closes up round the disk.
fn noise(seed: u64, salt: u64, along: f32, round: f32, around: i64) -> f32 {
    let (i, j) = (along.floor(), round.floor());
    let (fi, fj) = (along - i, round - j);
    let (si, sj) = (fi * fi * (3.0 - 2.0 * fi), fj * fj * (3.0 - 2.0 * fj));
    // Both are small: a radius times a few, and a share of a turn times a
    // few dozen.
    let (i, j) = (i as i64, j as i64);
    let corner = |di: i64, dj: i64| {
        let ring = (j + dj).rem_euclid(around);
        let n = ((i + di) as u64) << 32 ^ ring as u64;
        // The top 24 bits, which an f32 holds exactly.
        (key(seed, salt, n) >> 40) as f32 / (1u64 << 24) as f32
    };
    let low = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * si;
    let high = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * si;
    low + (high - low) * sj
}

/// The streaks of the gas at `gas` when the outer edge has gone `laps` of
/// the way round: long arcs, thin across, that each go round at their own
/// radius's pace.
fn streaks(gas: &Gas, laps: f64, seed: u64) -> f32 {
    // Taken to a fraction of a turn first, in double precision, since the
    // laps since the epoch run to millions.
    let gone = (laps * f64::from(gas.pace)).rem_euclid(1.0) as f32;
    let round = (gas.turn + gone).rem_euclid(1.0);
    let octave = |salt: u64, across: f32, around: i64| {
        noise(
            seed,
            salt,
            gas.radius * across,
            round * around as f32,
            around,
        )
    };
    0.5 * octave(0xD15C, 3.0, 4) + 0.3 * octave(0x6A5, 7.0, 9) + 0.2 * octave(0x5EED, 15.0, 19)
}

/// The colours the hole is drawn in: values where the terminal draws them,
/// the sixteen names where it does not.
#[derive(Debug)]
enum Ink {
    Deep(Deep),
    Sixteen(Sixteen),
}

/// The scene's colours as values, each a share of full red, green and blue.
#[derive(Debug)]
struct Deep {
    sky: Vector,
    hole: Vector,
    /// The disk from coolest to hottest: an ember, amber, gold and white.
    ramp: [Vector; 4],
    star: Vector,
    warm_star: Vector,
    cool_star: Vector,
}

/// The scene's colours in the sixteen names.
#[derive(Debug)]
struct Sixteen {
    sky: Color,
    hole: Color,
    /// The disk from dimmest to brightest.
    ramp: [Color; 4],
}

/// A colour as shares of full red, green and blue, where it is a value.
fn shares(colour: Color) -> Option<Vector> {
    rgb(colour).map(|(r, g, b)| [r, g, b].map(|channel| f32::from(channel) / 255.0))
}

/// Shares of full red, green and blue as a value.
fn value(colour: Vector) -> Color {
    // Clamped to a share of one first, so the product is within a byte.
    let [r, g, b] = colour.map(|share| (share.clamp(0.0, 1.0) * 255.0).round() as u8);
    Color::Rgb(r, g, b)
}

const WHITE: Vector = [1.0, 0.98, 0.94];

impl Ink {
    fn of(theme: &Theme) -> Self {
        let values = [
            theme.pane_bg,
            theme.del,
            theme.warn,
            theme.title,
            theme.fg,
            theme.hot,
        ]
        .map(shares);
        match values {
            [
                Some(pane),
                Some(del),
                Some(warn),
                Some(title),
                Some(fg),
                Some(hot),
            ] => Self::Deep(Deep {
                sky: pane,
                // Darker than the sky: the sky has starlight in it, the
                // shadow nothing.
                hole: scale(pane, 0.35),
                ramp: [
                    scale(lerp(del, warn, 0.35), 0.75),
                    lerp(del, warn, 0.75),
                    title,
                    lerp(title, WHITE, 0.6),
                ],
                star: lerp(fg, WHITE, 0.5),
                warm_star: lerp(title, WHITE, 0.3),
                cool_star: lerp(hot, WHITE, 0.4),
            }),
            _ => Self::Sixteen(Sixteen {
                sky: theme.pane_bg,
                hole: Color::Black,
                ramp: [
                    darker(theme.del),
                    darker(theme.title),
                    theme.title,
                    Color::White,
                ],
            }),
        }
    }

    /// The colour of a pixel that saw `seen`, at `laps` of the outer edge's
    /// orbit and `millis`.
    fn colour(&self, seen: Seen, laps: f64, millis: u64, seed: u64) -> Color {
        match (self, seen) {
            (Self::Deep(deep), Seen::Hole) => value(deep.hole),
            (Self::Sixteen(sixteen), Seen::Hole) => sixteen.hole,
            (_, Seen::Sky { cell }) => self.sky(cell, millis, seed),
            (_, Seen::Ring { glow }) => self.gas(stepped(0.25 + 0.75 * glow), 0.7),
            (_, Seen::Disk(gas)) => {
                let light = stepped(EXPOSURE * gas.shine * (0.5 + 0.7 * streaks(&gas, laps, seed)));
                self.gas(light, stepped(gas.heat))
            }
        }
    }

    /// The colour of the disk at `light`, as hot as `heat`.
    fn gas(&self, light: f32, heat: f32) -> Color {
        match self {
            Self::Deep(deep) => {
                let hue = ramp(&deep.ramp, heat.clamp(0.0, 1.0));
                let lit = scale(hue, light.min(1.0));
                let over = scale(WHITE, 0.6 * (light - 1.0).max(0.0));
                value(add(lerp(deep.sky, lit, light.min(1.0)), over))
            }
            Self::Sixteen(sixteen) => match light {
                l if l < 0.2 => sixteen.sky,
                l if l < 0.32 => sixteen.ramp[0],
                l if l < 0.6 => sixteen.ramp[1],
                l if l < 1.0 => sixteen.ramp[2],
                _ => sixteen.ramp[3],
            },
        }
    }

    /// The sky at `cell`: the pane's colour, or a star, a few of which
    /// twinkle at a time.
    fn sky(&self, cell: (i32, i32), millis: u64, seed: u64) -> Color {
        let n = (u64::from(cell.0 as u32) << 32) | u64::from(cell.1 as u32);
        let star = key(seed, 0x57A2, n);
        let sky = match self {
            Self::Deep(deep) => value(deep.sky),
            Self::Sixteen(sixteen) => sixteen.sky,
        };
        if star % 1000 >= STARS {
            return sky;
        }
        // Each star dims for a moment now and then, on a clock of its own.
        let slot = millis.wrapping_add(star % 4_000) / 1_300;
        let twinkle = key(seed, 0x7_1F1, n.wrapping_add(slot.wrapping_mul(0x9E37)));
        let bright = (star >> 20) % 100;
        let bright = match twinkle % 9 {
            0 => bright / 3,
            _ => bright,
        };
        match self {
            Self::Deep(deep) => {
                let tint = match (star >> 40) % 6 {
                    0 => deep.warm_star,
                    1 => deep.cool_star,
                    _ => deep.star,
                };
                // Rounded to a few steps, as the disk is.
                let share = stepped(0.3 + 0.7 * bright as f32 / 100.0);
                value(lerp(deep.sky, tint, share))
            }
            Self::Sixteen(sixteen) => match bright {
                b if b < 40 => match sixteen.sky == Color::DarkGray {
                    true => Color::Gray,
                    false => Color::DarkGray,
                },
                b if b < 80 => Color::Gray,
                _ => Color::White,
            },
        }
    }
}

/// The colour `heat` of the way along `ramp`, its four stops evenly spaced.
fn ramp(ramp: &[Vector; 4], heat: f32) -> Vector {
    let at = heat * 3.0;
    // Within the ramp: heat is clamped to one, so `at` is at most three.
    let low = (at.floor() as usize).min(2);
    lerp(ramp[low], ramp[low + 1], at - low as f32)
}

/// Draws the hole into `area` of `buffer` as it stands `millis` after the
/// origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let seen = chart(area.width, area.height);
    let ink = Ink::of(theme);
    let shift = key(seed, 0x1A9, 0) % 1_000_000;
    // Laps of the outer edge, whose orbit is the inner edge's lap times its
    // pace there.
    let outer_lap = LAP * f64::from((OUTER / INNER).powf(1.5));
    let laps = (millis.wrapping_add(shift)) as f64 / outer_lap;
    let width = usize::from(area.width);
    let colour = |row: usize, column: usize| {
        seen.get(row * width + column)
            .map_or(theme.pane_bg, |seen| ink.colour(*seen, laps, millis, seed))
    };
    for row in 0..area.height {
        for column in 0..area.width {
            let (y, x) = (usize::from(row) * 2, usize::from(column));
            let (upper, lower) = (colour(y, x), colour(y + 1, x));
            let (glyph, style) = match upper == lower {
                true => (' ', Style::new().bg(upper)),
                false => ('▀', Style::new().fg(upper).bg(lower)),
            };
            buffer[(area.x + column, area.y + row)]
                .set_char(glyph)
                .set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::theme::{Depth, STELLAR};

    const WIDTH: u16 = 90;
    const HEIGHT: u16 = 30;

    fn hole(theme: &Theme, millis: u64, seed: u64) -> Buffer {
        let area = Rect::new(0, 0, WIDTH, HEIGHT);
        let mut buffer = Buffer::empty(area);
        super::super::draw(
            &mut buffer,
            area,
            Duration::from_millis(millis),
            seed,
            theme,
        );
        buffer
    }

    /// What the pixel `column` across and `row` down sees, in a 90 by 30
    /// pane, two pixels to a row.
    fn seen(column: u16, row: u16) -> Seen {
        let seen = chart(WIDTH, HEIGHT);
        seen[usize::from(row) * usize::from(WIDTH) + usize::from(column)]
    }

    fn is_disk(seen: Seen) -> bool {
        matches!(seen, Seen::Disk(_))
    }

    /// Down the middle of the pane: the halo the disk's far side makes over
    /// the shadow, the shadow, the near side of the disk across it, the
    /// shadow again and the halo under it — and the stars above and below.
    #[test]
    fn down_the_middle_the_disk_is_bent_over_and_under_a_dark_shadow() {
        let middle: Vec<Seen> = (0..2 * HEIGHT).map(|row| seen(WIDTH / 2, row)).collect();
        let first_dark = middle
            .iter()
            .position(|s| *s == Seen::Hole)
            .expect("the shadow crosses the middle");
        let last_dark = middle
            .iter()
            .rposition(|s| *s == Seen::Hole)
            .expect("the shadow crosses the middle");
        assert!(
            middle[..first_dark].iter().any(|s| is_disk(*s)),
            "no halo over the shadow"
        );
        assert!(
            middle[last_dark..].iter().any(|s| is_disk(*s)),
            "no halo under the shadow"
        );
        assert!(matches!(middle[0], Seen::Sky { .. }), "{:?}", middle[0]);
        assert!(matches!(middle[middle.len() - 1], Seen::Sky { .. }));
        let shadow = last_dark - first_dark;
        assert!(
            (8..40).contains(&shadow),
            "the shadow is {shadow} pixels tall"
        );
    }

    /// The halo over the shadow is the far side of the disk, behind the hole:
    /// a ray that would have passed over the hole to the stars was bent down
    /// onto it.
    #[test]
    fn the_halo_over_the_shadow_is_the_far_side_of_the_disk_bent_into_view() {
        let camera = Camera::new();
        let per = (f32::from(WIDTH) / 2.0 / HALF_WIDE).min(f32::from(2 * HEIGHT) / 2.0 / HALF_TALL);
        let (column, row) = (0..2 * HEIGHT)
            .map(|row| (WIDTH / 2, row))
            .find(|(column, row)| is_disk(seen(*column, *row)))
            .expect("a halo over the shadow");
        let Seen::Disk(gas) = seen(column, row) else {
            unreachable!("found as the disk");
        };
        // Behind the hole is away from the camera, which is at negative z.
        assert!((gas.turn * TAU).sin() > 0.5, "turn {}", gas.turn);
        let across = (f32::from(column) + 0.5 - f32::from(WIDTH) / 2.0) / per;
        let up = (f32::from(HEIGHT) - f32::from(row) - 0.5) / per;
        let straight = trace(camera.at, camera.way(across, up), false, per * DISTANCE);
        assert!(matches!(straight, Seen::Sky { .. }), "{straight:?}");
    }

    /// Across the middle the disk reaches well past the shadow on both
    /// sides, and the side coming toward the camera — the right, as the gas
    /// is carried — is the brighter.
    #[test]
    fn the_disk_spreads_wide_of_the_shadow_and_its_approaching_side_is_brighter() {
        let row = HEIGHT;
        let shine = |columns: std::ops::Range<u16>| {
            columns
                .filter_map(|column| match seen(column, row) {
                    Seen::Disk(gas) => Some(gas.shine),
                    _ => None,
                })
                .fold((0.0_f32, 0_u32), |(sum, n), shine| (sum + shine, n + 1))
        };
        let (left, on_left) = shine(0..WIDTH / 2);
        let (right, on_right) = shine(WIDTH / 2..WIDTH);
        assert!(
            on_left > 20 && on_right > 20,
            "{on_left} and {on_right} pixels of disk"
        );
        assert!(right > left * 1.3, "left {left}, right {right}");
    }

    /// Light from the gas nearest the hole climbs out of its well, and the
    /// gas going away is shifted down: both are redder than the gas coming
    /// toward the camera.
    #[test]
    fn the_light_is_shifted_by_the_holes_well_and_the_gass_motion() {
        let toward = Camera::new().way(6.0, 0.0);
        let away = Camera::new().way(-6.0, 0.0);
        let at = Camera::new().at;
        let shift = |way| match trace(at, way, true, 100.0) {
            Seen::Disk(gas) => gas.shift,
            other => panic!("{other:?}"),
        };
        assert!(shift(toward) > 1.0, "{}", shift(toward));
        assert!(shift(away) < 1.0, "{}", shift(away));
    }

    #[test]
    fn a_ray_aimed_at_the_hole_falls_in_and_one_aimed_wide_leaves_for_the_stars() {
        let camera = Camera::new();
        assert_eq!(
            trace(camera.at, camera.way(0.0, 1.2), true, 100.0),
            Seen::Hole
        );
        assert!(matches!(
            trace(camera.at, camera.way(0.0, 20.0), true, 100.0),
            Seen::Sky { .. }
        ));
    }

    /// Where each pixel's ray ends is worked out once for a pane's size and
    /// kept: a second frame of the same size takes the same survey, and a
    /// pane resized and back finds it still kept.
    #[test]
    fn the_survey_of_a_pane_is_worked_out_once_for_its_size() {
        let first = chart(40, 12);
        assert!(Rc::ptr_eq(&first, &chart(40, 12)));
        let _ = chart(41, 12);
        assert!(Rc::ptr_eq(&first, &chart(40, 12)));
        assert_eq!(first.len(), 40 * 24);
    }

    /// No character stands for a shade: every cell is a blank or the upper
    /// half block, two pixels of colour one over the other.
    #[test]
    fn the_hole_is_drawn_in_pixels_of_colour_rather_than_characters() {
        for theme in [STELLAR, STELLAR.at(Depth::TrueColour)] {
            let drawn = hole(&theme, 4_000, 7);
            for cell in drawn.content() {
                assert!([" ", "▀"].contains(&cell.symbol()), "{:?}", cell.symbol());
            }
            let colours: std::collections::HashSet<Color> = drawn
                .content()
                .iter()
                .flat_map(|cell| [cell.fg, cell.bg])
                .collect();
            assert!(
                colours.len() >= 6,
                "{}: {} colours",
                theme.name,
                colours.len()
            );
        }
    }

    /// The disk flows and the stars twinkle, but the shadow stands still: a
    /// moment later, cells of the disk have changed and no pixel of the
    /// shadow has.
    #[test]
    fn the_disk_flows_round_a_shadow_that_stands_still() {
        let theme = STELLAR.at(Depth::TrueColour);
        let (now, later) = (hole(&theme, 10_000, 7), hole(&theme, 11_000, 7));
        let changed = now
            .content()
            .iter()
            .zip(later.content())
            .filter(|(was, is)| was != is)
            .count();
        assert!(changed > 20, "{changed} cells changed");
        let shadow = value(scale(shares(theme.pane_bg).expect("a value"), 0.35));
        for (was, is) in now.content().iter().zip(later.content()) {
            if was.bg == shadow && was.fg == shadow {
                assert_eq!(was, is);
            }
        }
    }

    /// The disk moves only where a step of its light changes, so a frame
    /// rewrites a share of the pane rather than all of it.
    #[test]
    fn a_frame_rewrites_only_some_of_the_pane() {
        for theme in [STELLAR, STELLAR.at(Depth::TrueColour)] {
            for millis in [0, 30_000, 3_600_000] {
                let now = hole(&theme, millis, 7);
                let next = hole(&theme, millis + 100, 7);
                let changed = now
                    .content()
                    .iter()
                    .zip(next.content())
                    .filter(|(was, is)| was != is)
                    .count();
                let all = usize::from(WIDTH) * usize::from(HEIGHT);
                assert!(
                    changed * 5 < all,
                    "{}: {changed} of {all} cells",
                    theme.name
                );
            }
        }
    }

    /// In the sixteen names the hole is drawn in the theme's own and black;
    /// where the terminal draws values, in values mixed from the theme's.
    #[test]
    fn the_hole_is_drawn_in_the_themes_own_colours() {
        let sixteen = hole(&STELLAR, 2_000, 7);
        let colours: Vec<Color> = sixteen
            .content()
            .iter()
            .flat_map(|cell| [cell.fg, cell.bg])
            .collect();
        for colour in [STELLAR.title, STELLAR.pane_bg, Color::White] {
            assert!(colours.contains(&colour), "{colour:?}");
        }
        assert!(
            !colours
                .iter()
                .any(|colour| matches!(colour, Color::Rgb(..)))
        );
        let deep = hole(&STELLAR.at(Depth::TrueColour), 2_000, 7);
        assert!(
            deep.content()
                .iter()
                .all(|cell| matches!(cell.bg, Color::Rgb(..)))
        );
    }

    #[test]
    fn noise_closes_up_round_the_disk() {
        for along in [0.3, 2.7, 9.1] {
            let start = noise(7, 1, along, 0.0, 15);
            let round = noise(7, 1, along, 15.0, 15);
            assert!((start - round).abs() < 1e-6);
            assert!((0.0..=1.0).contains(&noise(7, 1, along, 4.4, 15)));
        }
    }
}
