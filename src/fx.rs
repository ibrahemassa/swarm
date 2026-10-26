//! Visual effects toolkit: colors, cheap randomness, particles, message packets, block-letter banners.

use std::f32::consts::TAU;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const STAR: &[&str] = &["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];
pub const GLYPHS: &[char] = &[
    '!', '<', '>', '-', '_', '\\', '/', '[', ']', '{', '}', '=', '+', '*', '^', '?', '#', '░', '▒', '▓',
    'ｱ', 'ｶ', 'ﾀ', 'ﾅ', '0', '1',
];
pub const RAIN: &[char] = &[
    'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ', 'ﾃ', 'ﾄ',
    '0', '1', '<', '>', '/', '=', '{', '}',
];
pub const VERBS: &[&str] = &[
    "Vibing", "Transmuting", "Synthesizing", "Conjuring", "Percolating", "Noodling", "Hyperfocusing",
    "Reticulating", "Cooking", "Manifesting", "Wrangling pixels", "Overclocking", "Flexboxing",
    "Gradienting", "Tokenmaxxing", "Speedrunning", "Locking in", "Grinding", "Kerning", "Shipping",
    "Compiling vibes", "Bending CSS", "Summoning divs", "Juggling z-indexes",
];

/// xorshift64*: plenty random for sparkles.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn seeded() -> Self {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(7);
        Rng::new(n)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / 16_777_216.0
    }

    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// Stateless hash in [0, 1] — lets per-cell effects stay stable between frames.
pub fn hash(a: u32, b: u32) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77).rotate_left(13);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x & 0xFFFF) as f32 / 65_535.0
}

pub fn hsv(h: f32, s: f32, v: f32) -> Color {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::Rgb(((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8)
}

pub fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Gray => (170, 170, 170),
        Color::DarkGray => (90, 90, 90),
        _ => (210, 210, 220),
    }
}

pub fn lerp(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color::Rgb(m(ar, br), m(ag, bg), m(ab, bb))
}

/// Mix toward black; `k` = how much of the color survives.
pub fn dim(c: Color, k: f32) -> Color {
    lerp(Color::Rgb(0, 0, 0), c, k)
}

pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub life: f32,
    pub max: f32,
    pub ch: char,
    pub color: Color,
    pub gravity: f32,
    pub rocket: bool,
}

impl Particle {
    pub fn step(&mut self, dt: f32) {
        self.vy += self.gravity * dt;
        let drag = if self.rocket { 1.0 } else { 1.0 - 1.4 * dt };
        self.vx *= drag;
        self.x += self.vx * dt;
        self.y += self.vy * dt;
        self.life -= dt;
    }
}

pub fn burst(out: &mut Vec<Particle>, rng: &mut Rng, x: f32, y: f32, color: Color, n: usize, power: f32) {
    const CH: &[char] = &['✦', '✧', '*', '+', '·', '•', '°', '⋆'];
    for _ in 0..n {
        let a = rng.range(0.0, TAU);
        let s = rng.range(0.25, 1.0) * power;
        let life = rng.range(0.45, 1.3);
        let c = lerp(color, hsv(rng.range(0.0, 360.0), 0.6, 1.0), rng.range(0.0, 0.35));
        out.push(Particle {
            x,
            y,
            vx: a.cos() * s * 2.2,
            vy: a.sin() * s,
            life,
            max: life,
            ch: *rng.pick(CH),
            color: c,
            gravity: 6.0,
            rocket: false,
        });
    }
}

pub fn rocket(rng: &mut Rng, left: f32, right: f32, bottom: f32) -> Particle {
    let life = rng.range(0.9, 1.5);
    Particle {
        x: rng.range(left + 4.0, (right - 4.0).max(left + 5.0)),
        y: bottom - 1.0,
        vx: rng.range(-4.0, 4.0),
        vy: -rng.range(18.0, 30.0),
        life,
        max: life,
        ch: '•',
        color: Color::Rgb(255, 240, 200),
        gravity: 9.0,
        rocket: true,
    }
}

/// A message in flight between two agents.
pub struct Packet {
    pub from: usize,
    pub to: usize,
    pub t: f32,
    pub speed: f32,
    pub color: Color,
    pub from_name: String,
    pub text: String,
}

fn letter(c: char) -> [&'static str; 6] {
    match c {
        'S' => ["███████╗", "██╔════╝", "███████╗", "╚════██║", "███████║", "╚══════╝"],
        'W' => ["██╗    ██╗", "██║    ██║", "██║ █╗ ██║", "██║███╗██║", "╚███╔███╔╝", " ╚══╝╚══╝ "],
        'A' => [" █████╗ ", "██╔══██╗", "███████║", "██╔══██║", "██║  ██║", "╚═╝  ╚═╝"],
        'R' => ["██████╗ ", "██╔══██╗", "██████╔╝", "██╔══██╗", "██║  ██║", "╚═╝  ╚═╝"],
        'M' => ["███╗   ███╗", "████╗ ████║", "██╔████╔██║", "██║╚██╔╝██║", "██║ ╚═╝ ██║", "╚═╝     ╚═╝"],
        'H' => ["██╗  ██╗", "██║  ██║", "███████║", "██╔══██║", "██║  ██║", "╚═╝  ╚═╝"],
        'I' => ["██╗", "██║", "██║", "██║", "██║", "╚═╝"],
        'P' => ["██████╗ ", "██╔══██╗", "██████╔╝", "██╔═══╝ ", "██║     ", "╚═╝     "],
        'T' => ["████████╗", "╚══██╔══╝", "   ██║   ", "   ██║   ", "   ██║   ", "   ╚═╝   "],
        _ => ["   "; 6],
    }
}

/// Big "ANSI Shadow" style letters, six rows tall.
pub fn banner(word: &str) -> Vec<String> {
    let letters: Vec<[&str; 6]> = word.chars().map(letter).collect();
    (0..6)
        .map(|row| {
            letters
                .iter()
                .map(|l| {
                    let w = l.iter().map(|s| s.chars().count()).max().unwrap_or(0);
                    format!("{:<w$}", l[row])
                })
                .collect::<String>()
        })
        .collect()
}
