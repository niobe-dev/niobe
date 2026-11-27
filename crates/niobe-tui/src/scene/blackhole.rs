// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The black hole `stellar` shows in the session pane before anything has
//! happened in it: a hole seen from just above the plane of its accretion
//! disk, the disk's far side bent up over the shadow and down under it, a
//! thin ring of light hugging the shadow's edge, the stars behind it pulled
//! aside, and the disk's gas flowing round, faster the nearer it is. The
//! camera is banked as the film's was, so the disk rises from the pane's
//! lower left to its upper right, round a hole that sits right of the middle.
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
//! than in characters. A star is the exception: a pixel of it would be a
//! square, or two a bar, so a cell whose pixels both see the sky draws the
//! brighter star either sees as a point — `·`, `•` or `+` by how bright —
//! once for each image of it the hole makes. Every light is one of a few
//! steps, so a frame rewrites only the cells whose step changed. In the
//! sixteen names the disk is the theme's red, darker gold, gold and white by
//! how bright it is, and the stars white and grey.

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

/// How far the camera is banked round the way it faces, in radians: about
/// twenty degrees, turned so the disk rises to the right, as the film's
/// camera was banked over its hole.
const ROLL: f32 = -0.35;

/// Where the disk starts and ends, in the horizon's radius: the innermost
/// orbit gas can keep round a non-rotating hole, and out to ten.
const INNER: f32 = 3.0;
const OUTER: f32 = 10.0;

/// How much of the scene the pane holds at the least, either side of the
/// hole and above and below it, at the hole's distance: the shadow and the
/// halo round it whole, and the disk out to where it runs off the pane.
const HALF_WIDE: f32 = 8.6;
const HALF_TALL: f32 = 5.4;

/// Where the hole is in the pane, as a share of its width from the left and
/// of its height from the top: right of the middle, as the film frames it,
/// so the disk's near side sweeps down to the lower left, away from the
/// greeting over the pane's top left.
const FOCUS: (f32, f32) = (0.58, 0.5);

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

/// How far from the point straight behind the hole, in radians, the sky
/// holds no star. The hole spreads a star there into an arc round its
/// shadow, or a whole ring — half the ring's radius, `√(2 / DISTANCE)`, out
/// it is stretched to twice its length — and a terminal's pixels, too few
/// to draw so thin an arc, catch it at a few places that read as a cluster
/// of stars, or a circle drawn on the pane.
const CLEAR: f32 = 0.14;

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
    /// an arc where it did; `None` within [`CLEAR`] of straight behind the
    /// hole, where no star is drawn.
    Sky { cell: Option<(i32, i32)> },
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
        let level = unit(cross([0.0, 1.0, 0.0], forward));
        let upright = cross(forward, level);
        let (sin, cos) = ROLL.sin_cos();
        let right = add(scale(level, cos), scale(upright, sin));
        let up = add(scale(upright, cos), scale(level, -sin));
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
/// cells to a radian: about a pixel's worth of sky each. `None` within
/// [`CLEAR`] of straight behind the hole.
fn sky_cell(way: Vector, per_radian: f32) -> Option<(i32, i32)> {
    let way = unit(way);
    let behind = scale(Camera::new().at, -1.0 / DISTANCE);
    if dot(way, behind) > CLEAR.cos() {
        return None;
    }
    let east = way[0].atan2(way[2]);
    let north = way[1].clamp(-1.0, 1.0).asin();
    // Both within a turn, times at most a few thousand pixels to a radian.
    Some((
        (east * per_radian).floor() as i32,
        (north * per_radian).floor() as i32,
    ))
}

/// How a pane's pixels are laid over the scene: how many pixels to the
/// horizon's radius, at the hole's distance, and the pixel the hole is at.
#[derive(Debug, Clone, Copy)]
struct Frame {
    per: f32,
    centre: (f32, f32),
}

impl Frame {
    /// The frame of a pane `width` columns by `height` rows, two pixels to
    /// a cell one over the other; `None` for a pane with no pixels.
    fn of(width: u16, height: u16) -> Option<Self> {
        let (columns, rows) = (f32::from(width), 2.0 * f32::from(height));
        let per = (columns / 2.0 / HALF_WIDE).min(rows / 2.0 / HALF_TALL);
        (per > 0.0).then_some(Self {
            per,
            centre: (columns * FOCUS.0, rows * FOCUS.1),
        })
    }

