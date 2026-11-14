// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The cyborg `modern` shows in the session pane before anything has
//! happened in it: a head in three dimensions drawn in ASCII, half of it
//! plated in metal round a glowing red eye, turning gently from side to side,
//! breaking into a grin and winking its human eye while it does.
//!
//! The head is a handful of ellipsoids — skull, jaw, nose, ears, eyes — and
//! a neck, each met by a ray cast from every cell of the pane straight into
//! it, so the picture is worked out whole for the moment drawn. Where a ray
//! meets the head, the light falling on that point picks a character from a
//! ramp of denser ones and a shade of the part's colour; the face's features
//! — the seams of the plate, the brow, the lips, the line a closing lid
//! leaves — are painted on the surface where the ray meets it. A cell is
//! about twice as tall as it is wide, so the head is laid out two columns to
//! a row.
//!
//! Every shade is one of a few steps of light rather than a value of its
//! own, so a cell is rewritten only when the turn of the head moves it to
//! another step, not every frame.

use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{darker, faded, key, rgb};
use crate::theme::Theme;

/// The characters a lit surface is drawn in, darkest first.
const RAMP: [char; 9] = ['.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// How tall the pane's picture is, in the units the head is built in: from
/// above the antenna's tip to the neck under the jaw.
const TALL: f32 = 2.4;

/// How wide the picture is at the least, in the same units: the head turned
/// with its ears and room to spare.
const WIDE: f32 = 2.3;

/// The height in the head's units the middle of the pane is at.
const MIDDLE: f32 = -0.05;

/// How long the head takes to turn from one side to the other and back,
/// in milliseconds.
const TURN: u64 = 7_000;

/// How long a nod takes, in milliseconds.
const NOD: u64 = 9_000;

/// How long the face takes to go from straight to a grin, wink and back,
/// in milliseconds.
const MOOD: u64 = 9_000;

/// How long the red eye takes to brighten and dim, in milliseconds.
const PULSE: u64 = 1_500;

/// How long the antenna's light takes to blink, in milliseconds.
const BLINK: u64 = 1_000;

/// Where the human eye is, on the head's right; the red eye is its mirror.
const EYE: [f32; 3] = [0.27, 0.2, 0.6];

/// How big an eye is.
const EYE_RADIUS: f32 = 0.14;

/// How big the lid over the human eye is: just bigger than the eye, so it
/// covers it as it closes.
const LID_RADIUS: f32 = 0.155;

/// Where the mouth's middle is, how wide it is at its straightest and how
/// thick its line is drawn.
const MOUTH: (f32, f32, f32) = (-0.4, 0.2, 0.028);

/// What a ray met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Skin,
    Metal,
    Neck,
    /// The red eye, on the plated side.
    Lens,
    /// The human eye.
    Eye,
    /// The lid over the human eye.
    Lid,
    /// The light on the antenna's tip.
    Tip,
}

/// What a point of the surface is drawn as, once the face's features are
/// painted on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paint {
    Skin,
    Metal,
    /// A ridge of the neck's metal.
    Rib,
    /// A seam of the plate, the ring round the red eye or the brow.
    Line(char),
    /// The lips.
    Lips(char),
    Teeth,
    /// The dark of an open mouth.
    Gap,
    Lens {
        core: bool,
    },
    White,
    Pupil,
    Lash,
    Tip,
}

type Vector = [f32; 3];

fn add(a: Vector, b: Vector) -> Vector {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: Vector, b: Vector) -> Vector {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: Vector, by: f32) -> Vector {
    [a[0] * by, a[1] * by, a[2] * by]
}

fn dot(a: Vector, b: Vector) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn unit(a: Vector) -> Vector {
    let length = dot(a, a).sqrt();
    match length > f32::EPSILON {
        true => scale(a, 1.0 / length),
        false => [0.0, 0.0, 1.0],
    }
}

/// A part of the head shaped as an ellipsoid: its centre, its three radii
/// and what it is.
#[derive(Debug, Clone, Copy)]
struct Ellipsoid {
    centre: Vector,
    radii: Vector,
    part: Part,
}

