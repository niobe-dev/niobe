// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The cyborg `modern` shows in the session pane before anything has
//! happened in it: a head in three dimensions, half of it plated in metal
//! round a glowing red eye, turning gently from side to side, breaking into a
//! grin and winking its human eye while it does.
//!
//! The head is a handful of ellipsoids — skull, jaw, nose, ears, eyes — and
//! a neck, each met by a ray cast from every pixel of the pane straight into
//! it, so the picture is worked out whole for the moment drawn. A pixel is
//! half a terminal cell, the upper or lower half block, so the pixels are
//! about square and the head is drawn in shaded colour rather than in
//! characters. Where a ray meets the head, the surface is lit as its
//! material is — skin softly, with the light wrapping a little round it;
//! metal hard, with a sharp highlight — by a warm light from the upper left,
//! a cold rim light in the theme's accent from behind on the right, and the
//! red eye's own glow on the plate round it. The face's features — the
//! seams of the plate, the brow, the lips, the iris, the line a closing lid
//! leaves — are painted on the surface where the ray meets it.
//!
//! Every light is one of a few steps rather than a value of its own, so a
//! cell is rewritten only when the turn of the head moves it to another step,
//! not every frame. In the sixteen names each material has three shades —
//! its colour, its darker and the darker of that — and a highlight is white.

use std::f32::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::{darker, key, rgb};
use crate::theme::Theme;

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
const TURN: u64 = 9_000;

/// How long a nod takes, in milliseconds.
const NOD: u64 = 11_000;

/// How long the face takes to go from straight to a grin, wink and back,
/// in milliseconds.
const MOOD: u64 = 9_000;

/// How long the red eye takes to brighten and dim, in milliseconds.
const PULSE: u64 = 1_500;

/// How long the antenna's light takes to blink, in milliseconds.
const BLINK: u64 = 1_000;

/// How many steps a light is rounded to: enough that the shading reads as
/// smooth, few enough that a slow turn moves only the cells at a step's edge.
const STEPS: f32 = 14.0;

/// Where the human eye is, on the head's right; the red eye is its mirror.
const EYE: [f32; 3] = [0.27, 0.2, 0.6];

/// How big an eye is.
const EYE_RADIUS: f32 = 0.14;

/// How big the lid over the human eye is: just bigger than the eye, so it
/// covers it as it closes.
const LID_RADIUS: f32 = 0.155;

/// How far from where an eye looks its pupil and its iris reach.
const PUPIL: f32 = 0.035;
const IRIS: f32 = 0.08;

/// Where the mouth's middle is, how wide it is at its straightest and how
/// thick its lips are drawn.
const MOUTH: (f32, f32, f32) = (-0.4, 0.2, 0.045);

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
    /// A seam of the plate, or the socket round the red eye.
    Seam,
    Brow,
    Lips,
    Teeth,
    /// The dark of an open mouth.
    Gap,
    Lens {
        core: bool,
    },
    White,
    Iris,
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

