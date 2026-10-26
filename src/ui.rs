//! Everything on screen: boot sequence, header, lead pane, comms mesh, agent cards, panels, overlays.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Circle, Line as CanvasLine, Points};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{AState, Agent, App, FileEntry, Kind, Level, Phase};
use crate::fx::{self, dim, hsv, lerp};

const TEXT: Color = Color::Rgb(226, 226, 236);
const SOFT: Color = Color::Rgb(176, 176, 196);
const MUTED: Color = Color::Rgb(128, 128, 154);
const FAINT: Color = Color::Rgb(66, 66, 88);
const WHITE: Color = Color::Rgb(255, 255, 255);
const GREEN: Color = Color::Rgb(80, 250, 123);
const RED: Color = Color::Rgb(255, 85, 85);
const GOLD: Color = Color::Rgb(255, 196, 0);
const AMBER: Color = Color::Rgb(255, 203, 107);
const PINK: Color = Color::Rgb(255, 121, 198);
const NEON: Color = Color::Rgb(255, 46, 136);
const CYAN: Color = Color::Rgb(139, 233, 253);
const PURPLE: Color = Color::Rgb(189, 147, 249);
const ORANGE: Color = Color::Rgb(255, 184, 108);
const PUNCT: Color = Color::Rgb(120, 130, 160);
const LAVENDER: Color = Color::Rgb(170, 150, 220);

fn st(c: Color) -> Style {
    Style::new().fg(c)
}

fn span(s: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(s.into(), st(c))
}

fn bold(s: impl Into<String>, c: Color) -> Span<'static> {
    Span::styled(s.into(), st(c).add_modifier(Modifier::BOLD))
}

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, fg: Color) {
    if let Some(c) = buf.cell_mut((x, y)) {
        c.set_char(ch);
        c.set_fg(fg);
    }
}

fn line_at(buf: &mut Buffer, area: Rect, x: u16, y: u16, line: &Line) {
    if y < area.bottom() && x < area.right() {
        buf.set_line(x, y, line, area.right() - x);
    }
}

fn centered(buf: &mut Buffer, area: Rect, y: u16, line: &Line) {
    let w = line.width() as u16;
    line_at(buf, area, area.x + area.width.saturating_sub(w) / 2, y, line);
}

pub fn fmt_k(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.2}M", n as f64 / 1e6),
    }
}

pub fn fmt_dur(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn fmt_bytes(b: u64) -> String {
    if b < 1024 {
        format!("{b}B")
    } else {
        format!("{:.1}K", b as f64 / 1024.0)
    }
}

fn trunc(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        format!("{}…", s.chars().take(w - 1).collect::<String>())
    }
}

fn wrap_words(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let word = trunc(word, w);
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn shimmer(text: &str, base: Color, t: f32) -> Vec<Span<'static>> {
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let k = 0.5 + 0.5 * (t * 6.0 - i as f32 * 0.45).sin();
            bold(ch.to_string(), lerp(base, WHITE, k * 0.55))
        })
        .collect()
}

fn bar(p: f32, w: usize, color: Color, t: f32) -> Vec<Span<'static>> {
    const PART: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let p = p.clamp(0.0, 1.0);
    let total = p * w as f32;
    let full = total.floor() as usize;
    let frac = ((total - full as f32) * 8.0) as usize;
    let shine = ((t * 14.0) as usize) % (w + 10);
    (0..w)
        .map(|i| {
            if i < full {
                let mut c = lerp(dim(color, 0.5), color, i as f32 / w.max(1) as f32);
                if i == shine || i + 1 == shine {
                    c = lerp(c, WHITE, 0.6);
                }
                span("█", c)
            } else if i == full && frac > 0 {
                span(PART[frac], color)
            } else {
                span("░", FAINT)
            }
        })
        .collect()
}

fn spark(data: &VecDeque<u64>, w: usize, color: Color) -> Vec<Span<'static>> {
    const B: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let tail: Vec<u64> = data.iter().rev().take(w).rev().copied().collect();
    let max = tail.iter().copied().max().unwrap_or(0).max(1);
    let mut out = Vec::new();
    if tail.len() < w {
        out.push(span(" ".repeat(w - tail.len()), FAINT));
    }
    for v in tail {
        let lvl = ((v as f32 / max as f32) * 7.0).round() as usize;
        let c = if v == 0 { FAINT } else { lerp(dim(color, 0.45), color, lvl as f32 / 7.0) };
        out.push(span(B[lvl.min(7)], c));
    }
    out
}