impl Ellipsoid {
    const fn new(centre: Vector, radii: Vector, part: Part) -> Self {
        Self {
            centre,
            radii,
            part,
        }
    }

    /// How far along the ray from `from` going `way` it first meets the
    /// surface, where it does ahead of `from`.
    fn meet(&self, from: Vector, way: Vector) -> Option<f32> {
        let r = self.radii;
        let o = sub(from, self.centre);
        let o = [o[0] / r[0], o[1] / r[1], o[2] / r[2]];
        let d = [way[0] / r[0], way[1] / r[1], way[2] / r[2]];
        let (a, b, c) = (dot(d, d), dot(o, d), dot(o, o) - 1.0);
        let disc = b * b - a * c;
        if disc < 0.0 || a <= f32::EPSILON {
            return None;
        }
        let near = (-b - disc.sqrt()) / a;
        (near > 0.0).then_some(near)
    }

    fn normal(&self, at: Vector) -> Vector {
        let p = sub(at, self.centre);
        let r = self.radii;
        unit([
            p[0] / (r[0] * r[0]),
            p[1] / (r[1] * r[1]),
            p[2] / (r[2] * r[2]),
        ])
    }
}

/// The parts of the head, in its own frame: across to its right, up, and
/// out of its face.
const PARTS: [Ellipsoid; 10] = [
    Ellipsoid::new([0.0, 0.18, -0.05], [0.7, 0.88, 0.8], Part::Skin),
    Ellipsoid::new([0.0, -0.36, 0.08], [0.52, 0.46, 0.6], Part::Skin),
    Ellipsoid::new([0.0, -0.05, 0.71], [0.08, 0.17, 0.12], Part::Skin),
    Ellipsoid::new([0.69, 0.04, -0.05], [0.07, 0.19, 0.13], Part::Skin),
    Ellipsoid::new([-0.69, 0.06, -0.05], [0.13, 0.22, 0.22], Part::Metal),
    Ellipsoid::new([-0.74, 0.52, -0.08], [0.035, 0.3, 0.035], Part::Metal),
    Ellipsoid::new([-0.74, 0.86, -0.08], [0.07, 0.07, 0.07], Part::Tip),
    Ellipsoid::new(
        [-EYE[0], EYE[1], EYE[2]],
        [EYE_RADIUS, EYE_RADIUS, EYE_RADIUS],
        Part::Lens,
    ),
    Ellipsoid::new(EYE, [EYE_RADIUS, EYE_RADIUS, EYE_RADIUS], Part::Eye),
    Ellipsoid::new(EYE, [LID_RADIUS, LID_RADIUS, LID_RADIUS], Part::Lid),
];

/// The neck: an upright cylinder under the jaw, running off the pane.
const NECK: (f32, f32, f32) = (0.3, -0.1, -0.55);

/// How the head stands and what its face is doing at a moment.
#[derive(Debug, Clone, Copy)]
struct Pose {
    /// Turned to its right by this many radians.
    yaw: f32,
    /// Tipped back by this many radians.
    pitch: f32,
    /// How far into a grin, from nought to one.
    grin: f32,
    /// How far the human eye has closed, from nought to one.
    wink: f32,
    /// How bright the red eye is, from nought to one.
    glow: f32,
    /// Whether the antenna's light is on.
    tip: bool,
}

/// Where `millis` falls in a cycle `period` long, shifted by the seed so two
/// sessions do not move in step, from nought to one.
fn phase(seed: u64, salt: u64, millis: u64, period: u64) -> f32 {
    let shift = key(seed, salt, 0) % period;
    let into = (millis % period + shift) % period;
    // Both below the period, which is a few thousand, so neither loses
    // anything as a float.
    into as f32 / period as f32
}