/// `a` and `b` mixed, `share` of the way from `a` to `b`.
fn lerp(a: Vector, b: Vector, share: f32) -> Vector {
    add(scale(a, 1.0 - share), scale(b, share))
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
        Part::Lens => Paint::Lens {
            core: off_axis(at, [-EYE[0], EYE[1], EYE[2]], toward) < 0.065,
        },
        Part::Eye => match off_axis(at, EYE, toward) {
            off if off < PUPIL => Paint::Pupil,
            off if off < IRIS => Paint::Iris,
            _ => Paint::White,
        },
        Part::Lid => match (y - lid_edges(pose.wink).0).abs() < 0.04 {
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
/// seams and the socket round the red eye, the brow over the human eye, and
/// the mouth.
fn face(at: Vector, pose: &Pose) -> Paint {
    let [x, y, z] = at;
    if z > 0.3
        && let Some(mouth) = mouth(x, y, pose.grin)
    {
        return mouth;
    }
    let plated = x < -0.03 && y > -0.12;
    let ring = (x + EYE[0]).hypot(y - EYE[1]);
    if plated && z > 0.3 && (0.15..0.2).contains(&ring) {
        return Paint::Seam;
    }
    let seam = ((x + 0.03).abs() < 0.02 && y > -0.12)
        || ((y + 0.12).abs() < 0.022 && x < -0.03)
        || ((y - 0.44).abs() < 0.02 && (-0.46..-0.08).contains(&x));
    if seam {
        return Paint::Seam;
    }
    if plated {
        return Paint::Metal;
    }
    let brow = EYE[1] + 0.27 - 1.2 * (x - EYE[0]).powi(2) - 0.05 * pose.wink;
    if z > 0.3 && (x - EYE[0]).abs() < 0.17 && (y - brow).abs() < 0.035 {
        return Paint::Brow;
    }
    Paint::Skin
}

/// What the mouth shows at `(x, y)` on the face, if it is there: the lips,
/// curving up at their corners the wider the grin, and once the grin is
/// wide, teeth and the dark of the open mouth under the upper lip.
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
    // curling up steeply is drawn as thick as the middle; the lips thin
    // toward the corners.
    let taper = 1.0 - 0.5 * (x / half).powi(2);
    if (y - lip).abs() < thick * taper * rise.hypot(1.0) {
        return Some(Paint::Lips);
    }
    let open = (grin - 0.45).max(0.0) * 0.26 * (1.0 - (x / half).powi(2));
    let below = lip - y;
    match below > 0.0 && below < open {
        true if below < 0.06 => Some(Paint::Teeth),
        true => Some(Paint::Gap),
        false => None,
    }
}

/// How the light falls on a point: from the key light, the rim light behind
/// it and its highlight, each rounded to one of [`STEPS`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Light {
    /// The diffuse light and the ambient under it, from nought to one.
    body: f32,
    /// The highlight, from nought to one.
    shine: f32,
    /// The accent's rim light, from nought to one.
    rim: f32,
}

/// `light` rounded to one of [`STEPS`], so a slow turn changes a point's
/// light only where it crosses a step.
fn stepped(light: f32) -> f32 {
    (light.clamp(0.0, 1.0) * STEPS).round() / STEPS
}

/// The directions a point is lit from and seen from, in the head's frame.
struct Lamps {
    key: Vector,
    back: Vector,
    toward: Vector,
}

/// How a surface facing `normal`, painted `paint`, is lit by `lamps`.
fn light(normal: Vector, paint: Paint, lamps: &Lamps) -> Light {
    let facing = dot(normal, lamps.key);
    let (wrap, hardness, strength) = match paint {
        Paint::Metal | Paint::Rib | Paint::Seam => (0.0, 48, 0.9),
        Paint::White | Paint::Iris | Paint::Pupil | Paint::Lens { .. } => (0.1, 90, 1.0),
        Paint::Teeth => (0.2, 24, 0.4),
        Paint::Lips => (0.35, 16, 0.25),
        Paint::Skin | Paint::Brow | Paint::Lash | Paint::Gap | Paint::Tip => (0.35, 12, 0.12),
    };
    // Skin lets the light wrap a little past where it faces away, which is
    // what keeps a face from looking carved out of plaster.
    let diffuse = ((facing + wrap) / (1.0 + wrap)).max(0.0);
    let ambient = 0.2 + 0.08 * normal[1];
    let half = unit(add(lamps.key, lamps.toward));
    let shine = strength * dot(normal, half).max(0.0).powi(hardness);
    let edge = (1.0 - dot(normal, lamps.toward).max(0.0)).powi(3);
    let rim = edge * (dot(normal, lamps.back) + 0.25).max(0.0);
    Light {
        body: stepped(ambient + 0.85 * diffuse),
        shine: stepped(shine),
        rim: stepped(0.9 * rim),
    }
}

/// The colours the head is drawn in: values where the terminal draws them,
/// the sixteen names where it does not.
enum Ink {
    Deep(Box<Deep>),
    Sixteen(Sixteen),
}

/// The head's colours as values, each a share of full red, green and blue.
struct Deep {
    pane: Vector,
    skin: Vector,
    metal: Vector,
    lips: Vector,
    lens: Vector,
    iris: Vector,
    rim: Vector,
    white: Vector,
}