fn parse_hex(word: &str) -> Option<Color> {
    let h = word.strip_prefix('#')?;
    let h: String = h.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    let (r, g, b) = match h.len() {
        3 | 4 => {
            let v: Vec<u8> = h.chars().take(3).map(|c| c.to_digit(16).unwrap_or(0) as u8 * 17).collect();
            (v[0], v[1], v[2])
        }
        6 | 8 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    let c = Color::Rgb(r, g, b);
    // keep near-black swatches readable
    let luma = 0.3 * r as f32 + 0.59 * g as f32 + 0.11 * b as f32;
    Some(if luma < 70.0 { lerp(c, WHITE, 0.45) } else { c })
}

const KEYWORDS: &[&str] = &[
    "const", "let", "var", "function", "return", "if", "else", "for", "of", "in", "new", "=>", "document",
    "window", "class", "import", "export", "async", "await", "true", "false", "null", "this", "@media",
    "@keyframes", "@import", "!important",
];

/// Tiny HTML/CSS/JS highlighter. Hex colors render in their own color.
fn highlight(s: &str) -> Line<'static> {
    let trimmed = s.trim_start();
    if trimmed.starts_with("/*") || trimmed.starts_with("//") || trimmed.starts_with("<!--") || trimmed.starts_with('*') {
        return Line::from(Span::styled(s.to_string(), st(Color::Rgb(98, 108, 132)).add_modifier(Modifier::ITALIC)));
    }
    let chars: Vec<char> = s.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut tag_next = false;
    while i < chars.len() {
        let c = chars[i];
        let st_i = i;
        if c == '"' || c == '\'' || c == '`' {
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            spans.push(span(chars[st_i..i].iter().collect::<String>(), AMBER));
            tag_next = false;
        } else if c == '#' && i + 1 < chars.len() && chars[i + 1].is_ascii_alphanumeric() {
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '-' || chars[i] == '_') {
                i += 1;
            }
            let w: String = chars[st_i..i].iter().collect();
            match parse_hex(&w).filter(|_| w.len() <= 9) {
                Some(col) => spans.push(bold(w, col)),
                None => spans.push(span(w, ORANGE)),
            }
            tag_next = false;
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '%') {
                i += 1;
            }
            spans.push(span(chars[st_i..i].iter().collect::<String>(), ORANGE));
            tag_next = false;
        } else if c.is_alphabetic() || c == '-' || c == '_' || c == '@' || c == '!' || (c == '.' && chars.get(i + 1).is_some_and(|n| n.is_alphabetic())) {
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || matches!(chars[i], '-' | '_')) {
                i += 1;
            }
            let w: String = chars[st_i..i].iter().collect();
            let next = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
            let color = if tag_next {
                PINK
            } else if w.starts_with('.') {
                ORANGE
            } else if KEYWORDS.contains(&w.as_str()) {
                PURPLE
            } else if next == Some('=') {
                GREEN
            } else if next == Some(':') && !s.contains("://") {
                CYAN
            } else if next == Some('(') {
                Color::Rgb(130, 170, 255)
            } else {
                TEXT
            };
            spans.push(span(w, color));
            tag_next = false;
        } else {
            while i < chars.len() {
                let d = chars[i];
                if d.is_alphanumeric() || matches!(d, '"' | '\'' | '`' | '#' | '-' | '_' | '@' | '!') {
                    break;
                }
                i += 1;
            }
            if i == st_i {
                i += 1;
            }
            let p: String = chars[st_i..i].iter().collect();
            tag_next = p.ends_with('<') || p.ends_with("</");
            spans.push(span(p, PUNCT));
        }
    }
    Line::from(spans)
}

fn render_line(kind: Kind, s: &str) -> Line<'static> {
    match kind {
        Kind::Text => Line::from(span(s.to_string(), TEXT)),
        Kind::Thinking => Line::from(Span::styled(s.to_string(), st(LAVENDER).add_modifier(Modifier::ITALIC))),
        Kind::Code => highlight(s),
    }
}

fn stream_lines(app: &App, a: &Agent, width: usize, height: usize) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut out: Vec<Line> = Vec::new();
    'outer: for (k, text) in a.lines.iter().rev() {
        let chars: Vec<char> = text.chars().collect();
        let chunks: Vec<String> = if chars.is_empty() {
            vec![String::new()]
        } else {
            chars.chunks(width).map(|c| c.iter().collect()).collect()
        };
        for c in chunks.iter().rev() {
            out.push(render_line(*k, c));
            if out.len() >= height {
                break 'outer;
            }
        }
    }
    out.reverse();
    if a.working() && (app.frame / 12) % 2 == 0 {
        if let Some(last) = out.last_mut() {
            if last.width() < width {
                last.spans.push(span("▌", a.color));
            }
        }
    }
    out
}

fn rain(buf: &mut Buffer, app: &App, area: Rect, color: Color, seed: usize) {
    let t = app.t;
    // only rain to the right of whatever text is on each row, never inside it
    let ends: Vec<u16> = (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .rev()
                .find(|&x| buf.cell((x, y)).is_some_and(|c| c.symbol() != " "))
                .map(|x| x + 3)
                .unwrap_or(area.left())
        })
        .collect();
    for x in area.left()..area.right() {
        let hx = fx::hash(x as u32, seed as u32 * 7919 + 13);
        if hx < 0.4 {
            continue;
        }
        let speed = 5.0 + hx * 14.0;
        let len = area.height as f32 + 10.0;
        let head = (t * speed + hx * 97.0) % len;
        for k in 0..6u16 {
            let yf = head - k as f32;
            if yf < 0.0 {
                continue;
            }
            let y = area.y + yf as u16;
            if y >= area.bottom() || x < ends[(y - area.y) as usize] {
                continue;
            }
            if let Some(c) = buf.cell_mut((x, y)) {
                if c.symbol() != " " {
                    continue;
                }
                let gi = (fx::hash(x as u32, y as u32 + (t * 8.0) as u32) * fx::RAIN.len() as f32) as usize;
                let g = fx::RAIN[gi.min(fx::RAIN.len() - 1)];
                let fade = 1.0 - k as f32 / 6.0;
                let col = if k == 0 { lerp(color, WHITE, 0.4) } else { dim(color, 0.12 + 0.4 * fade) };
                c.set_char(g);
                c.set_fg(col);
            }
        }
    }
}

fn border_color(app: &App, i: usize) -> Color {
    let a = &app.agents[i];
    let t = app.t;
    let base = match a.state {
        AState::Done => GREEN,
        AState::Failed => lerp(RED, dim(RED, 0.4), 0.5 + 0.5 * (t * 8.0).sin()),
        AState::Queued => FAINT,
        AState::Booting => {
            if fx::hash(i as u32, (t * 12.0) as u32) > 0.5 {
                a.color
            } else {
                dim(a.color, 0.3)
            }
        }
        _ => lerp(dim(a.color, 0.35), a.color, 0.5 + 0.5 * (t * 3.5 + i as f32).sin()),
    };
    let base = if i == 0 && app.phase == Phase::Build && !a.working() {
        lerp(dim(GOLD, 0.4), GOLD, 0.5 + 0.5 * (t * 2.5).sin())
    } else {
        base
    };
    match a.flash {
        Some((at, c)) => {
            let k = at.elapsed().as_secs_f32() / 0.7;
            if k < 1.0 {
                lerp(lerp(c, WHITE, 0.3), base, k)
            } else {
                base
            }
        }
        None => base,
    }
}