/// Nought before `from`, one after `to` and a smooth rise between.
fn smooth(from: f32, to: f32, at: f32) -> f32 {
    let t = ((at - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Pose {
    fn at(seed: u64, millis: u64) -> Self {
        let turn = phase(seed, 0x7_0A, millis, TURN);
        let nod = phase(seed, 0x40D, millis, NOD);
        let mood = phase(seed, 0x300D, millis, MOOD);
        let pulse = phase(seed, 0x9_1E, millis, PULSE);
        let blink = phase(seed, 0xB1_1C, millis, BLINK);
        Self {
            yaw: 0.42 * (turn * TAU).sin(),
            pitch: 0.07 * (nod * TAU).sin(),
            grin: smooth(0.16, 0.26, mood) * (1.0 - smooth(0.72, 0.81, mood)),
            wink: wink(mood),
            glow: 0.5 + 0.5 * (pulse * TAU).sin(),
            tip: blink < 0.5,
        }
    }

    /// `v` in the world turned into the head's own frame: the head's turn
    /// and tip undone.
    fn seen_from_head(&self, v: Vector) -> Vector {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // Undo the turn about the upright axis, then the tip about the
        // axis across.
        let x = cy * v[0] - sy * v[2];
        let z = sy * v[0] + cy * v[2];
        let y = cp * v[1] + sp * z;
        let z = -sp * v[1] + cp * z;
        [x, y, z]
    }
}

/// How far the human eye is closed at `mood` into the face's cycle: shut
/// quickly while the grin is at its widest, held, and opened a little
/// slower.
fn wink(mood: f32) -> f32 {
    let (start, shut, held, open) = (0.42, 0.435, 0.47, 0.49);
    match mood {
        at if at < start || at >= open => 0.0,
        at if at < shut => (at - start) / (shut - start),
        at if at < held => 1.0,
        at => 1.0 - (at - held) / (open - held),
    }
}

/// Where the ray from `from` going `way` meets the head, the part it meets
/// and the surface's normal there, in the head's frame.
fn cast(from: Vector, way: Vector, pose: &Pose) -> Option<(Vector, Part, Vector)> {
    let mut nearest: Option<(f32, Part, Vector)> = None;
    let mut keep = |t: f32, part: Part, normal: Vector| {
        if nearest.is_none_or(|(best, _, _)| t < best) {
            nearest = Some((t, part, normal));
        }
    };
    for part in &PARTS {
        let Some(t) = part.meet(from, way) else {
            continue;
        };
        let at = add(from, scale(way, t));
        if part.part == Part::Lid && !lidded(at[1], pose.wink) {
            continue;
        }
        keep(t, part.part, part.normal(at));
    }
    if let Some((t, normal)) = neck(from, way) {
        keep(t, Part::Neck, normal);
    }
    nearest.map(|(t, part, normal)| (add(from, scale(way, t)), part, normal))
}

/// The lid's edges at `wink`: it covers the eye above the first and below
/// the second, which meet a little under the eye's middle as it shuts.
fn lid_edges(wink: f32) -> (f32, f32) {
    let top = EYE[1] + LID_RADIUS - wink * LID_RADIUS * 1.35;
    let bottom = EYE[1] - LID_RADIUS + wink * LID_RADIUS * 0.65;
    (top, bottom)
}

fn lidded(y: f32, wink: f32) -> bool {
    let (top, bottom) = lid_edges(wink);
    y > top || y < bottom
}

/// Where the ray meets the neck, and its normal there.
fn neck(from: Vector, way: Vector) -> Option<(f32, Vector)> {
    let (radius, depth, top) = NECK;
    let (ox, oz) = (from[0], from[2] - depth);
    let (dx, dz) = (way[0], way[2]);
    let a = dx * dx + dz * dz;
    let b = ox * dx + oz * dz;
    let c = ox * ox + oz * oz - radius * radius;
    let disc = b * b - a * c;
    if disc < 0.0 || a <= f32::EPSILON {
        return None;
    }
    let t = (-b - disc.sqrt()) / a;
    let at = add(from, scale(way, t));
    (t > 0.0 && at[1] < top).then(|| (t, unit([at[0], 0.0, at[2] - depth])))
}

/// What the surface at `at` is drawn as, given the part the ray met.
fn paint(part: Part, at: Vector, toward: Vector, pose: &Pose) -> Paint {
    let y = at[1];
    match part {
        Part::Tip => Paint::Tip,
        Part::Neck => match (y * 7.0).rem_euclid(1.0) < 0.3 {
            true => Paint::Rib,
            false => Paint::Metal,
        },
        Part::Metal => Paint::Metal,
        Part::Lens => match off_axis(at, [-EYE[0], EYE[1], EYE[2]], toward) < 0.065 {
            true => Paint::Lens { core: true },
            false => Paint::Lens { core: false },
        },
        Part::Eye => match off_axis(at, EYE, toward) < 0.06 {
            true => Paint::Pupil,
            false => Paint::White,
        },
        Part::Lid => match (y - lid_edges(pose.wink).0).abs() < 0.035 {
            true => Paint::Lash,
            false => Paint::Skin,
        },
        Part::Skin => face(at, pose),
    }
}

/// How far `at`, on an eye centred at `centre`, is from the point of it that
/// looks along `toward`: the eyes follow the operator as the head turns.
fn off_axis(at: Vector, centre: Vector, toward: Vector) -> f32 {
    let looking = add(centre, scale(toward, EYE_RADIUS));
    let gap = sub(at, looking);
    dot(gap, gap).sqrt()
}

/// What the skin of the face shows at `at`: the plate on its left, its
/// seams and the ring round the red eye, the brow over the human eye, and
/// the mouth.
fn face(at: Vector, pose: &Pose) -> Paint {
    let [x, y, z] = at;
    if z > 0.3
        && let Some(mouth) = mouth(x, y, pose.grin)
    {
        return mouth;
    }
    let plated = x < -0.03 && y > -0.12;
    let lens = [-EYE[0], EYE[1]];
    let ring = (x - lens[0]).hypot(y - lens[1]);
    if plated && z > 0.3 && (0.16..0.21).contains(&ring) {
        return Paint::Line(match (x - lens[0]).abs() > (y - lens[1]).abs() * 1.4 {
            true if x < lens[0] => '(',
            true => ')',
            false => '-',
        });
    }
    let seam = ((x + 0.03).abs() < 0.018 && y > -0.12)
        || ((y + 0.12).abs() < 0.02 && x < -0.03)
        || ((y - 0.44).abs() < 0.02 && (-0.46..-0.08).contains(&x));
    if seam {
        let upright = (x + 0.03).abs() < 0.018;
        return Paint::Line(if upright { '|' } else { '-' });
    }
    if plated {
        return Paint::Metal;
    }
    let brow = EYE[1] + 0.27 - 1.2 * (x - EYE[0]).powi(2) - 0.05 * pose.wink;
    if z > 0.3 && (x - EYE[0]).abs() < 0.17 && (y - brow).abs() < 0.028 {
        return Paint::Line(slope(-2.4 * (x - EYE[0]), '-'));
    }
    Paint::Skin
}

/// What the mouth shows at `(x, y)` on the face, if it is there: the upper
/// lip's line, curving up at its corners the wider the grin, and once the
/// grin is wide, teeth and the dark of the open mouth under it.
fn mouth(x: f32, y: f32, grin: f32) -> Option<Paint> {
    let (middle, half, thick) = MOUTH;
    let half = half + 0.08 * grin;
    if x.abs() > half {
        return None;
    }
    let curve = 0.15 + 1.9 * grin;
    let lip = middle + curve * x * x;
    let rise = 2.0 * curve * x;
    // Measured across the line rather than straight down, so a corner
    // curling up steeply is drawn as unbroken as the middle.
    if (y - lip).abs() < thick * rise.hypot(1.0) {
        return Some(Paint::Lips(slope(rise, '_')));
    }
    let open = (grin - 0.45).max(0.0) * 0.26 * (1.0 - (x / half).powi(2));
    let below = lip - y;
    match below > 0.0 && below < open {
        true if below < 0.06 => Some(Paint::Teeth),
        true => Some(Paint::Gap),
        false => None,
    }
}

/// The line that runs most nearly at `rise`, up over across in the head's
/// units, a cell being twice as tall as it is wide: `flat` where it is near
/// level.
fn slope(rise: f32, flat: char) -> char {
    match rise / 2.0 {
        cells if cells > 0.3 => '/',
        cells if cells < -0.3 => '\\',
        _ => flat,
    }
}

/// The colours the head is drawn in.
struct Ink {
    deep: bool,
    pane: Color,
    skin: Color,
    metal: Color,
    line: Color,
    lips: Color,
    lens: Color,
    iris: Color,
    white: Color,
}

impl Ink {
    fn of(theme: &Theme) -> Self {
        let deep = rgb(theme.pane_bg).is_some();
        Self {
            deep,
            pane: theme.pane_bg,
            skin: theme.fg,
            metal: theme.hot,
            line: theme.dim,
            lips: theme.tool,
            lens: theme.del,
            iris: theme.add,
            white: match deep {
                true => Color::Rgb(255, 255, 255),
                false => Color::White,
            },
        }
    }

    /// `colour` at `step` of the ramp's light, darkest first: a share of it
    /// over the pane where the terminal draws values, and in the sixteen
    /// names the colour, its darker, or the darker of that.
    fn lit(&self, colour: Color, step: usize) -> Color {
        let step = u32::try_from(step).unwrap_or(0);
        let last = u32::try_from(RAMP.len() - 1).unwrap_or(1);
        match self.deep {
            true => faded(colour, self.pane, 30 + 70 * step / last, colour),
            false if step * 3 >= last * 2 => colour,
            false if step * 3 >= last => darker(colour),
            false => darker(darker(colour)),
        }
    }

    /// The glyph and style a cell is drawn in, for what is painted there at
    /// `step` of the ramp's light.
    fn cell(&self, paint: Paint, step: usize, pose: &Pose) -> (char, Style) {
        let paper = Style::new().bg(self.pane);
        let ramp = RAMP.get(step).copied().unwrap_or('.');
        match paint {
            Paint::Skin => (ramp, paper.fg(self.lit(self.skin, step))),
            Paint::Metal if step == RAMP.len() - 1 => ('@', paper.fg(self.white).bold()),
            Paint::Metal => (ramp, paper.fg(self.lit(self.metal, step))),
            Paint::Rib => ('=', paper.fg(self.lit(self.metal, step / 2))),
            Paint::Line(glyph) => (glyph, paper.fg(self.line)),
            Paint::Lips(glyph) => (glyph, paper.fg(self.lips).bold()),
            Paint::Teeth => ('=', paper.fg(self.white)),
            Paint::Gap => ('.', paper.fg(darker(self.line))),
            Paint::Lens { core: true } => ('@', paper.fg(self.lens).bold()),
            Paint::Lens { core: false } => {
                let bright = pose.glow > 0.5;
                let colour = match (self.deep, bright) {
                    (_, true) => self.lens,
                    (true, false) => faded(self.lens, self.pane, 60, self.lens),
                    (false, false) => darker(self.lens),
                };
                (if bright { 'O' } else { 'o' }, paper.fg(colour))
            }
            Paint::White => (ramp, paper.fg(self.lit(self.white, step))),
            Paint::Pupil => ('@', paper.fg(self.iris).bold()),
            Paint::Lash => ('-', paper.fg(self.line).bold()),
            Paint::Tip if pose.tip => ('*', paper.fg(self.lens).bold()),
            Paint::Tip => ('.', paper.fg(darker(self.lens))),
        }
    }
}

/// The step of the ramp a surface facing `normal` is lit to, with the light
/// coming from `light` and the operator looking from `toward`: metal shines
/// where the light glances off it toward them.
fn step(normal: Vector, light: Vector, toward: Vector, shiny: bool) -> usize {
    let diffuse = dot(normal, light).max(0.0);
    let shine = match shiny {
        true => {
            let bounce = sub(scale(normal, 2.0 * dot(normal, light)), light);
            dot(bounce, toward).max(0.0).powi(16) * 0.5
        }
        false => 0.0,
    };
    let light = (0.1 + 0.8 * diffuse + shine).clamp(0.0, 1.0);
    let last = RAMP.len() - 1;
    // Rounded to the nearest step, which is between nought and the last.
    ((light * last as f32).round() as usize).min(last)
}

/// Draws the head into `area` of `buffer`, cleared to the pane's colour, as
/// it stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let pose = Pose::at(seed, millis);
    let ink = Ink::of(theme);
    let (width, height) = (f32::from(area.width), f32::from(area.height));
    // Rows to a unit of the head, as many as fit it whole; a column is half
    // as tall, so two to a row.
    let rows = (height / TALL).min(width / (2.0 * WIDE));
    if rows <= 0.0 {
        return;
    }
    let way = pose.seen_from_head([0.0, 0.0, -1.0]);
    let toward = scale(way, -1.0);
    let light = pose.seen_from_head(unit([-0.4, 0.45, 0.8]));
    for row in 0..area.height {
        let up = MIDDLE - (f32::from(row) + 0.5 - height / 2.0) / rows;
        for column in 0..area.width {
            let across = (f32::from(column) + 0.5 - width / 2.0) / (2.0 * rows);
            if across.abs() > 1.2 || up > 1.2 {
                continue;
            }
            let from = pose.seen_from_head([across, up, 4.0]);
            let Some((at, part, normal)) = cast(from, way, &pose) else {
                continue;
            };
            let paint = paint(part, at, toward, &pose);
            let shiny = matches!(paint, Paint::Metal | Paint::Rib);
            let lit = step(normal, light, toward, shiny);
            let (glyph, style) = ink.cell(paint, lit, &pose);
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

    use crate::theme::{Depth, MODERN};

    fn head(theme: &Theme, millis: u64, seed: u64) -> Buffer {
        let area = Rect::new(0, 0, 90, 30);
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

    fn text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The moment `share` of the way into the face's cycle, for `seed`.
    fn into_mood(seed: u64, share: f32) -> u64 {
        let shift = key(seed, 0x300D, 0) % MOOD;
        let at = (share * MOOD as f32) as u64;
        (at + MOOD - shift) % MOOD + MOOD * 10
    }

    #[test]
    fn the_head_stands_in_the_middle_of_the_pane_and_fills_most_of_its_height() {
        let drawn = head(&MODERN, 0, 7);
        let rows: Vec<u16> = (0..30)
            .filter(|y| (0..90).any(|x| drawn[(x, *y)].symbol() != " "))
            .collect();
        assert!(
            rows.len() >= 26,
            "{} rows of head\n{}",
            rows.len(),
            text(&drawn)
        );
        let columns: Vec<u16> = (0..90)
            .filter(|x| (0..30).any(|y| drawn[(*x, y)].symbol() != " "))
            .collect();
        let (left, right) = (columns[0], columns[columns.len() - 1]);
        assert!(left > 10 && right < 80, "{left}..{right}\n{}", text(&drawn));
        assert!((i32::from(left) + i32::from(right) - 90).abs() < 16);
    }

    #[test]
    fn the_face_grins_and_winks_while_the_red_eye_keeps_glowing() {
        let pupils = |buffer: &Buffer| {
            buffer
                .content()
                .iter()
                .filter(|cell| cell.symbol() == "@" && cell.fg == MODERN.add)
                .count()
        };
        let lens = |buffer: &Buffer| {
            buffer
                .content()
                .iter()
                .filter(|cell| cell.fg == MODERN.del || cell.fg == Color::Red)
                .count()
        };
        let lips = |buffer: &Buffer| -> String {
            buffer
                .content()
                .iter()
                .filter(|cell| cell.fg == MODERN.tool)
                .map(|cell| cell.symbol())
                .collect()
        };
        for seed in [1, 7, 40] {
            let straight = head(&MODERN, into_mood(seed, 0.05), seed);
            let grinning = head(&MODERN, into_mood(seed, 0.35), seed);
            let winking = head(&MODERN, into_mood(seed, 0.45), seed);
            assert!(pupils(&straight) > 0, "{}", text(&straight));
            assert!(pupils(&grinning) > 0, "{}", text(&grinning));
            assert_eq!(pupils(&winking), 0, "{}", text(&winking));
            assert!(lens(&winking) > 0, "{}", text(&winking));
            assert!(!lips(&straight).contains('/'), "{}", text(&straight));
            assert!(lips(&grinning).contains('/'), "{}", text(&grinning));
            assert!(lips(&grinning).contains('\\'), "{}", text(&grinning));
            assert!(
                grinning
                    .content()
                    .iter()
                    .any(|cell| cell.symbol() == "=" && cell.fg == Color::White),
                "no teeth\n{}",
                text(&grinning)
            );
        }
    }

    #[test]
    fn a_wink_shuts_the_eye_quickly_holds_it_and_opens_it_again() {
        assert_eq!(wink(0.0), 0.0);
        assert_eq!(wink(0.45), 1.0);
        assert_eq!(wink(0.6), 0.0);
        let closing: Vec<f32> = (0..=10).map(|n| wink(0.42 + 0.0015 * n as f32)).collect();
        assert!(
            closing.windows(2).all(|pair| pair[0] <= pair[1]),
            "{closing:?}"
        );
        assert!(wink(0.48) > 0.0 && wink(0.48) < 1.0);
    }

    #[test]
    fn the_head_turns_from_side_to_side_without_leaving_the_pane() {
        let yaws: Vec<f32> = (0..70).map(|n| Pose::at(3, n * TURN / 70).yaw).collect();
        let (least, most) = yaws.iter().fold((f32::MAX, f32::MIN), |(lo, hi), yaw| {
            (lo.min(*yaw), hi.max(*yaw))
        });
        assert!(least < -0.35 && most > 0.35, "{least}..{most}");
        for millis in (0..TURN).step_by(500) {
            let drawn = head(&MODERN, millis, 3);
            for y in 0..30 {
                for x in [0, 89] {
                    assert_eq!(drawn[(x, y)].symbol(), " ", "{millis} ms\n{}", text(&drawn));
                }
            }
        }
    }

    /// Every shade is one of the ramp's steps, so a frame rewrites only the
    /// cells the turn moved to another step rather than the whole head.
    #[test]
    fn a_frame_changes_only_a_few_of_the_heads_cells() {
        for theme in [MODERN, MODERN.at(Depth::TrueColour)] {
            for millis in [0, 1_750, 30_000, 3_600_000] {
                let now = head(&theme, millis, 7);
                let next = head(&theme, millis + 33, 7);
                let drawn = now
                    .content()
                    .iter()
                    .filter(|cell| cell.symbol() != " ")
                    .count();
                let changed = now
                    .content()
                    .iter()
                    .zip(next.content())
                    .filter(|(was, is)| was != is)
                    .count();
                assert!(
                    changed * 5 < drawn,
                    "{}: {changed} of {drawn} cells changed at {millis} ms",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn the_head_is_drawn_in_the_themes_own_colours() {
        let drawn = head(&MODERN, 2_000, 7);
        let colours: Vec<Color> = drawn.content().iter().map(|cell| cell.fg).collect();
        for colour in [MODERN.fg, MODERN.hot, MODERN.tool] {
            assert!(colours.contains(&colour), "{colour:?}\n{}", text(&drawn));
        }
        assert!(drawn.content().iter().all(|cell| cell.bg == MODERN.pane_bg));
    }

    #[test]
    fn a_line_leans_the_way_it_rises() {
        assert_eq!(slope(0.0, '_'), '_');
        assert_eq!(slope(1.0, '_'), '/');
        assert_eq!(slope(-1.0, '-'), '\\');
    }
}