/// The head's colours in the sixteen names.
struct Sixteen {
    pane: Color,
    skin: Color,
    metal: Color,
    lens: Color,
    iris: Color,
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

impl Ink {
    fn of(theme: &Theme) -> Self {
        let values = [
            theme.pane_bg,
            theme.title,
            theme.del,
            theme.hot,
            theme.dim,
            theme.add,
        ]
        .map(shares);
        match values {
            [
                Some(pane),
                Some(title),
                Some(del),
                Some(hot),
                Some(dim),
                Some(add),
            ] => {
                // The theme's warm yellow, flushed a little with its red: a
                // skin tone drawn from the palette rather than beside it.
                let skin = lerp(title, del, 0.2);
                Self::Deep(Box::new(Deep {
                    pane,
                    skin,
                    // Steel: the theme's grey with a cast of its accent.
                    metal: lerp(dim, hot, 0.3),
                    lips: scale(lerp(skin, del, 0.4), 0.8),
                    lens: del,
                    iris: add,
                    rim: hot,
                    white: [0.93, 0.93, 0.95],
                }))
            }
            _ => Self::Sixteen(Sixteen {
                pane: theme.pane_bg,
                skin: theme.fg,
                metal: theme.hot,
                lens: theme.del,
                iris: theme.add,
            }),
        }
    }

    /// The colour of a point painted `paint` and lit by `light`, which
    /// `glow` of the red eye's light falls on.
    fn colour(&self, paint: Paint, light: Light, glow: f32, pose: &Pose) -> Color {
        match self {
            Self::Deep(deep) => value(deep.colour(paint, light, glow, pose)),
            Self::Sixteen(sixteen) => sixteen.colour(paint, light, pose),
        }
    }