fn status_line(app: &App, i: usize) -> Line<'static> {
    let a = &app.agents[i];
    let el = fmt_dur(a.elapsed());
    match a.state {
        AState::Queued => Line::from(span("○ waiting for orders", MUTED)),
        AState::Done => Line::from(vec![
            bold("✔ ", GREEN),
            bold(format!("Shipped in {el}"), GREEN),
            span(format!(" · ↓ {} tokens", fmt_k(a.tokens)), MUTED),
        ]),
        AState::Failed => Line::from(vec![bold("✖ ", RED), span(a.error.clone().unwrap_or_default(), RED)]),
        AState::Booting => {
            let fr = fx::SPINNER[(app.frame as usize / 2 + i) % fx::SPINNER.len()];
            let mut s = vec![bold(format!("{fr} "), a.color)];
            s.extend(shimmer("Booting…", a.color, app.t));
            s.push(span(format!(" ({el})"), MUTED));
            Line::from(s)
        }
        _ => {
            let fr = fx::SPINNER[(app.frame as usize / 2 + i) % fx::SPINNER.len()];
            let mut s = vec![bold(format!("{fr} "), a.color)];
            s.extend(shimmer(&format!("{}…", a.verb), a.color, app.t + i as f32));
            s.push(span(format!(" ({el} · ↓ {} tokens)", fmt_k(a.tokens)), MUTED));
            Line::from(s)
        }
    }
}

fn tool_line(app: &App, i: usize) -> Line<'static> {
    let a = &app.agents[i];
    if a.state == AState::Done && !a.summary.is_empty() {
        // the lead's plan answer is raw JSON; don't print that
        let s = if a.summary.starts_with('{') { "plan locked".to_string() } else { a.summary.clone() };
        return Line::from(vec![span("⎿ ", GREEN), span(s, SOFT)]);
    }
    if a.tool.is_empty() {
        let s = if a.state == AState::Queued { "⎿ standing by" } else { "⎿ warming up…" };
        return Line::from(span(s, FAINT));
    }
    let running = matches!(a.state, AState::Tool | AState::Writing);
    let dot = if running {
        if (app.frame / 8) % 2 == 0 {
            a.color
        } else {
            dim(a.color, 0.35)
        }
    } else {
        MUTED
    };
    Line::from(vec![
        span(if running { "⏺ " } else { "⎿ " }, dot),
        bold(a.tool.clone(), TEXT),
        span(format!("({})", a.detail), MUTED),
    ])
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.screen = area;
    if app.booting() {
        draw_boot(f, app, area);
        return;
    }

    let bottom_h = if area.height >= 34 { 9 } else { 0 };
    let top_h = (app.plan.len() as u16 + 5).clamp(11, 14).min(area.height / 3).max(5);
    let [header, top, grid, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(top_h),
        Constraint::Min(5),
        Constraint::Length(bottom_h),
    ])
    .areas(area);
    let (lead_area, mesh_area) = if area.width >= 110 {
        let [l, m] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(top);
        (l, Some(m))
    } else {
        (top, None)
    };

    app.rects.clear();
    app.rects.insert(0, lead_area);
    let n = app.agents.len().saturating_sub(1);
    for (k, r) in grid_rects(grid, n).into_iter().enumerate() {
        app.rects.insert(k + 1, r);
    }

    let app: &App = app;
    draw_header(f, app, header);
    draw_lead(f, app, lead_area);
    if let Some(m) = mesh_area {
        draw_mesh(f, app, m);
    }
    for i in 1..app.agents.len() {
        if let Some(r) = app.rects.get(&i) {
            draw_card(f, app, i, *r);
        }
    }
    if bottom_h > 0 {
        let [ev, files, comms] = Layout::horizontal([
            Constraint::Percentage(44),
            Constraint::Percentage(24),
            Constraint::Percentage(32),
        ])
        .areas(bottom);
        draw_events(f, app, ev);
        draw_files(f, app, files);
        draw_comms(f, app, comms);
    }
    draw_bubbles(f, app);
    draw_packets(f, app);
    if app.shipped.is_some() {
        draw_ship(f, app, area);
    }
    draw_particles(f, app, area);
    if app.fx {
        draw_scanline(f, app, area);
    }
    draw_wipe(f, app, area);
}