    /// How far across and up of the hole, in the horizon's radius at its
    /// distance, the middle of the pixel `column` across and `row` down is.
    fn aim(&self, column: u16, row: u16) -> (f32, f32) {
        (
            (f32::from(column) + 0.5 - self.centre.0) / self.per,
            (self.centre.1 - f32::from(row) - 0.5) / self.per,
        )
    }
}

/// What every pixel of a pane `width` columns by `height` rows sees, two
/// pixels to a cell one over the other, a row of pixels at a time.
fn survey(width: u16, height: u16) -> Vec<Seen> {
    let Some(frame) = Frame::of(width, height) else {
        return Vec::new();
    };
    let camera = Camera::new();
    let per_radian = frame.per * DISTANCE;
    (0..2 * height)
        .flat_map(|row| (0..width).map(move |column| (row, column)))
        .map(|(row, column)| {
            let (across, up) = frame.aim(column, row);
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
    0.45 * octave(0xD15C, 1.4, 3) + 0.35 * octave(0x6A5, 2.8, 7) + 0.2 * octave(0x5EED, 5.0, 13)
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
    /// orbit: the sky is its own colour here, its stars being glyphs a cell
    /// holds whole ([`Ink::star`]).
    fn colour(&self, seen: Seen, laps: f64, seed: u64) -> Color {
        match (self, seen) {
            (Self::Deep(deep), Seen::Hole) => value(deep.hole),
            (Self::Sixteen(sixteen), Seen::Hole) => sixteen.hole,
            (_, Seen::Sky { .. }) => self.sky(),
            (_, Seen::Ring { glow }) => self.gas(stepped(0.25 + 0.75 * glow), 0.7),
            (_, Seen::Disk(gas)) => {
                let light = stepped(EXPOSURE * gas.shine * (0.5 + 0.7 * streaks(&gas, laps, seed)));
                self.gas(light, stepped(gas.heat))
            }
        }
    }

    /// The sky's own colour, with no star in it.
    fn sky(&self) -> Color {
        match self {
            Self::Deep(deep) => value(deep.sky),
            Self::Sixteen(sixteen) => sixteen.sky,
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

    /// The star at `cell` of the sky, if one is there, as it stands at
    /// `millis`: a few of them twinkle at a time.
    fn star(&self, cell: (i32, i32), millis: u64, seed: u64) -> Option<Star> {
        let n = (u64::from(cell.0 as u32) << 32) | u64::from(cell.1 as u32);
        let star = key(seed, 0x57A2, n);
        if star % 1000 >= STARS {
            return None;
        }
        // Each star dims for a moment now and then, on a clock of its own.
        let slot = millis.wrapping_add(star % 4_000) / 1_300;
        let twinkle = key(seed, 0x7_1F1, n.wrapping_add(slot.wrapping_mul(0x9E37)));
        let bright = (star >> 20) % 100;
        let bright = match twinkle % 9 {
            0 => bright / 3,
            _ => bright,
        };
        let colour = match self {
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
        };
        let glyph = match bright {
            b if b < 40 => '·',
            b if b < 80 => '•',
            _ => '+',
        };
        Some(Star {
            glyph,
            colour,
            bright,
        })
    }

    /// The cell whose upper pixel saw `upper` and lower one `lower`: two
    /// pixels of colour one over the other, or, where both saw the sky, the
    /// brighter of the stars they saw drawn as a glyph, so that a star is a
    /// point rather than a block half a cell tall. `firsts` says of each
    /// pixel whether its star is first seen there ([`first_sight`]).
    fn cell(
        &self,
        [upper, lower]: [Seen; 2],
        firsts: [bool; 2],
        laps: f64,
        millis: u64,
        seed: u64,
    ) -> (char, Style) {
        if let (Seen::Sky { cell: over }, Seen::Sky { cell: under }) = (upper, lower) {
            let star = [over, under]
                .into_iter()
                .zip(firsts)
                .filter(|(_, first)| *first)
                .filter_map(|(cell, _)| self.star(cell?, millis, seed))
                .max_by_key(|star| star.bright);
            return match star {
                Some(star) => (star.glyph, Style::new().fg(star.colour).bg(self.sky())),
                None => (' ', Style::new().bg(self.sky())),
            };
        }
        let (upper, lower) = (
            self.colour(upper, laps, seed),
            self.colour(lower, laps, seed),
        );
        match upper == lower {
            true => (' ', Style::new().bg(upper)),
            false => ('▀', Style::new().fg(upper).bg(lower)),
        }
    }
}

/// A star as a cell draws it: a point of light, bigger the brighter it is.
#[derive(Debug, Clone, Copy)]
struct Star {
    glyph: char,
    colour: Color,
    /// How bright, from nought to a hundred.
    bright: u64,
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
    let seen_at = |row: usize, column: usize| {
        seen.get(row * width + column)
            .copied()
            .unwrap_or(Seen::Hole)
    };
    for row in 0..area.height {
        for column in 0..area.width {
            let (y, x) = (usize::from(row) * 2, usize::from(column));
            let pixels = [seen_at(y, x), seen_at(y + 1, x)];
            let firsts = [y, y + 1].map(|y| first_sight(&seen, width, y, x));
            let (glyph, style) = ink.cell(pixels, firsts, laps, millis, seed);
            buffer[(area.x + column, area.y + row)]
                .set_char(glyph)
                .set_style(style);
        }
    }
}

/// Whether the pixel `column` across and `row` down, of a survey `width`
/// pixels wide, is where the image of the star it sees begins: no pixel
/// before it on its row, nor one touching it on the row above, sees the same
/// cell of the sky. The hole stretches a star near it into an arc of many
/// pixels, which, drawn as a glyph at each, would be a line of stars where
/// there is one.
fn first_sight(seen: &[Seen], width: usize, row: usize, column: usize) -> bool {
    let Some(&sky @ Seen::Sky { cell: Some(_) }) = seen.get(row * width + column) else {
        return false;
    };
    let before = [(0, 1), (1, 1), (1, 0), (1, -1)];
    !before.iter().any(|&(up, back)| {
        let (Some(row), Some(column)) = (row.checked_sub(up), column.checked_add_signed(-back))
        else {
            return false;
        };
        column < width && seen.get(row * width + column) == Some(&sky)
    })
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

    fn frame() -> Frame {
        Frame::of(WIDTH, HEIGHT).expect("a 90 by 30 pane has pixels")
    }

    /// How far across and up the camera's banked view the point `level`
    /// along the disk's plane, as it crosses the view, and `upright` of it
    /// is, in the horizon's radius at the hole's distance.
    fn banked(level: f32, upright: f32) -> (f32, f32) {
        let (sin, cos) = ROLL.sin_cos();
        (level * cos + upright * sin, upright * cos - level * sin)
    }

    /// The pixel the point `across` and `up` of the hole falls in, where it
    /// is in the pane.
    fn pixel((across, up): (f32, f32)) -> Option<(u16, u16)> {
        let frame = frame();
        let column = (frame.centre.0 + across * frame.per).floor();
        let row = (frame.centre.1 - up * frame.per).floor();
        let inside = (0.0..f32::from(WIDTH)).contains(&column)
            && (0.0..f32::from(2 * HEIGHT)).contains(&row);
        // Within the pane, so within a u16.
        inside.then_some((column as u16, row as u16))
    }

    /// What is seen along a line through the hole, a pixel's length apart,
    /// from `from` to `to` in the horizon's radius along it, `along` being
    /// where on the banked view each distance falls.
    fn line(from: f32, to: f32, along: impl Fn(f32) -> (f32, f32)) -> Vec<Seen> {
        let per = frame().per;
        // A few dozen steps: the distances are a few radii.
        let steps = ((to - from) * per).abs().ceil() as i32;
        (0..=steps)
            .filter_map(|step| pixel(along(from + (to - from) * step as f32 / steps as f32)))
            .map(|(column, row)| seen(column, row))
            .collect()
    }

    /// Across the shadow, upright to the disk: the halo the disk's far side
    /// makes over the shadow, the shadow, the near side of the disk across
    /// it, the shadow again and the halo under it — and the stars beyond.
    #[test]
    fn across_the_disk_it_is_bent_over_and_under_a_dark_shadow() {
        let middle = line(HALF_TALL, -HALF_TALL, |upright| banked(0.0, upright));
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
        assert!(
            middle[first_dark..last_dark].iter().any(|s| is_disk(*s)),
            "no near side across the shadow"
        );
        assert!(matches!(middle[0], Seen::Sky { .. }), "{:?}", middle[0]);
        assert!(matches!(middle[middle.len() - 1], Seen::Sky { .. }));
        let shadow = last_dark - first_dark;
        assert!(
            (8..40).contains(&shadow),
            "the shadow is {shadow} pixels across"
        );
    }

    /// The halo over the shadow is the far side of the disk, behind the hole:
    /// a ray that would have passed over the hole to the stars was bent down
    /// onto it.
    #[test]
    fn the_halo_over_the_shadow_is_the_far_side_of_the_disk_bent_into_view() {
        let camera = Camera::new();
        let frame = frame();
        let (column, row) = (0..=60)
            .filter_map(|step| pixel(banked(0.0, HALF_TALL - step as f32 / frame.per)))
            .find(|(column, row)| is_disk(seen(*column, *row)))
            .expect("a halo over the shadow");
        let Seen::Disk(gas) = seen(column, row) else {
            unreachable!("found as the disk");
        };
        // Behind the hole is away from the camera, which is at negative z.
        assert!((gas.turn * TAU).sin() > 0.5, "turn {}", gas.turn);
        let (across, up) = frame.aim(column, row);
        let straight = trace(
            camera.at,
            camera.way(across, up),
            false,
            frame.per * DISTANCE,
        );
        assert!(matches!(straight, Seen::Sky { .. }), "{straight:?}");
    }

    /// Along the disk the near side reaches well past the shadow on both
    /// sides, and the side coming toward the camera — the right, as the gas
    /// is carried — is the brighter.
    #[test]
    fn the_disk_spreads_wide_of_the_shadow_and_its_approaching_side_is_brighter() {
        let shine = |from: f32, to: f32| {
            line(from, to, |level| banked(level, 0.0))
                .into_iter()
                .filter_map(|seen| match seen {
                    Seen::Disk(gas) => Some(gas.shine),
                    _ => None,
                })
                .fold((0.0_f32, 0_u32), |(sum, n), shine| (sum + shine, n + 1))
        };
        let (left, on_left) = shine(-HALF_WIDE, 0.0);
        let (right, on_right) = shine(0.0, HALF_WIDE);
        assert!(
            on_left > 20 && on_right > 20,
            "{on_left} and {on_right} pixels of disk"
        );
        let (left, right) = (left / on_left as f32, right / on_right as f32);
        assert!(right > left * 1.3, "left {left}, right {right}");
    }

    /// The camera is banked as the film's was: the disk rises from the
    /// pane's lower left to its upper right, round a hole that sits right of
    /// the middle.
    #[test]
    fn the_disk_rises_to_the_right_round_a_hole_right_of_the_middle() {
        let seen = chart(WIDTH, HEIGHT);
        let frame = frame();
        let mean = |pick: &dyn Fn(Seen, f32) -> bool, of: &dyn Fn(u16, u16) -> u16| {
            let picked: Vec<f32> = (0..2 * HEIGHT)
                .flat_map(|row| (0..WIDTH).map(move |column| (column, row)))
                .filter(|&(column, row)| {
                    let at = usize::from(row) * usize::from(WIDTH) + usize::from(column);
                    pick(seen[at], frame.aim(column, row).0)
                })
                .map(|(column, row)| f32::from(of(column, row)))
                .collect();
            picked.iter().sum::<f32>() / picked.len().max(1) as f32
        };
        let row = |_: u16, row: u16| row;
        let left = mean(&|seen, across| is_disk(seen) && across < -5.0, &row);
        let right = mean(&|seen, across| is_disk(seen) && across > 5.0, &row);
        assert!(
            right + 4.0 < left,
            "rows {left} on the left, {right} on the right"
        );
        let shadow = mean(&|seen, _| seen == Seen::Hole, &|column, _| column);
        assert!(
            shadow > f32::from(WIDTH) / 2.0 + 2.0,
            "the shadow is about column {shadow}"
        );
    }

    /// Light from the gas nearest the hole climbs out of its well, and the
    /// gas going away is shifted down: both are redder than the gas coming
    /// toward the camera.
    #[test]
    fn the_light_is_shifted_by_the_holes_well_and_the_gass_motion() {
        let camera = Camera::new();
        let way = |(across, up)| camera.way(across, up);
        let shift = |way| match trace(camera.at, way, true, 100.0) {
            Seen::Disk(gas) => gas.shift,
            other => panic!("{other:?}"),
        };
        let (toward, away) = (way(banked(6.0, 0.0)), way(banked(-6.0, 0.0)));
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

    /// No character stands for a shade: the disk and the shadow are a blank
    /// or the upper half block, two pixels of colour one over the other, and
    /// a star is a point of light on the sky — never a block of colour, which
    /// a pixel half a cell tall draws as a square or, two of them, a bar.
    #[test]
    fn the_disk_is_drawn_in_pixels_of_colour_and_a_star_as_a_point_on_the_sky() {
        let seen = chart(WIDTH, HEIGHT);
        for theme in [STELLAR, STELLAR.at(Depth::TrueColour)] {
            let drawn = hole(&theme, 4_000, 7);
            let sky = Ink::of(&theme).sky();
            let mut stars = 0;
            for row in 0..HEIGHT {
                for column in 0..WIDTH {
                    let cell = &drawn[(column, row)];
                    let at =
                        |y: u16| seen[usize::from(y) * usize::from(WIDTH) + usize::from(column)];
                    let both_sky = matches!(
                        (at(2 * row), at(2 * row + 1)),
                        (Seen::Sky { .. }, Seen::Sky { .. })
                    );
                    match both_sky {
                        true => {
                            assert!(
                                [" ", "·", "•", "+"].contains(&cell.symbol()),
                                "{:?}",
                                cell.symbol()
                            );
                            assert_eq!(cell.bg, sky, "{}: ({column}, {row})", theme.name);
                            stars += usize::from(cell.symbol() != " ");
                        }
                        false => {
                            assert!([" ", "▀"].contains(&cell.symbol()), "{:?}", cell.symbol())
                        }
                    }
                }
            }
            assert!(stars >= 5, "{}: {stars} stars", theme.name);
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

    /// A star the hole stretches into an arc of touching pixels is drawn at
    /// the arc's first pixel alone; a second image of it, apart from the
    /// first, is drawn too.
    #[test]
    fn a_star_stretched_into_an_arc_is_drawn_once_for_each_image_of_it() {
        let star = Seen::Sky { cell: Some((5, 9)) };
        let other = Seen::Sky { cell: Some((6, 9)) };
        let width = 5;
        #[rustfmt::skip]
        let seen = [
            other, other, star,  other, other,
            other, star,  star,  other, other,
            star,  other, other, other, star,
            other, other, other, other, other,
        ];
        let firsts: Vec<(usize, usize)> = (0..4)
            .flat_map(|row| (0..width).map(move |column| (row, column)))
            .filter(|&(row, column)| seen[row * width + column] == star)
            .filter(|&(row, column)| first_sight(&seen, width, row, column))
            .collect();
        assert_eq!(firsts, [(0, 2), (2, 4)]);
    }

    /// The sky straight behind the hole holds no star, which the hole would
    /// spread into a ring round its shadow; a little wider of it, it does.
    #[test]
    fn no_star_is_drawn_straight_behind_the_hole() {
        let behind = scale(Camera::new().at, -1.0);
        assert_eq!(sky_cell(behind, 100.0), None);
        let (sin, cos) = (1.2 * CLEAR).sin_cos();
        let wide = add(scale(unit(behind), cos), [sin, 0.0, 0.0]);
        assert!(sky_cell(wide, 100.0).is_some());
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