    fn pane(&self) -> Color {
        match self {
            Self::Deep(deep) => value(deep.pane),
            Self::Sixteen(sixteen) => sixteen.pane,
        }
    }
}

impl Deep {
    fn colour(&self, paint: Paint, light: Light, glow: f32, pose: &Pose) -> Vector {
        let dark = scale(self.pane, 0.5);
        let (albedo, emits) = match paint {
            Paint::Skin => (self.skin, None),
            Paint::Brow | Paint::Lash => (scale(self.skin, 0.35), None),
            Paint::Metal => (self.metal, None),
            Paint::Rib => (scale(self.metal, 0.6), None),
            Paint::Seam => (scale(self.metal, 0.3), None),
            Paint::Lips => (self.lips, None),
            Paint::Teeth => (self.white, None),
            Paint::Gap => (dark, None),
            Paint::White => (self.white, None),
            Paint::Iris => (self.iris, None),
            Paint::Pupil => (dark, None),
            Paint::Lens { core: true } => (self.white, Some(lerp(self.lens, self.white, 0.5))),
            Paint::Lens { core: false } => {
                let bright = 0.55 + 0.45 * stepped(pose.glow);
                (self.lens, Some(scale(self.lens, bright)))
            }
            Paint::Tip => match pose.tip {
                true => (self.lens, Some(self.lens)),
                false => (scale(self.lens, 0.35), None),
            },
        };
        if let Some(emits) = emits {
            return add(emits, scale([1.0, 1.0, 1.0], 0.5 * light.shine));
        }
        let lit = scale(albedo, light.body);
        let shine = scale(lerp(self.white, albedo, 0.3), light.shine);
        let rim = scale(self.rim, 0.7 * light.rim);
        let red = scale(self.lens, 0.6 * glow);
        add(add(lit, shine), add(rim, red))
    }
}

impl Sixteen {
    fn colour(&self, paint: Paint, light: Light, pose: &Pose) -> Color {
        let shade = |colour: Color| match light.body {
            body if body >= 0.66 => colour,
            body if body >= 0.33 => darker(colour),
            _ => darker(darker(colour)),
        };
        let shiny = light.shine >= 0.5;
        match paint {
            Paint::Skin => shade(self.skin),
            Paint::Brow | Paint::Lash | Paint::Seam => Color::DarkGray,
            Paint::Metal | Paint::Rib if shiny => Color::White,
            Paint::Metal => shade(self.metal),
            Paint::Rib => darker(self.metal),
            Paint::Lips => darker(self.lens),
            Paint::Teeth | Paint::White => Color::White,
            Paint::Gap | Paint::Pupil => Color::Black,
            Paint::Iris => self.iris,
            Paint::Lens { core: true } => Color::White,
            Paint::Lens { core: false } if pose.glow > 0.5 => self.lens,
            Paint::Lens { core: false } => darker(self.lens),
            Paint::Tip if pose.tip => self.lens,
            Paint::Tip => darker(self.lens),
        }
    }
}

/// How much of the red eye's light falls on the face at `at`: the plate and
/// the cheek round the socket blush with it as it brightens.
fn glow(at: Vector, paint: Paint, pose: &Pose) -> f32 {
    if !matches!(paint, Paint::Skin | Paint::Metal | Paint::Seam) || at[2] < 0.3 {
        return 0.0;
    }
    let from = (at[0] + EYE[0]).hypot(at[1] - EYE[1]);
    let near = (1.0 - (from - EYE_RADIUS) / 0.22).clamp(0.0, 1.0);
    stepped(near * near * (0.4 + 0.6 * pose.glow))
}

/// What a pixel at `across` and `up`, in the head's units, shows: what is
/// painted there and how it is lit, or nothing where the ray misses the head.
fn pixel(across: f32, up: f32, pose: &Pose, lamps: &Lamps) -> Option<(Paint, Vector, Light)> {
    if across.abs() > 1.2 || up > 1.2 {
        return None;
    }
    let from = pose.seen_from_head([across, up, 4.0]);
    let way = scale(lamps.toward, -1.0);
    let (at, part, normal) = cast(from, way, pose)?;
    let paint = paint(part, at, lamps.toward, pose);
    Some((paint, at, light(normal, paint, lamps)))
}

/// The picture of the head in a pane `width` columns by `height` rows, two
/// pixels to a cell one over the other, as what each pixel shows, a row of
/// pixels at a time.
fn picture(width: u16, height: u16, pose: &Pose) -> Vec<Vec<Option<(Paint, Vector, Light)>>> {
    let (columns, rows) = (f32::from(width), 2.0 * f32::from(height));
    // Pixels to a unit of the head, as many as fit it whole; a pixel is
    // about square.
    let per = (rows / TALL).min(columns / WIDE);
    if per <= 0.0 {
        return Vec::new();
    }
    let lamps = Lamps {
        key: pose.seen_from_head(unit([-0.45, 0.5, 0.75])),
        back: pose.seen_from_head(unit([0.85, 0.25, -0.45])),
        toward: pose.seen_from_head([0.0, 0.0, 1.0]),
    };
    (0..2 * height)
        .map(|row| {
            let up = MIDDLE - (f32::from(row) + 0.5 - rows / 2.0) / per;
            (0..width)
                .map(|column| {
                    let across = (f32::from(column) + 0.5 - columns / 2.0) / per;
                    pixel(across, up, pose, &lamps)
                })
                .collect()
        })
        .collect()
}

/// Draws the head into `area` of `buffer`, cleared to the pane's colour, as
/// it stands `millis` after the origin, from `seed`.
pub(super) fn draw(buffer: &mut Buffer, area: Rect, millis: u64, seed: u64, theme: &Theme) {
    if area.is_empty() {
        return;
    }
    let pose = Pose::at(seed, millis);
    let ink = Ink::of(theme);
    let pane = ink.pane();
    let shown = picture(area.width, area.height, &pose);
    let colour = |row: usize, column: usize| {
        shown
            .get(row)
            .and_then(|pixels| pixels.get(column))
            .copied()
            .flatten()
            .map_or(pane, |(paint, at, light)| {
                ink.colour(paint, light, glow(at, paint, &pose), &pose)
            })
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

    /// Whether the cell at `(x, y)` shows any of the head.
    fn drawn_at(buffer: &Buffer, theme: &Theme, x: u16, y: u16) -> bool {
        let cell = &buffer[(x, y)];
        cell.symbol() != " " || cell.bg != theme.pane_bg
    }

    /// What every pixel of a 90 by 30 pane shows at `millis` from `seed`.
    fn painted(millis: u64, seed: u64) -> Vec<Vec<Option<Paint>>> {
        picture(90, 30, &Pose::at(seed, millis))
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|pixel| pixel.map(|(paint, _, _)| paint))
                    .collect()
            })
            .collect()
    }

    fn count(pixels: &[Vec<Option<Paint>>], paint: Paint) -> usize {
        pixels
            .iter()
            .flatten()
            .filter(|p| **p == Some(paint))
            .count()
    }

    /// The moment `share` of the way into the face's cycle, for `seed`.
    fn into_mood(seed: u64, share: f32) -> u64 {
        let shift = key(seed, 0x300D, 0) % MOOD;
        let at = (share * MOOD as f32) as u64;
        (at + MOOD - shift) % MOOD + MOOD * 10
    }

    #[test]
    fn the_head_stands_in_the_middle_of_the_pane_and_fills_most_of_its_height() {
        for theme in [MODERN, MODERN.at(Depth::TrueColour)] {
            let drawn = head(&theme, 0, 7);
            let rows = (0..30)
                .filter(|y| (0..90).any(|x| drawn_at(&drawn, &theme, x, *y)))
                .count();
            assert!(rows >= 27, "{rows} rows of head");
            let columns: Vec<u16> = (0..90)
                .filter(|x| (0..30).any(|y| drawn_at(&drawn, &theme, *x, y)))
                .collect();
            let (left, right) = (columns[0], columns[columns.len() - 1]);
            assert!(left > 10 && right < 80, "{left}..{right}");
            assert!((i32::from(left) + i32::from(right) - 90).abs() < 16);
        }
    }

    /// No character stands for a shade: every cell is a blank or the upper
    /// half block, two pixels of colour one over the other.
    #[test]
    fn the_head_is_drawn_in_pixels_of_colour_rather_than_characters() {
        for theme in [MODERN, MODERN.at(Depth::TrueColour)] {
            for millis in [0, 2_500, 40_000] {
                let drawn = head(&theme, millis, 7);
                for cell in drawn.content() {
                    assert!(
                        [" ", "▀"].contains(&cell.symbol()),
                        "{}: {:?}",
                        theme.name,
                        cell.symbol()
                    );
                }
                let pixels: std::collections::HashSet<Color> = drawn
                    .content()
                    .iter()
                    .flat_map(|cell| [cell.fg, cell.bg])
                    .collect();
                assert!(pixels.len() >= 8, "{} colours", pixels.len());
            }
        }
    }

    #[test]
    fn the_face_grins_and_winks_while_the_red_eye_keeps_glowing() {
        for seed in [1, 7, 40] {
            let straight = painted(into_mood(seed, 0.05), seed);
            let grinning = painted(into_mood(seed, 0.35), seed);
            let winking = painted(into_mood(seed, 0.45), seed);
            for open in [&straight, &grinning] {
                assert!(count(open, Paint::Iris) > 0, "seed {seed}: no iris");
                assert!(count(open, Paint::Pupil) > 0, "seed {seed}: no pupil");
            }
            for paint in [Paint::Iris, Paint::Pupil, Paint::White] {
                assert_eq!(count(&winking, paint), 0, "seed {seed}: {paint:?}");
            }
            assert!(count(&winking, Paint::Lens { core: false }) > 0);
            assert_eq!(count(&straight, Paint::Teeth), 0, "seed {seed}");
            assert!(count(&grinning, Paint::Teeth) > 0, "seed {seed}: no teeth");
        }
    }

    /// Grinning, the lips' corners rise above their middle; straight, they
    /// hardly do.
    #[test]
    fn the_lips_curl_up_at_their_corners_into_a_grin() {
        let rise = |pixels: &[Vec<Option<Paint>>]| {
            let lips: Vec<(usize, usize)> = pixels
                .iter()
                .enumerate()
                .flat_map(|(y, row)| {
                    row.iter()
                        .enumerate()
                        .filter(|(_, p)| **p == Some(Paint::Lips))
                        .map(move |(x, _)| (x, y))
                })
                .collect();
            let (least, most) = lips
                .iter()
                .fold((usize::MAX, 0), |(lo, hi), (x, _)| (lo.min(*x), hi.max(*x)));
            let middle = (least + most) / 2;
            let lowest_at = |x: usize| {
                lips.iter()
                    .filter(|(at, _)| *at == x)
                    .map(|(_, y)| *y)
                    .max()
            };
            let corners = lowest_at(least).max(lowest_at(most)).unwrap_or(0);
            lowest_at(middle).unwrap_or(0).saturating_sub(corners)
        };
        let seed = 7;
        let straight = rise(&painted(into_mood(seed, 0.05), seed));
        let grinning = rise(&painted(into_mood(seed, 0.35), seed));
        assert!(straight <= 1, "straight lips rise {straight} pixels");
        assert!(grinning >= 2, "a grin rises only {grinning} pixels");
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
        let yaws: Vec<f32> = (0..90).map(|n| Pose::at(3, n * TURN / 90).yaw).collect();
        let (least, most) = yaws.iter().fold((f32::MAX, f32::MIN), |(lo, hi), yaw| {
            (lo.min(*yaw), hi.max(*yaw))
        });
        assert!(least < -0.35 && most > 0.35, "{least}..{most}");
        for millis in (0..TURN).step_by(500) {
            let drawn = head(&MODERN, millis, 3);
            for y in 0..30 {
                for x in [0, 89] {
                    assert!(!drawn_at(&drawn, &MODERN, x, y), "{millis} ms ({x}, {y})");
                }
            }
        }
    }

    /// Every light is one of a few steps, so a frame rewrites only the cells
    /// the turn moved to another step rather than the whole head.
    #[test]
    fn a_frame_changes_only_a_few_of_the_heads_cells() {
        for theme in [MODERN, MODERN.at(Depth::TrueColour)] {
            for millis in [0, 1_750, 30_000, 3_600_000] {
                let now = head(&theme, millis, 7);
                let next = head(&theme, millis + 33, 7);
                let drawn = (0..30)
                    .flat_map(|y| (0..90).map(move |x| (x, y)))
                    .filter(|(x, y)| drawn_at(&now, &theme, *x, *y))
                    .count();
                let changed = now
                    .content()
                    .iter()
                    .zip(next.content())
                    .filter(|(was, is)| was != is)
                    .count();
                assert!(
                    changed * 4 < drawn,
                    "{}: {changed} of {drawn} cells changed at {millis} ms",
                    theme.name
                );
            }
        }
    }

    /// In the sixteen names the head is drawn in the theme's own; where the
    /// terminal draws values, in values mixed from the theme's.
    #[test]
    fn the_head_is_drawn_in_the_themes_own_colours() {
        let sixteen = head(&MODERN, 2_000, 7);
        let colours: Vec<Color> = sixteen
            .content()
            .iter()
            .flat_map(|cell| [cell.fg, cell.bg])
            .collect();
        for colour in [MODERN.fg, MODERN.hot, MODERN.pane_bg] {
            assert!(colours.contains(&colour), "{colour:?}");
        }
        assert!(
            !colours
                .iter()
                .any(|colour| matches!(colour, Color::Rgb(..)))
        );
        let deep = MODERN.at(Depth::TrueColour);
        let values = head(&deep, 2_000, 7);
        assert!(
            values
                .content()
                .iter()
                .all(|cell| matches!(cell.bg, Color::Rgb(..)))
        );
    }

    #[test]
    fn a_light_is_rounded_to_one_of_a_few_steps() {
        assert_eq!(stepped(0.0), 0.0);
        assert_eq!(stepped(1.7), 1.0);
        assert_eq!(stepped(0.5 + 0.2 / STEPS), stepped(0.5));
        assert!(stepped(0.5 + 0.8 / STEPS) > stepped(0.5));
    }
}