fn grid_rects(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 || area.height == 0 {
        return Vec::new();
    }
    let max_cols = match area.width {
        200.. if n >= 7 => 4,
        170.. => 3,
        100.. => 2,
        _ => 1,
    };
    // balance rows: 4 agents → 2×2, not 3 + 1 stretched
    let rows = n.div_ceil(max_cols);
    let cols = n.div_ceil(rows).max(1);
    let row_rects = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    let mut out = Vec::new();
    for (r, rr) in row_rects.iter().enumerate() {
        let k = cols.min(n - r * cols);
        let cs = Layout::horizontal(vec![Constraint::Ratio(1, k as u32); k]).split(*rr);
        out.extend(cs.iter().copied());
    }
    out
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let t = app.t;
    let mut spans = Vec::new();
    for (i, ch) in " ◢◤ S W A R M ◢◤ ".chars().enumerate() {
        let glitch = app.fx && ch != ' ' && fx::hash(i as u32, (t * 7.0) as u32) > 0.97;
        let ch = if glitch { fx::GLYPHS[(app.frame as usize + i) % fx::GLYPHS.len()] } else { ch };
        spans.push(bold(ch.to_string(), hsv(i as f32 * 12.0 + t * 90.0, 0.7, 1.0)));
    }
    if app.demo {
        spans.push(Span::styled(" DEMO ", Style::new().fg(Color::Black).bg(NEON).add_modifier(Modifier::BOLD)));
    }
    spans.push(span("  ", FAINT));
    for (i, p) in Phase::ALL.iter().enumerate() {
        let s = if *p == app.phase {
            let k = 0.5 + 0.5 * (t * 5.0).sin();
            Style::new()
                .fg(Color::Black)
                .bg(lerp(Color::Rgb(0, 190, 255), NEON, k))
                .add_modifier(Modifier::BOLD)
        } else if *p < app.phase {
            st(GREEN)
        } else {
            st(FAINT)
        };
        let label = if *p < app.phase { format!(" ✔ {} ", p.label()) } else { format!(" {} ", p.label()) };
        spans.push(Span::styled(label, s));
        if i < 3 {
            spans.push(span("━━", if *p < app.phase { dim(GREEN, 0.6) } else { FAINT }));
        }
    }
    let left = Line::from(spans);
    let left_w = left.width() as u16;
    f.render_widget(left, area);

    let active = app.agents.iter().filter(|a| a.working()).count();
    let mut right = vec![
        span(" ⏱ ", MUTED),
        bold(fmt_dur(app.elapsed()), TEXT),
        span("  ⚡ ", MUTED),
        bold(format!("{}/{}", active, app.agents.len()), if active > 0 { GOLD } else { TEXT }),
        span("  ✉ ", MUTED),
        bold(app.msg_count.to_string(), TEXT),
        span("  ↓ ", MUTED),
        bold(fmt_k(app.total_tokens()), TEXT),
        span("  $", MUTED),
        bold(format!("{:.2}", app.total_cost()), GREEN),
        span("  ", MUTED),
    ];
    right.extend(spark(&app.rate, 16, CYAN));
    right.push(span(" ", MUTED));
    let right = Line::from(right);
    let w = right.width() as u16;
    if area.width > w + left_w + 2 {
        f.render_widget(right, Rect { x: area.right() - w, y: area.y, width: w, height: 1 });
    }
}

fn lead_status(app: &App) -> Line<'static> {
    let a = &app.agents[0];
    let workers = &app.agents[1..];
    let done = workers.iter().filter(|w| w.state == AState::Done).count();
    match app.phase {
        Phase::Ship => Line::from(vec![
            bold("✔ ", GREEN),
            bold("Shipped", GREEN),
            span(
                format!(
                    " · {} agents · {} · ↓ {} tokens · {} messages",
                    app.agents.len(),
                    fmt_dur(app.elapsed()),
                    fmt_k(app.total_tokens()),
                    app.msg_count
                ),
                MUTED,
            ),
        ]),
        Phase::Build if !a.working() => {
            let fr = fx::STAR[(app.frame as usize / 3) % fx::STAR.len()];
            let mut s = vec![bold(format!("{fr} "), GOLD)];
            s.extend(shimmer("Orchestrating…", GOLD, app.t));
            s.push(span(
                format!(
                    " ({} · ↓ {} tokens · {}/{} shipped)",
                    fmt_dur(app.elapsed()),
                    fmt_k(app.total_tokens()),
                    done,
                    workers.len()
                ),
                MUTED,
            ));
            Line::from(s)
        }
        _ => status_line(app, 0),
    }
}

fn plan_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let idw = app.plan.iter().map(|p| p.id.chars().count()).max().unwrap_or(4).min(14);
    let namew = app.plan.iter().map(|p| p.agent.chars().count()).max().unwrap_or(6).min(12);
    app.plan
        .iter()
        .map(|p| {
            let Some(i) = app.agent_index(&p.agent) else {
                return Line::from(span(format!("○ #{}", p.id), MUTED));
            };
            let a = &app.agents[i];
            let (glyph, gc) = match a.state {
                AState::Done => ("✔".to_string(), GREEN),
                AState::Failed => ("✖".to_string(), RED),
                AState::Queued => ("○".to_string(), FAINT),
                _ => (fx::SPINNER[(app.frame as usize / 2 + i) % fx::SPINNER.len()].to_string(), a.color),
            };
            let mut s = vec![
                bold(format!("{glyph} "), gc),
                bold(format!("#{:<idw$}", trunc(&p.id, idw)), TEXT),
                span(" ─▸ ", FAINT),
                bold(format!("{:<namew$} ", trunc(&p.agent, namew)), a.color),
            ];
            let used = 2 + 1 + idw + 4 + namew + 1;
            let barw = 10usize.min(width.saturating_sub(used + 6));
            let mut used = used;
            if barw >= 4 {
                s.extend(bar(a.progress(), barw, a.color, app.t + i as f32));
                s.push(span(format!(" {:>3}% ", (a.progress() * 100.0) as u32), MUTED));
                used += barw + 6;
            }
            if width > used + 4 && !a.tool.is_empty() && a.working() {
                s.push(span(trunc(&format!("{}({})", a.tool, a.detail), width - used), FAINT));
            }
            Line::from(s)
        })
        .collect()
}

fn draw_lead(f: &mut Frame, app: &App, area: Rect) {
    let a = &app.agents[0];
    let bc = border_color(app, 0);
    let title = Line::from(vec![
        span(" ", bc),
        bold("★ ", GOLD),
        bold(a.spec.name.clone(), GOLD),
        span(" ", bc),
        Span::styled(" LEAD ", Style::new().fg(Color::Black).bg(GOLD).add_modifier(Modifier::BOLD)),
        span(format!(" {} · {} ", a.spec.provider.label(), a.spec.model_label()), MUTED),
    ]);
    let name = if app.title.is_empty() { trunc(&app.task, 48) } else { trunc(&app.title, 48) };
    let right = Line::from(span(format!(" {name} "), SOFT)).right_aligned();
    let foot = Line::from(span(" q quit · o open page · f fx ", FAINT)).right_aligned();
    let block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(st(bc))
        .title(title)
        .title_top(right)
        .title_bottom(foot);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 || inner.width < 10 {
        return;
    }
    let row = |y: u16| Rect { x: inner.x, y, width: inner.width, height: 1 };
    f.render_widget(lead_status(app), row(inner.y));
    let sub = if app.phase == Phase::Ship {
        Line::from(vec![span("⎿ ", GREEN), span("index.html assembled · press ", SOFT), bold("o", GOLD), span(" to open it", SOFT)])
    } else if app.phase == Phase::Build && !a.working() {
        let active = app.agents.iter().filter(|w| w.working()).count();
        Line::from(span(
            format!("⎿ {active} agents working · {} messages relayed · {} files on disk", app.msg_count, app.files.len()),
            MUTED,
        ))
    } else {
        tool_line(app, 0)
    };
    f.render_widget(sub, row(inner.y + 1));
    let body = Rect { x: inner.x, y: inner.y + 2, width: inner.width, height: inner.height.saturating_sub(2) };
    if body.height == 0 {
        return;
    }
    if app.plan.is_empty() {
        f.render_widget(Paragraph::new(stream_lines(app, a, body.width as usize, body.height as usize)), body);
        if app.fx && matches!(a.state, AState::Booting | AState::Thinking) {
            rain(f.buffer_mut(), app, body, GOLD, 0);
        }
    } else {
        let [plan_r, gap, stream_r] =
            Layout::horizontal([Constraint::Percentage(62), Constraint::Length(2), Constraint::Fill(1)]).areas(body);
        f.render_widget(Paragraph::new(plan_lines(app, plan_r.width as usize)), plan_r);
        let buf = f.buffer_mut();
        for y in gap.top()..gap.bottom() {
            put(buf, gap.x, y, '│', FAINT);
        }
        f.render_widget(Paragraph::new(stream_lines(app, a, stream_r.width as usize, stream_r.height as usize)), stream_r);
    }
}

fn draw_mesh(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(st(FAINT))
        .title(Line::from(vec![span(" ◈ ", CYAN), bold("MESH", TEXT), span(" · live comms ", MUTED)]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 12 || inner.height < 5 {
        return;
    }
    let n = app.agents.len();
    // stretch the ring into an ellipse that fills the panel so labels have room
    let (xr, yr) = (1.4, 1.35);
    let t = app.t as f64;
    let nodes: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            if i == 0 || n == 1 {
                (0.0, 0.0)
            } else {
                let a = (i - 1) as f64 / (n - 1) as f64 * TAU + t * 0.1;
                (a.cos(), a.sin() * 0.88)
            }
        })
        .collect();
    let trails: Vec<(Vec<(f64, f64)>, (f64, f64), Color)> = app
        .packets
        .iter()
        .filter(|p| p.from < n && p.to < n)
        .map(|p| {
            let (a, b) = (nodes[p.from], nodes[p.to]);
            let at = |u: f32| {
                let u = fx::ease(u.clamp(0.0, 1.0)) as f64;
                (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u)
            };
            let trail = (1..9).map(|k| at(p.t - k as f32 * 0.025)).collect();
            (trail, at(p.t), p.color)
        })
        .collect();
    let agents = &app.agents;
    let cell_w = 2.0 * xr / inner.width as f64;
    let pulse = |i: usize| 0.5 + 0.5 * (t * 4.0 + i as f64).sin();
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([-xr, xr])
        .y_bounds([-yr, yr])
        .paint(|ctx| {
            for i in 1..n {
                let a = &agents[i];
                let (x, y) = nodes[i];
                let c = if a.working() {
                    lerp(dim(a.color, 0.25), a.color, pulse(i) as f32 * 0.6)
                } else if a.state == AState::Done {
                    dim(GREEN, 0.45)
                } else {
                    FAINT
                };
                ctx.draw(&CanvasLine { x1: 0.0, y1: 0.0, x2: x, y2: y, color: c });
                if n > 3 {
                    let j = if i + 1 < n { i + 1 } else { 1 };
                    let (x2, y2) = nodes[j];
                    ctx.draw(&CanvasLine { x1: x, y1: y, x2, y2, color: Color::Rgb(42, 42, 60) });
                }
            }
            ctx.layer();
            for (trail, head, c) in &trails {
                ctx.draw(&Points { coords: trail, color: dim(*c, 0.7) });
                ctx.draw(&Circle { x: head.0, y: head.1, radius: 0.05, color: *c });
                ctx.draw(&Points { coords: &[*head], color: WHITE });
            }
            ctx.layer();
            for i in 0..n {
                let a = &agents[i];
                let (x, y) = nodes[i];
                let c = match a.state {
                    AState::Done => GREEN,
                    AState::Failed => RED,
                    AState::Queued if i != 0 => FAINT,
                    _ => a.color,
                };
                let r = if i == 0 { 0.13 } else { 0.07 } + if a.working() { 0.03 * pulse(i) } else { 0.0 };
                ctx.draw(&Circle { x, y, radius: r, color: c });
                if i == 0 {
                    ctx.draw(&Circle { x, y, radius: r * 0.5, color: c });
                }
            }
            for i in 0..n {
                let a = &agents[i];
                let (x, y) = nodes[i];
                let label = if i == 0 { format!("★ {}", a.spec.name) } else { a.spec.name.clone() };
                let lx = if x < -0.1 { x - 0.14 - label.chars().count() as f64 * cell_w } else { x + 0.14 };
                ctx.print(lx, y, Line::from(bold(label, a.color)));
            }
        });
    f.render_widget(canvas, inner);
}

fn draw_card(f: &mut Frame, app: &App, i: usize, area: Rect) {
    let a = &app.agents[i];
    let bc = border_color(app, i);
    let bt = match a.state {
        AState::Done => BorderType::Double,
        _ if a.working() => BorderType::Thick,
        _ => BorderType::Rounded,
    };
    let dot = match a.state {
        AState::Done => "✔".to_string(),
        AState::Failed => "✖".to_string(),
        AState::Queued => "○".to_string(),
        _ => fx::SPINNER[(app.frame as usize / 2 + i) % fx::SPINNER.len()].to_string(),
    };
    let title = Line::from(vec![
        span(" ", bc),
        bold(dot, bc),
        span(" ", bc),
        bold(a.spec.name.clone(), a.color),
        span(format!(" ─ {} {} ", a.spec.provider.label(), a.spec.model_label()), MUTED),
    ]);
    let role = if a.role.is_empty() { "…".to_string() } else { format!("#{}", a.role) };
    let role_t = Line::from(Span::styled(
        format!(" {role} "),
        Style::new().fg(Color::Black).bg(dim(a.color, 0.9)).add_modifier(Modifier::BOLD),
    ))
    .right_aligned();
    let foot = Line::from(span(format!(" ⚒ {} tools · ✎ {} files ", a.tools_used, a.files), MUTED)).right_aligned();
    let block = Block::bordered()
        .border_type(bt)
        .border_style(st(bc))
        .title(title)
        .title_top(role_t)
        .title_bottom(foot);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 || inner.width < 10 {
        return;
    }
    let row = |y: u16| Rect { x: inner.x, y, width: inner.width, height: 1 };
    let mut y = inner.y;
    f.render_widget(status_line(app, i), row(y));
    y += 1;
    if inner.height >= 4 {
        f.render_widget(tool_line(app, i), row(y));
        y += 1;
    }
    let bar_h = if inner.height >= 6 { 1 } else { 0 };
    let stream_area = Rect { x: inner.x, y, width: inner.width, height: inner.bottom().saturating_sub(y + bar_h) };
    if stream_area.height > 0 {
        f.render_widget(Paragraph::new(stream_lines(app, a, stream_area.width as usize, stream_area.height as usize)), stream_area);
        if app.fx && matches!(a.state, AState::Booting | AState::Thinking) {
            rain(f.buffer_mut(), app, stream_area, a.color, i);
        }
    }
    if bar_h == 1 {
        let w = inner.width as usize;
        let sw = 12.min(w / 4);
        let bw = w.saturating_sub(sw + 7);
        let mut s = bar(a.progress(), bw, a.color, app.t + i as f32 * 0.7);
        s.push(span(format!(" {:>3}% ", (a.progress() * 100.0) as u32), MUTED));
        s.extend(spark(&a.rate, sw, a.color));
        f.render_widget(Line::from(s), row(inner.bottom() - 1));
    }
}

fn panel(title: &str, icon: &str, icon_c: Color, extra: String) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(st(FAINT))
        .title(Line::from(vec![span(format!(" {icon} "), icon_c), bold(title.to_string(), TEXT), span(format!("{extra} "), MUTED)]))
}

fn draw_events(f: &mut Frame, app: &App, area: Rect) {
    let block = panel("EVENTS", "⌁", PINK, String::new());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (h, w) = (inner.height as usize, inner.width as usize);
    let lines: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(h)
        .rev()
        .map(|(at, lvl, text)| {
            let ts = at.saturating_duration_since(app.start).as_secs();
            let (glyph, gc, tc) = match lvl {
                Level::Info => ("›", MUTED, SOFT),
                Level::Good => ("✔", GREEN, GREEN),
                Level::Warn => ("▲", AMBER, AMBER),
                Level::Bad => ("✖", RED, RED),
                Level::Msg => ("✉", CYAN, CYAN),
            };
            let tc = if at.elapsed().as_secs_f32() < 0.8 { WHITE } else { tc };
            Line::from(vec![
                span(format!("{:02}:{:02} ", ts / 60, ts % 60), FAINT),
                span(format!("{glyph} "), gc),
                span(trunc(text, w.saturating_sub(8)), tc),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_files(f: &mut Frame, app: &App, area: Rect) {
    let block = panel("FILES", "▤", AMBER, format!(" · {}", app.files.len()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (h, w) = (inner.height as usize, inner.width as usize);
    let mut list: Vec<&FileEntry> = app.files.iter().collect();
    if list.len() > h {
        list.sort_by_key(|e| std::cmp::Reverse(e.at));
        list.truncate(h);
    }
    list.sort_by(|a, b| a.path.cmp(&b.path));
    let lines: Vec<Line> = list
        .iter()
        .map(|e| {
            let (icon, ic) = match e.path.rsplit('.').next().unwrap_or("") {
                "html" => ("◇", ORANGE),
                "css" => ("◆", CYAN),
                "js" => ("◈", AMBER),
                _ => ("·", MUTED),
            };
            let age = e.at.elapsed().as_secs_f32();
            let badge = if age < 2.5 {
                if e.new {
                    " NEW"
                } else {
                    " MOD"
                }
            } else {
                ""
            };
            let size = fmt_bytes(e.bytes);
            let pw = w.saturating_sub(2 + badge.len() + size.len() + 1);
            let path = trunc(&e.path, pw);
            let pad = pw.saturating_sub(path.chars().count());
            let pc = if age < 2.5 { lerp(WHITE, TEXT, age / 2.5) } else { SOFT };
            Line::from(vec![
                span(format!("{icon} "), ic),
                span(path, pc),
                bold(badge, if e.new { GREEN } else { AMBER }),
                span(" ".repeat(pad + 1), FAINT),
                span(size, MUTED),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn color_of(app: &App, name: &str) -> Color {
    if name.eq_ignore_ascii_case("all") {
        return GOLD;
    }
    app.agent_index(name).map(|i| app.agents[i].color).unwrap_or(MUTED)
}

fn draw_comms(f: &mut Frame, app: &App, area: Rect) {
    let block = panel("COMMS", "✉", CYAN, format!(" · {}", app.msg_count));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let (h, w) = (inner.height as usize, inner.width as usize);
    let lines: Vec<Line> = app
        .comms
        .iter()
        .rev()
        .take(h)
        .rev()
        .map(|(from, to, text)| {
            let head = from.chars().count() + to.chars().count() + 5;
            Line::from(vec![
                bold(from.clone(), color_of(app, from)),
                span(" ➜ ", FAINT),
                bold(to.clone(), color_of(app, to)),
                span(": ", FAINT),
                span(trunc(text, w.saturating_sub(head)), SOFT),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_bubbles(f: &mut Frame, app: &App) {
    for (i, a) in app.agents.iter().enumerate() {
        let Some((from, text, at)) = &a.bubble else { continue };
        let Some(r) = app.rects.get(&i) else { continue };
        let age = at.elapsed().as_secs_f32();
        let maxw = r.width.saturating_sub(4).min(48);
        if maxw < 16 || r.height < 7 {
            continue;
        }
        let lines: Vec<String> = wrap_words(text, (maxw - 2) as usize).into_iter().take(3).collect();
        let h = lines.len() as u16 + 2;
        let grow = (age / 0.18).min(1.0);
        let w = ((maxw as f32) * grow).max(6.0) as u16;
        let rect = Rect { x: r.right().saturating_sub(w + 2), y: r.bottom().saturating_sub(h + 2), width: w, height: h };
        let fc = color_of(app, from);
        let fade = if age > 3.6 { (1.0 - (age - 3.6) / 0.9).max(0.25) } else { 1.0 };
        f.render_widget(Clear, rect);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(st(dim(fc, fade)))
            .title(Line::from(vec![span(" ✉ ", fc), bold(from.clone(), dim(fc, fade)), span(" ", fc)]));
        let body: Vec<Line> = lines.into_iter().map(|l| Line::from(span(l, dim(TEXT, fade)))).collect();
        f.render_widget(Paragraph::new(body).block(block), rect);
    }
}

fn center(r: &Rect) -> (f32, f32) {
    (r.x as f32 + r.width as f32 / 2.0, r.y as f32 + r.height as f32 / 2.0)
}

fn draw_packets(f: &mut Frame, app: &App) {
    let buf = f.buffer_mut();
    for p in &app.packets {
        let (Some(a), Some(b)) = (app.rects.get(&p.from), app.rects.get(&p.to)) else { continue };
        let (x1, y1) = center(a);
        let (x2, y2) = center(b);
        let steps = ((x2 - x1).abs().max((y2 - y1).abs() * 2.0) as usize).max(2);
        let head = fx::ease(p.t);
        for s in 0..=steps {
            let u = s as f32 / steps as f32;
            if u > head {
                break;
            }
            let x = x1 + (x2 - x1) * u;
            let y = y1 + (y2 - y1) * u;
            let behind = (head - u) * steps as f32;
            let (ch, c) = if behind < 1.0 {
                ('◆', lerp(p.color, WHITE, 0.5))
            } else if behind < 8.0 {
                ('•', lerp(p.color, dim(p.color, 0.35), behind / 8.0))
            } else {
                ('·', dim(p.color, 0.4))
            };
            put(buf, x.round() as u16, y.round() as u16, ch, c);
        }
    }
}

fn draw_particles(f: &mut Frame, app: &App, area: Rect) {
    let buf = f.buffer_mut();
    for p in &app.particles {
        if p.x < area.x as f32 || p.y < area.y as f32 {
            continue;
        }
        let (x, y) = (p.x as u16, p.y as u16);
        if x >= area.right() || y >= area.bottom() {
            continue;
        }
        let k = (p.life / p.max).clamp(0.0, 1.0);
        put(buf, x, y, p.ch, lerp(dim(p.color, 0.2), p.color, k));
    }
}

fn draw_scanline(f: &mut Frame, app: &App, area: Rect) {
    let (period, sweep) = (7.0, 1.3);
    let ph = app.t % period;
    if ph > sweep || area.height == 0 {
        return;
    }
    let y = area.y + ((ph / sweep) * area.height as f32) as u16;
    let buf = f.buffer_mut();
    for (dy, col) in [(0u16, Color::Rgb(48, 22, 72)), (1, Color::Rgb(26, 14, 40))] {
        let yy = y.saturating_sub(dy);
        if yy >= area.bottom() {
            continue;
        }
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, yy)) {
                c.set_bg(col);
            }
        }
    }
}

fn draw_wipe(f: &mut Frame, app: &App, area: Rect) {
    let Some(at) = app.dash_at else { return };
    let (age, dur) = (at.elapsed().as_secs_f32(), 0.7);
    if age >= dur {
        return;
    }
    let total = area.width as f32 + area.height as f32 * 2.0;
    let front = age / dur * total * 1.1;
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let d = (x - area.x) as f32 + (y - area.y) as f32 * 2.0;
            let Some(c) = buf.cell_mut((x, y)) else { continue };
            if d > front {
                c.set_char(' ');
                c.set_bg(Color::Reset);
            } else if d > front - 5.0 {
                let g = fx::GLYPHS[(fx::hash(x as u32, y as u32 + app.frame as u32) * fx::GLYPHS.len() as f32) as usize % fx::GLYPHS.len()];
                c.set_char(g);
                c.set_fg(hsv(d * 7.0, 0.8, 1.0));
            }
        }
    }
}

fn draw_boot(f: &mut Frame, app: &App, area: Rect) {
    let age = app.boot_age();
    let logo = fx::banner("SWARM");
    let lw = logo.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let rows = 6 + 3 + app.preflight.len() as u16 + 4;
    let x0 = area.x + area.width.saturating_sub(lw) / 2;
    let y0 = area.y + area.height.saturating_sub(rows) / 2;
    let buf = f.buffer_mut();
    for (r, line) in logo.iter().enumerate() {
        for (c, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let (x, y) = (x0 + c as u16, y0 + r as u16);
            let h = fx::hash(r as u32 + 1, c as u32 + 1);
            let reveal = 0.1 + h * 0.85;
            if age >= reveal {
                put(buf, x, y, ch, hsv(c as f32 * 5.0 + app.t * 140.0, 0.65, 1.0));
            } else if age > reveal * 0.35 {
                let gi = (fx::hash(c as u32, app.frame as u32) * fx::GLYPHS.len() as f32) as usize % fx::GLYPHS.len();
                put(buf, x, y, fx::GLYPHS[gi], dim(CYAN, 0.6));
            }
        }
    }
    let sub = "multi-agent web forge  ·  claude × codex × antigravity";
    let shown = ((age - 0.7).max(0.0) * 70.0) as usize;
    centered(buf, area, y0 + 7, &Line::from(span(sub.chars().take(shown).collect::<String>(), MUTED)));

    let w = 60usize;
    let lx = area.x + area.width.saturating_sub(w as u16) / 2;
    let ly = y0 + 9;
    for (k, (label, status, ok)) in app.preflight.iter().enumerate() {
        let appear = 1.0 + k as f32 * 0.22;
        if age < appear {
            break;
        }
        let dots_n = w.saturating_sub(label.chars().count() + status.chars().count() + 5);
        let dots = (((age - appear) * 140.0) as usize).min(dots_n);
        let mut spans = vec![span("⟐ ", CYAN), bold(label.clone(), TEXT), span(" ", FAINT), span("·".repeat(dots), FAINT)];
        if dots == dots_n {
            spans.push(span(" ", FAINT));
            spans.push(bold(status.clone(), if *ok { GREEN } else { RED }));
        }
        line_at(buf, area, lx, ly + k as u16, &Line::from(spans));
    }
    let p = (age / app.boot_len).min(1.0);
    let by = ly + app.preflight.len() as u16 + 1;
    let mut spans = vec![bold("BOOTING SWARM ", TEXT)];
    spans.extend(bar(p, 36, NEON, app.t));
    spans.push(bold(format!(" {:>3}%", (p * 100.0) as u32), TEXT));
    centered(buf, area, by, &Line::from(spans));
    centered(buf, area, by + 2, &Line::from(span("press any key to skip", FAINT)));
}

fn draw_ship(f: &mut Frame, app: &App, area: Rect) {
    let Some(at) = app.shipped else { return };
    let age = at.elapsed().as_secs_f32();
    if age < 0.6 {
        return;
    }
    let k = ((age - 0.6) / 0.5).min(1.0);
    {
        let buf = f.buffer_mut();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(c) = buf.cell_mut((x, y)) {
                    let fg = if c.fg == Color::Reset { TEXT } else { c.fg };
                    c.set_fg(lerp(fg, Color::Rgb(18, 16, 26), 0.72 * k));
                }
            }
        }
    }
    let banner = fx::banner("SHIP IT");
    let bw = banner.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let stats = format!(
        "{} agents · {} · ↓ {} tokens · {} messages · ${:.2}",
        app.agents.len(),
        fmt_dur(app.elapsed()),
        fmt_k(app.total_tokens()),
        app.msg_count,
        app.total_cost()
    );
    let path = app.dir.join("index.html").display().to_string();
    let hint = "o  open in browser     ·     q  quit";
    let box_w = (bw.max(stats.chars().count() as u16).max(path.chars().count() as u16) + 8).min(area.width);
    let box_h = 13.min(area.height);
    let r = Rect {
        x: area.x + area.width.saturating_sub(box_w) / 2,
        y: area.y + area.height.saturating_sub(box_h) / 2,
        width: box_w,
        height: box_h,
    };
    f.render_widget(Clear, r);
    f.render_widget(
        Block::bordered().border_type(BorderType::Double).border_style(st(hsv(app.t * 120.0, 0.7, 1.0))),
        r,
    );
    let buf = f.buffer_mut();
    let x0 = r.x + r.width.saturating_sub(bw) / 2;
    let y0 = r.y + 1;
    let open = ((age - 0.6) * 90.0) as i32;
    let mid = bw as i32 / 2;
    for (row, line) in banner.iter().enumerate() {
        for (c, ch) in line.chars().enumerate() {
            if ch == ' ' || (c as i32 - mid).abs() > open {
                continue;
            }
            let col = hsv(c as f32 * 6.0 + row as f32 * 10.0 - app.t * 220.0, 0.75, 1.0);
            put(buf, x0 + c as u16, y0 + row as u16, ch, col);
        }
    }
    centered(buf, r, y0 + 7, &Line::from(bold(stats, TEXT)));
    centered(buf, r, y0 + 8, &Line::from(span(path, MUTED)));
    centered(buf, r, y0 + 10, &Line::from(bold(hint, GOLD)));
}
