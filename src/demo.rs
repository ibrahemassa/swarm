//! Demo mode: scripted agents, real files, zero tokens. Builds a neon ramen-bar landing page,
//! then keeps playing along when you talk to the swarm.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::sleep;

use crate::app::{AgentEv, Ev, Level, Phase, PlanItem};
use crate::fx::Rng;
use crate::orchestrator::{self, msg, name_word, Cmd, Config, Page, Shared, Tx};
use crate::project::Kind;
use crate::providers::AgentSpec;

pub const TASK: &str = "landing page for NOODLE//NOIR, a neon late-night ramen bar";

struct Section {
    key: &'static str,
    brief: &'static str,
    think: &'static str,
    html: &'static str,
    css: &'static str,
    js: Option<&'static str>,
    summary: &'static str,
}

const BASE_CSS: &str = r##"@import url('https://fonts.googleapis.com/css2?family=Space+Grotesk:wght@400;600;700&family=Noto+Serif+JP:wght@700&display=swap');
/* NOODLE//NOIR design system */
:root {
  --bg: #0b0710;
  --ink: #f6efe8;
  --muted: #a99fb5;
  --neon: #ff2e88;
  --cyan: #21f0ff;
  --amber: #ffb627;
  --card: #160f1f;
  --line: #2a1f38;
  --r: 18px;
  --font: 'Space Grotesk', system-ui, sans-serif;
  --jp: 'Noto Serif JP', serif;
}
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
html { scroll-behavior: smooth; }
body { background: var(--bg); color: var(--ink); font-family: var(--font); line-height: 1.6; overflow-x: hidden; }
a { color: inherit; text-decoration: none; }
.container { width: min(1120px, 92vw); margin-inline: auto; }
section { padding: clamp(64px, 10vw, 128px) 0; }
h1, h2, h3 { line-height: 1.05; letter-spacing: -.02em; }
.eyebrow { font-size: .8rem; letter-spacing: .3em; text-transform: uppercase; color: var(--cyan); }
html.js .reveal { opacity: 0; transform: translateY(24px); transition: opacity .8s ease, transform .8s ease; }
html.js .reveal.in { opacity: 1; transform: none; }
@media (prefers-reduced-motion: reduce) { html.js .reveal { opacity: 1; transform: none; transition: none; } }
"##;

const SECTIONS: &[Section] = &[
    Section {
        key: "hero",
        brief: "Full-viewport hero with the nav, giant neon headline and a ghost kanji backdrop. Owns the site-wide JS (scroll reveals, cursor glow).",
        think: "Reading the design tokens in base.css first.\nHero needs one huge line with a neon glow on the key phrase.\nGhost kanji in the corner, stroked, flickering like a bad sign.\nI also own the global JS: scroll reveals + a cursor glow.\n",
        html: r##"<header id="__ID__" class="hero">
  <nav class="container nav">
    <a href="#__ID__" class="logo">NOODLE<span>//</span>NOIR</a>
    <div class="links"><a href="#menu">Menu</a><a href="#story">Story</a><a href="#visit">Visit</a></div>
  </nav>
  <div class="container hero-body">
    <p class="eyebrow">Open till 3AM · Canal St</p>
    <h1>Ramen for the<br><em>after hours.</em></h1>
    <p class="lede">36-hour tonkotsu, hand-pulled noodles, and a neon glow you can taste.</p>
    <a href="#menu" class="cta">See tonight's bowls →</a>
  </div>
  <div class="kanji" aria-hidden="true">夜麺</div>
</header>
"##,
        css: r##"#__ID__ { position: relative; min-height: 100vh; display: flex; flex-direction: column; overflow: hidden;
  background: radial-gradient(60% 60% at 70% 30%, #ff2e8833, transparent 70%),
              radial-gradient(50% 50% at 20% 80%, #21f0ff22, transparent 70%); }
#__ID__ .nav { display: flex; justify-content: space-between; align-items: center; padding: 28px 0; position: relative; z-index: 2; }
#__ID__ .logo { font-weight: 700; letter-spacing: .08em; }
#__ID__ .logo span { color: var(--neon); }
#__ID__ .links { display: flex; gap: 28px; color: var(--muted); }
#__ID__ .links a:hover { color: var(--ink); }
#__ID__ .hero-body { margin-block: auto; padding-bottom: 12vh; position: relative; z-index: 2; }
#__ID__ h1 { font-size: clamp(3rem, 9vw, 7.5rem); margin: 18px 0; }
#__ID__ h1 em { font-style: normal; color: var(--neon); text-shadow: 0 0 24px #ff2e88aa, 0 0 64px #ff2e8855; }
#__ID__ .lede { max-width: 46ch; color: var(--muted); font-size: 1.15rem; }
#__ID__ .cta { display: inline-block; margin-top: 32px; padding: 14px 26px; border: 1px solid var(--neon); border-radius: 999px; transition: .3s; }
#__ID__ .cta:hover { background: var(--neon); color: var(--bg); box-shadow: 0 0 32px var(--neon); }
#__ID__ .kanji { position: absolute; right: -2vw; bottom: -6vw; font-family: var(--jp); font-size: 32vw; line-height: 1;
  color: transparent; -webkit-text-stroke: 1px #ff2e8840; pointer-events: none; animation: flicker 6s infinite; }
@keyframes flicker { 0%, 47%, 49%, 100% { opacity: 1; } 48% { opacity: .35; } }
@media (max-width: 640px) { #__ID__ .links { display: none; } }
"##,
        js: Some(r##"(() => {
  document.documentElement.classList.add('js');
  const io = new IntersectionObserver(entries => entries.forEach(e => {
    if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
  }), { threshold: 0.15 });
  document.querySelectorAll('.reveal').forEach(el => io.observe(el));
  if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  const glow = document.createElement('div');
  glow.style.cssText = 'position:fixed;width:420px;height:420px;border-radius:50%;pointer-events:none;z-index:1;' +
    'background:radial-gradient(#ff2e8822,transparent 60%);transform:translate(-50%,-50%);transition:left .2s,top .2s';
  document.body.appendChild(glow);
  addEventListener('pointermove', e => { glow.style.left = e.clientX + 'px'; glow.style.top = e.clientY + 'px'; });
})();
"##),
        summary: "Hero + nav shipped, kanji flickers, global reveal + cursor glow live.",
    },
    Section {
        key: "menu",
        brief: "Tonight's bowls as glowing cards with prices in amber; hover lifts the card with a neon edge.",
        think: "Four bowls, auto-fit grid so it collapses cleanly on phones.\nPrices in --amber, numbering in --cyan, cards on --card.\nHover: lift 6px + neon border. Tagging cards .reveal for the hero script.\n",
        html: r##"<section id="__ID__" class="menu">
  <div class="container">
    <p class="eyebrow">Tonight's bowls</p>
    <h2>The Menu</h2>
    <div class="grid">
      <article class="bowl reveal"><span class="num">01</span><h3>Midnight Tonkotsu</h3><p>36-hr pork broth, black garlic oil, chashu, ajitama.</p><b>$18</b></article>
      <article class="bowl reveal"><span class="num">02</span><h3>Neon Miso</h3><p>Red miso, chili crisp, charred corn, butter.</p><b>$17</b></article>
      <article class="bowl reveal"><span class="num">03</span><h3>Ghost Shio</h3><p>Clear chicken broth, yuzu, scallion, nori.</p><b>$16</b></article>
      <article class="bowl reveal"><span class="num">04</span><h3>Tokyo Drift Tsukemen</h3><p>Dipping noodles, fish-pork broth, lime.</p><b>$19</b></article>
    </div>
  </div>
</section>
"##,
        css: r##"#__ID__ h2 { font-size: clamp(2.4rem, 6vw, 4.5rem); margin: 12px 0 48px; }
#__ID__ .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 20px; }
#__ID__ .bowl { position: relative; padding: 28px; background: var(--card); border: 1px solid var(--line);
  border-radius: var(--r); transition: transform .3s, border-color .3s, box-shadow .3s; }
#__ID__ .bowl:hover { transform: translateY(-6px); border-color: var(--neon); box-shadow: 0 20px 60px -20px #ff2e8866; }
#__ID__ .num { font-size: .8rem; color: var(--cyan); letter-spacing: .2em; }
#__ID__ h3 { margin: 10px 0 8px; font-size: 1.35rem; }
#__ID__ p { color: var(--muted); font-size: .95rem; }
#__ID__ b { display: block; margin-top: 18px; color: var(--amber); font-size: 1.2rem; }
"##,
        js: None,
        summary: "#menu live: 4 bowl cards, amber prices, neon hover.",
    },
    Section {
        key: "story",
        brief: "Origin story split layout with three glowing stats (broth hours, bowls a night, last call).",
        think: "Two-column split: big headline left, story + stats right.\nStats glow in --cyan. Collapses to one column under 760px.\n",
        html: r##"<section id="__ID__" class="story">
  <div class="container split">
    <div class="reveal">
      <p class="eyebrow">Since 2019</p>
      <h2>Born in a<br>back alley.</h2>
    </div>
    <div class="reveal">
      <p>We started with one burner, one pot, and a broth that took a day and a half. Six years later the pot is bigger, the hours are worse, and the broth still takes exactly 36 hours.</p>
      <ul class="stats"><li><b>36h</b>broth</li><li><b>412</b>bowls / night</li><li><b>3AM</b>last call</li></ul>
    </div>
  </div>
</section>
"##,
        css: r##"#__ID__ .split { display: grid; grid-template-columns: 1fr 1.2fr; gap: 64px; align-items: end; }
#__ID__ h2 { font-size: clamp(2.4rem, 6vw, 4.5rem); margin-top: 12px; }
#__ID__ p { color: var(--muted); font-size: 1.1rem; }
#__ID__ .stats { display: flex; flex-wrap: wrap; gap: 40px; margin-top: 40px; list-style: none; }
#__ID__ .stats li { color: var(--muted); font-size: .85rem; text-transform: uppercase; letter-spacing: .15em; }
#__ID__ .stats b { display: block; font-size: 2.4rem; color: var(--cyan); letter-spacing: 0; text-shadow: 0 0 18px #21f0ff66; }
@media (max-width: 760px) { #__ID__ .split { grid-template-columns: 1fr; gap: 28px; } }
"##,
        js: None,
        summary: "#story done: split layout, cyan stat glow, stacks at 760px.",
    },
    Section {
        key: "gallery",
        brief: "Bento grid of gradient tiles standing in for photos (broth, counter, noodle pull, last call).",
        think: "No photos yet, so gradient tiles that read like neon-lit shots.\nBento: one tall tile, one wide tile. Captions sit on a dark fade.\n",
        html: r##"<section id="__ID__" class="gallery">
  <div class="container">
    <p class="eyebrow">Steam &amp; neon</p>
    <div class="tiles">
      <div class="tile t1 reveal"><span>Broth, hour 31</span></div>
      <div class="tile t2 reveal"><span>The counter</span></div>
      <div class="tile t3 reveal"><span>Noodle pull</span></div>
      <div class="tile t4 reveal"><span>Last call</span></div>
    </div>
  </div>
</section>
"##,
        css: r##"#__ID__ .tiles { display: grid; grid-template-columns: 2fr 1fr 1fr; grid-auto-rows: 220px; gap: 16px; margin-top: 24px; }
#__ID__ .tile { position: relative; border-radius: var(--r); overflow: hidden; display: flex; align-items: flex-end; padding: 18px; font-weight: 600; }
#__ID__ .tile::after { content: ""; position: absolute; inset: 0; background: linear-gradient(transparent 40%, #000a); }
#__ID__ .tile span { position: relative; z-index: 1; }
#__ID__ .t1 { grid-row: span 2; background: conic-gradient(from 200deg at 30% 60%, #ff2e88, #7b2dff, #21f0ff, #ff2e88); }
#__ID__ .t2 { background: linear-gradient(135deg, #ffb627, #ff2e88); }
#__ID__ .t3 { background: linear-gradient(160deg, #21f0ff, #160f1f); }
#__ID__ .t4 { grid-column: span 2; background: radial-gradient(circle at 80% 20%, #ff2e88, #160f1f 70%); }
@media (max-width: 760px) { #__ID__ .tiles { grid-template-columns: 1fr 1fr; } #__ID__ .t4 { grid-column: span 2; } }
"##,
        js: None,
        summary: "#gallery bento grid in, gradients stand in for photos.",
    },
    Section {
        key: "reviews",
        brief: "Three late-night quotes from regulars on dark gradient cards.",
        think: "Three quotes, auto-fit cards, big type.\nCards reuse --r and --line so they match the menu cards.\n",
        html: r##"<section id="__ID__" class="reviews">
  <div class="container">
    <p class="eyebrow">Overheard at 2AM</p>
    <div class="quotes">
      <blockquote class="reveal">“I've been chasing this broth since Fukuoka.”<cite>Mika, night-shift nurse</cite></blockquote>
      <blockquote class="reveal">“The neon miso fixed my life. Temporarily.”<cite>Dev, startup founder</cite></blockquote>
      <blockquote class="reveal">“Came for one bowl. Stayed for three.”<cite>Sam, DJ</cite></blockquote>
    </div>
  </div>
</section>
"##,
        css: r##"#__ID__ .quotes { display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 20px; margin-top: 24px; }
#__ID__ blockquote { padding: 32px; border-radius: var(--r); background: linear-gradient(180deg, #1d1429, var(--card));
  border: 1px solid var(--line); font-size: 1.25rem; line-height: 1.4; }
#__ID__ cite { display: block; margin-top: 20px; font-style: normal; font-size: .85rem; color: var(--muted); }
"##,
        js: None,
        summary: "#reviews shipped: three quotes, cards match menu radius.",
    },
    Section {
        key: "visit",
        brief: "Footer: address, hours, walk-in policy, huge sign-off line with a neon floor glow.",
        think: "Footer echoes the hero glow from below.\nThree columns: where, when, walk-ins. Big sign-off headline.\n",
        html: r##"<footer id="__ID__" class="visit">
  <div class="container">
    <h2>Pull up a stool.</h2>
    <div class="cols">
      <p><b>Where</b>88 Canal St, New York</p>
      <p><b>When</b>Tue–Sun · 6PM–3AM</p>
      <p><b>Walk-ins</b>Always. No reservations, no regrets.</p>
    </div>
    <small>© NOODLE//NOIR · built by a swarm of agents</small>
  </div>
</footer>
"##,
        css: r##"#__ID__ { padding: 120px 0 48px; border-top: 1px solid var(--line);
  background: radial-gradient(50% 80% at 50% 120%, #ff2e8833, transparent); }
#__ID__ h2 { font-size: clamp(2.6rem, 8vw, 6rem); }
#__ID__ .cols { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 24px; margin: 48px 0 80px; }
#__ID__ .cols p { color: var(--muted); }
#__ID__ .cols b { display: block; color: var(--ink); margin-bottom: 6px; }
#__ID__ small { color: var(--muted); }
"##,
        js: None,
        summary: "#visit footer done, neon floor glow mirrors the hero.",
    },
];

const PEER_LINES: &[&str] = &[
    "nav should link to #__ID__, keep that anchor",
    "accents are var(--neon) only, stay on palette?",
    "my cards use var(--r) radius, match it so we read as one page",
    "my section ends dark at the bottom, blend into it",
    "using .eyebrow from base.css for the small caps labels",
    "heads up: zero global selectors from me, all scoped under #__ID__",
    "tagged my blocks .reveal so the hero script animates them",
];
const REPLIES: &[&str] = &["copy that", "bet, matching it", "on it", "done, synced", "makes sense, adjusting", "love it, mirroring that"];
const LEAD_PINGS: &[&str] = &[
    "halfway there, keep selectors scoped",
    "palette check: neon, cyan, amber only",
    "remember 375px, nothing overflows",
    "nice pace, ship small and clean",
];

pub(crate) fn ms(x: f32) -> Duration {
    Duration::from_millis(x.max(0.0) as u64)
}

pub(crate) fn send(tx: &Tx, idx: usize, ev: AgentEv) {
    let _ = tx.send(Ev::Agent(idx, ev));
}

#[derive(Clone, Copy)]
pub(crate) enum Out {
    Text,
    Thinking,
    Code,
}

/// Dribble text out in small irregular chunks, like a model streaming tokens.
pub(crate) async fn stream(tx: &Tx, idx: usize, kind: Out, text: &str, cps: f32, rng: &mut Rng, tokens: &mut u64) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let n = (2 + rng.below(9)).min(chars.len() - i);
        let chunk: String = chars[i..i + n].iter().collect();
        i += n;
        // pretend there's hidden reasoning behind every visible token
        let hidden = if matches!(kind, Out::Thinking) { 3 } else { 1 };
        let t = (chunk.len() as u64).div_ceil(4) * hidden + rng.below(6) as u64;
        *tokens += t;
        send(tx, idx, AgentEv::Tokens(t));
        send(
            tx,
            idx,
            match kind {
                Out::Text => AgentEv::Text(chunk),
                Out::Thinking => AgentEv::Thinking(chunk),
                Out::Code => AgentEv::Code(chunk),
            },
        );
        sleep(ms(n as f32 / cps * 1000.0 * rng.range(0.6, 1.4))).await;
    }
}

pub(crate) async fn quick_tool(tx: &Tx, idx: usize, name: &str, detail: &str, wait: f32) {
    send(tx, idx, AgentEv::ToolStart(name.into()));
    send(tx, idx, AgentEv::ToolDetail(detail.into()));
    sleep(ms(wait)).await;
    send(tx, idx, AgentEv::ToolDone(false));
}

pub(crate) async fn write_file(tx: &Tx, idx: usize, dir: &Path, rel: &str, body: &str, cps: f32, rng: &mut Rng, tokens: &mut u64) {
    send(tx, idx, AgentEv::ToolStart("Write".into()));
    send(tx, idx, AgentEv::ToolDetail(rel.into()));
    sleep(ms(rng.range(250.0, 600.0))).await;
    stream(tx, idx, Out::Code, body, cps, rng, tokens).await;
    let _ = std::fs::write(dir.join(rel), body);
    send(tx, idx, AgentEv::ToolDone(false));
}

/// Which demo section each worker builds; the footer always goes last.
fn layout(n: usize) -> Vec<(usize, String)> {
    let last = SECTIONS.len() - 1;
    if n == 1 {
        return vec![(0, "hero".into())];
    }
    let mut out: Vec<(usize, String)> = (0..last).take(n - 1).map(|k| (k, SECTIONS[k].key.to_string())).collect();
    let mut round = 2;
    while out.len() < n - 1 {
        for k in 1..last {
            if out.len() >= n - 1 {
                break;
            }
            out.push((k, format!("{}-{round}", SECTIONS[k].key)));
        }
        round += 1;
    }
    out.push((last, SECTIONS[last].key.to_string()));
    out
}

#[allow(clippy::too_many_arguments)]
async fn worker(
    idx: usize,
    name: String,
    lead: String,
    peers: Vec<String>,
    sec: &'static Section,
    id: String,
    dir: PathBuf,
    tx: Tx,
    seed: u64,
) {
    let mut rng = Rng::new(seed);
    let mut tokens = 0u64;
    let cps = rng.range(70.0, 120.0);
    let fill = |s: &str| s.replace("__ID__", &id);
    sleep(ms(rng.range(300.0, 3500.0))).await;
    send(&tx, idx, AgentEv::Turn(format!("build #{id}")));
    send(&tx, idx, AgentEv::Start);
    sleep(ms(rng.range(1200.0, 2600.0))).await;
    stream(&tx, idx, Out::Thinking, sec.think, 45.0, &mut rng, &mut tokens).await;
    quick_tool(&tx, idx, "Read", "styles/base.css", rng.range(900.0, 1800.0)).await;
    quick_tool(&tx, idx, "Glob", "sections/*.html", rng.range(500.0, 1100.0)).await;
    write_file(&tx, idx, &dir, &format!("sections/{id}.html"), &fill(sec.html), cps, &mut rng, &mut tokens).await;

    if !peers.is_empty() {
        let peer = rng.pick(&peers).clone();
        msg(&tx, &name, &peer, fill(*rng.pick(PEER_LINES)));
        let (tx2, name2, peer2) = (tx.clone(), name.clone(), peer.clone());
        let reply = rng.pick(REPLIES).to_string();
        let wait = rng.range(1400.0, 3200.0);
        tokio::spawn(async move {
            sleep(ms(wait)).await;
            msg(&tx2, &peer2, &name2, reply);
        });
    }

    let css = fill(sec.css);
    write_file(&tx, idx, &dir, &format!("styles/{id}.css"), &css, cps, &mut rng, &mut tokens).await;
    if let Some(js) = sec.js {
        write_file(&tx, idx, &dir, &format!("scripts/{id}.js"), &fill(js), cps, &mut rng, &mut tokens).await;
    }

    // polish pass: a real edit that adds keyboard focus rings
    stream(&tx, idx, Out::Thinking, "Polish pass: contrast, focus states, 375px.\n", 45.0, &mut rng, &mut tokens).await;
    quick_tool(&tx, idx, "Grep", &format!("var(-- in styles/{id}.css"), rng.range(600.0, 1200.0)).await;
    let polish =
        fill("\n/* a11y: visible keyboard focus */\n#__ID__ a:focus-visible { outline: 2px solid var(--cyan); outline-offset: 3px; }\n");
    send(&tx, idx, AgentEv::ToolStart("Edit".into()));
    send(&tx, idx, AgentEv::ToolDetail(format!("styles/{id}.css")));
    sleep(ms(rng.range(300.0, 700.0))).await;
    stream(&tx, idx, Out::Code, &polish, cps, &mut rng, &mut tokens).await;
    let _ = std::fs::write(dir.join(format!("styles/{id}.css")), format!("{css}{polish}"));
    send(&tx, idx, AgentEv::ToolDone(false));

    if peers.len() > 1 && rng.f() < 0.6 {
        let peer = rng.pick(&peers).clone();
        msg(&tx, &name, &peer, fill(*rng.pick(PEER_LINES)));
    }
    quick_tool(&tx, idx, "Read", &format!(".swarm/inbox/{name}.md"), rng.range(700.0, 1400.0)).await;
    stream(&tx, idx, Out::Thinking, "Inbox read. Everything lines up with the team.\n", 45.0, &mut rng, &mut tokens).await;
    let summary = fill(sec.summary);
    stream(&tx, idx, Out::Text, &format!("{summary}\n"), 110.0, &mut rng, &mut tokens).await;
    msg(&tx, &name, &lead, summary.clone());
    send(&tx, idx, AgentEv::Cost(tokens as f64 * 15e-6));
    sleep(ms(300.0)).await;
    send(&tx, idx, AgentEv::Done(summary));
}

const TASK_THINK: &[&str] = &[
    "Re-reading my section with that in mind.\nSmallest change that gets there, keep the palette.\n",
    "Checking which selectors this touches.\nScoped edit, no global side effects.\n",
    "Easy one. Adjusting spacing and type scale, then a quick 375px check.\n",
];
const TASK_DONE: &[&str] = &["done, tweak is live", "shipped it, scoped and clean", "updated, check the page", "done. kept it on-palette"];
const LEAD_ROUTES: &[&str] = &[
    "On it. Splitting that across the team now.",
    "Good call. Routing it to the owners, watch the page.",
    "Easy. Handing it out, back in a moment.",
];

/// A follow-up turn: think, read the file, make a (harmless, real) edit, report back.
#[allow(clippy::too_many_arguments)]
async fn task(
    idx: usize,
    name: String,
    reply_to: String,
    label: String,
    ask: String,
    target: Option<Target>,
    dir: PathBuf,
    tx: Tx,
    seed: u64,
) {
    let mut rng = Rng::new(seed);
    let mut tokens = 0u64;
    send(&tx, idx, AgentEv::Turn(label));
    send(&tx, idx, AgentEv::Start);
    sleep(ms(rng.range(700.0, 1600.0))).await;
    stream(&tx, idx, Out::Thinking, &format!("Ask: \"{ask}\"\n{}", rng.pick(TASK_THINK)), 50.0, &mut rng, &mut tokens).await;
    let reply = match target {
        Some(Target { read, rel, note }) => {
            quick_tool(&tx, idx, "Read", &read, rng.range(600.0, 1200.0)).await;
            send(&tx, idx, AgentEv::ToolStart("Edit".into()));
            send(&tx, idx, AgentEv::ToolDetail(rel.clone()));
            sleep(ms(rng.range(300.0, 700.0))).await;
            stream(&tx, idx, Out::Code, &note, rng.range(70.0, 110.0), &mut rng, &mut tokens).await;
            let old = std::fs::read_to_string(dir.join(&rel)).unwrap_or_default();
            let _ = std::fs::write(dir.join(&rel), format!("{old}{note}"));
            send(&tx, idx, AgentEv::ToolDone(false));
            rng.pick(TASK_DONE).to_string()
        }
        None => "I don't own anything yet, ask the lead to give me some work.".to_string(),
    };
    stream(&tx, idx, Out::Text, &format!("{reply}\n"), 110.0, &mut rng, &mut tokens).await;
    msg(&tx, &name, &reply_to, reply.clone());
    send(&tx, idx, AgentEv::Cost(tokens as f64 * 15e-6));
    send(&tx, idx, AgentEv::Done(reply));
}

/// Where a follow-up edit lands: the file to read first, the file to append to, and what to append.
struct Target {
    read: String,
    rel: String,
    note: String,
}

pub(crate) struct Demo {
    pub(crate) cfg: Config,
    pub(crate) tx: Tx,
    pub(crate) rng: Rng,
    pub(crate) names: Vec<String>,
    pub(crate) items: Vec<PlanItem>,
    pub(crate) round: u32,
    pub(crate) in_round: bool,
    pub(crate) work: JoinSet<usize>,
    pub(crate) running: HashMap<usize, AbortHandle>,
    pub(crate) pinging: Option<Arc<AtomicBool>>,
    pub(crate) aborted: bool,
}

impl Demo {
    pub(crate) fn new(cfg: Config, tx: Tx, names: Vec<String>, items: Vec<PlanItem>) -> Demo {
        Demo {
            cfg,
            tx,
            rng: Rng::seeded(),
            names,
            items,
            round: 1,
            in_round: false,
            work: JoinSet::new(),
            running: HashMap::new(),
            pinging: None,
            aborted: false,
        }
    }

    fn code(&self) -> bool {
        self.cfg.kind == Kind::Code
    }

    pub(crate) fn lead(&self) -> String {
        self.names[0].clone()
    }

    /// The file a follow-up edit for `name` goes into, with a harmless note that records the request.
    fn target(&self, name: &str, ask: &str) -> Option<Target> {
        let it = self.items.iter().find(|i| i.agent == name)?;
        if !self.code() {
            let id = &it.id;
            let note = format!("\n/* request: {} */\n#{id} {{ scroll-margin-top: 24px; }}\n", ask.replace("*/", "* /"));
            return Some(Target { read: format!("sections/{id}.html"), rel: format!("styles/{id}.css"), note });
        }
        let rel = it.owns.iter().find(|o| !o.contains('*'))?.clone();
        let ask = ask.replace('\n', " ");
        let note = match rel.rsplit('.').next().unwrap_or("") {
            "md" | "html" => format!("\n<!-- request: {} -->\n", ask.replace("--", "-")),
            "py" | "toml" | "sh" | "yml" | "yaml" | "csv" => format!("\n# request: {ask}\n"),
            _ => format!("\n// request: {ask}\n"),
        };
        Some(Target { read: rel.clone(), rel, note })
    }

    pub(crate) fn start_round(&mut self) {
        if self.in_round {
            return;
        }
        self.in_round = true;
        let _ = self.tx.send(Ev::Phase(Phase::Build));
        let flag = Arc::new(AtomicBool::new(false));
        self.pinging = Some(flag.clone());
        let (tx, lead) = (self.tx.clone(), self.lead());
        let pings = if self.code() { crate::demo_code::LEAD_PINGS } else { LEAD_PINGS };
        tokio::spawn(async move {
            let mut rng = Rng::seeded();
            loop {
                sleep(ms(rng.range(9000.0, 15000.0))).await;
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                msg(&tx, &lead, "all", *rng.pick(pings));
            }
        });
    }

    fn task(&mut self, idx: usize, reply_to: &str, label: String, ask: &str) {
        let name = self.names[idx].clone();
        let target = self.target(&name, ask);
        let seed = self.rng.next();
        let h = self.work.spawn({
            let (reply_to, ask, dir, tx) = (reply_to.to_string(), ask.to_string(), self.cfg.dir.clone(), self.tx.clone());
            async move {
                task(idx, name, reply_to, label, ask, target, dir, tx, seed).await;
                idx
            }
        });
        self.running.insert(idx, h);
    }

    async fn route(&mut self, text: &str) {
        let lead = self.lead();
        let mut tokens = 0;
        send(&self.tx, 0, AgentEv::Turn("message from you".into()));
        send(&self.tx, 0, AgentEv::Start);
        let _ = self.tx.send(Ev::Phase(Phase::Plan));
        sleep(ms(600.0)).await;
        let n = self.names.len().saturating_sub(1);
        if n == 0 {
            return;
        }
        let picks: Vec<usize> = {
            let k = 1 + self.rng.below(2.min(n));
            let mut v: Vec<usize> = Vec::new();
            while v.len() < k {
                let i = 1 + self.rng.below(n);
                if !v.contains(&i) {
                    v.push(i);
                }
            }
            v
        };
        let who = picks.iter().map(|i| self.names[*i].clone()).collect::<Vec<_>>().join(" and ");
        let think = if self.code() {
            format!("User wants: \"{text}\".\nThat touches {who}'s files. Delegating; main.rs and the interfaces stay as they are.\n")
        } else {
            format!("User wants: \"{text}\".\nThat lands in {who}'s sections. Delegating, base.css stays as is.\n")
        };
        stream(&self.tx, 0, Out::Thinking, &think, 70.0, &mut self.rng, &mut tokens).await;
        let reply = self.rng.pick(LEAD_ROUTES).to_string();
        stream(&self.tx, 0, Out::Text, &format!("{reply}\n"), 110.0, &mut self.rng, &mut tokens).await;
        send(&self.tx, 0, AgentEv::Done(reply.clone()));
        msg(&self.tx, &lead, "you", reply);
        self.start_round();
        for i in picks {
            let name = self.names[i].clone();
            msg(&self.tx, &lead, &name, orchestrator::trunc(text, 120));
            self.task(i, &lead.clone(), format!("task from {lead}"), text);
        }
    }

    pub(crate) fn ship(&mut self) {
        self.in_round = false;
        if let Some(f) = self.pinging.take() {
            f.store(true, Ordering::Relaxed);
        }
        if !self.code() {
            let page = Page { title: TITLE.into(), ids: self.items.iter().map(|i| i.id.clone()).collect(), placeholders: false };
            orchestrator::assemble(&self.cfg.dir, &page);
        }
        let _ = self.tx.send(Ev::Phase(Phase::Ship));
        let _ = self.tx.send(Ev::Shipped(self.round));
        self.round += 1;
    }

    pub(crate) async fn review(&mut self) {
        if self.code() {
            return crate::demo_code::review(self).await;
        }
        let mut tokens = 0;
        let n = self.items.len();
        let _ = self.tx.send(Ev::Phase(Phase::Review));
        send(&self.tx, 0, AgentEv::Turn("review the page".into()));
        send(&self.tx, 0, AgentEv::Start);
        sleep(ms(700.0)).await;
        stream(
            &self.tx,
            0,
            Out::Thinking,
            &format!("Reviewing {n} sections: nav anchors, scoping, 375px overflow.\n"),
            90.0,
            &mut self.rng,
            &mut tokens,
        )
        .await;
        quick_tool(&self.tx, 0, "Read", "index.html", 900.0).await;
        quick_tool(&self.tx, 0, "Grep", "href=\"#", 600.0).await;
        stream(
            &self.tx,
            0,
            Out::Text,
            "All anchors resolve, every selector scoped, nothing overflows. Shipping.\n",
            110.0,
            &mut self.rng,
            &mut tokens,
        )
        .await;
        send(&self.tx, 0, AgentEv::Done("review clean".into()));
    }

    async fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Say { to, text } => {
                let lead = self.lead();
                if to.eq_ignore_ascii_case(&lead) || to.eq_ignore_ascii_case("lead") {
                    self.route(&text).await;
                } else if to == "all" {
                    self.start_round();
                    for i in 1..self.names.len() {
                        self.task(i, "you", "message from you".into(), &text);
                    }
                } else if let Some(i) = self.names.iter().position(|n| *n == to) {
                    self.start_round();
                    self.task(i, "you", "message from you".into(), &text);
                }
            }
            Cmd::Add(p, model) => {
                let word = name_word(p, &model);
                let mut k = 1;
                while self.names.contains(&format!("{word}-{k}")) {
                    k += 1;
                }
                let spec = AgentSpec { name: format!("{word}-{k}"), provider: p, model };
                let _ = self.tx.send(Ev::Spawn(spec.clone(), format!("joined the swarm ({} {})", p.label(), spec.model_label())));
                self.names.push(spec.name.clone());
                let what = if self.code() { "some files" } else { "a section" };
                msg(&self.tx, &self.lead(), &spec.name, format!("welcome aboard. Stand by for {what}."));
            }
            Cmd::Stop(who) => {
                let targets: Vec<usize> = match who.and_then(|w| self.names.iter().position(|n| n.eq_ignore_ascii_case(&w))) {
                    Some(i) => vec![i],
                    None => self.running.keys().copied().collect(),
                };
                self.aborted = true;
                for i in targets {
                    if let Some(h) = self.running.remove(&i) {
                        h.abort();
                        send(&self.tx, i, AgentEv::Stopped);
                    }
                }
            }
            Cmd::Check(_) if self.code() && !self.in_round => {
                crate::demo_code::check(&self.tx, &self.cfg.dir, true, "check").await;
            }
            Cmd::Check(_) | Cmd::Run(_) => {
                let _ = self.tx.send(Ev::Log(Level::Warn, "demo mode doesn't run commands".into()));
            }
            Cmd::Review => {
                if !self.in_round {
                    self.review().await;
                    self.ship();
                }
            }
        }
    }
}

const TITLE: &str = "NOODLE//NOIR · ramen after hours";

pub async fn run(cfg: Config, tx: Tx, cmds: UnboundedReceiver<Cmd>) {
    if cfg.kind == Kind::Code {
        return crate::demo_code::run(cfg, tx, cmds).await;
    }
    let shared = Shared::default();
    *shared.names.lock().unwrap() = cfg.roster.iter().map(|s| s.name.clone()).collect();
    let url = crate::server::start(cfg.dir.clone(), shared.version.clone()).await;
    if let Some(u) = &url {
        let _ = tx.send(Ev::Server(u.clone()));
    }
    tokio::spawn(orchestrator::watch_files(cfg.dir.clone(), tx.clone(), shared.clone(), crate::project::Kind::Page));
    sleep(ms(2200.0)).await; // let the boot sequence breathe

    let mut rng = Rng::seeded();
    let mut tokens = 0u64;
    let names: Vec<String> = cfg.roster.iter().map(|s| s.name.clone()).collect();
    let lead = names[0].clone();
    let workers: Vec<(usize, String)> = names.iter().cloned().enumerate().skip(1).collect();
    let n = workers.len();
    if n == 0 {
        log_line(&tx, "demo needs at least one worker");
        return;
    }

    let _ = tx.send(Ev::Phase(Phase::Plan));
    send(&tx, 0, AgentEv::Turn("plan the page".into()));
    send(&tx, 0, AgentEv::Start);
    sleep(ms(900.0)).await;
    let think = format!(
        "Request: a landing page for a late-night ramen bar.\nSplitting the page into {n} sections, one owner each, top to bottom.\nArt direction: ink-black, neon pink accent, cyan for data, amber for prices.\nType: Space Grotesk for UI, Noto Serif JP for a giant ghost kanji.\nEveryone scopes CSS under their section id so nobody collides.\nDesign system first. Writing base.css.\n"
    );
    stream(&tx, 0, Out::Thinking, &think, 90.0, &mut rng, &mut tokens).await;
    write_file(&tx, 0, &cfg.dir, "styles/base.css", BASE_CSS, 260.0, &mut rng, &mut tokens).await;
    stream(&tx, 0, Out::Text, &format!("Plan locked: {n} sections. Dispatching the team.\n"), 110.0, &mut rng, &mut tokens).await;
    send(&tx, 0, AgentEv::Cost(tokens as f64 * 75e-6));
    send(&tx, 0, AgentEv::Done("plan locked".into()));

    let picks = layout(n);
    let items: Vec<PlanItem> = picks
        .iter()
        .zip(&workers)
        .map(|((k, id), (_, name))| PlanItem {
            id: id.clone(),
            agent: name.clone(),
            brief: SECTIONS[*k].brief.to_string(),
            owns: Vec::new(),
        })
        .collect();
    let page = Page { title: TITLE.into(), ids: items.iter().map(|i| i.id.clone()).collect(), placeholders: true };
    *shared.page.lock().unwrap() = Some(page.clone());
    orchestrator::assemble(&cfg.dir, &page);
    let _ = tx.send(Ev::Plan(TITLE.into(), items.clone()));
    for it in &items {
        let brief: String = it.brief.chars().take(90).collect();
        msg(&tx, &lead, &it.agent, format!("you own #{}: {brief}", it.id));
        sleep(ms(150.0)).await;
    }

    let mut d = Demo::new(cfg.clone(), tx.clone(), names.clone(), items.clone());
    d.start_round();
    if cfg.live {
        if let Some(u) = &url {
            orchestrator::open(u);
        }
    }
    let peers_all: Vec<String> = workers.iter().map(|(_, n)| n.clone()).collect();
    for ((k, id), (idx, name)) in picks.into_iter().zip(workers.iter().cloned()) {
        let peers: Vec<String> = peers_all.iter().filter(|p| **p != name).cloned().collect();
        let seed = rng.next();
        let (lead, dir, tx) = (lead.clone(), cfg.dir.clone(), tx.clone());
        let h = d.work.spawn(async move {
            worker(idx, name, lead, peers, &SECTIONS[k], id, dir, tx, seed).await;
            idx
        });
        d.running.insert(idx, h);
    }
    d.serve(cmds, shared).await;
}

impl Demo {
    /// Play along with the command bar until the dashboard closes. The first round finishes with the
    /// review (and, in code mode, the scripted check-and-fix); later rounds just ship.
    pub(crate) async fn serve(mut self, mut cmds: UnboundedReceiver<Cmd>, shared: Shared) {
        let mut first = true;
        loop {
            tokio::select! {
                c = cmds.recv() => match c {
                    Some(c) => self.command(c).await,
                    None => break,
                },
                Some(r) = self.work.join_next(), if !self.work.is_empty() => {
                    if let Ok(idx) = r {
                        self.running.remove(&idx);
                    }
                    if !self.work.is_empty() || !self.in_round {
                        continue;
                    }
                    if self.aborted {
                        self.aborted = false;
                        self.in_round = false;
                        if let Some(f) = self.pinging.take() {
                            f.store(true, Ordering::Relaxed);
                        }
                        let _ = self.tx.send(Ev::Phase(Phase::Ship));
                        let _ = self.tx.send(Ev::Log(Level::Warn, "round stopped".into()));
                        continue;
                    }
                    if self.code() {
                        if first {
                            crate::demo_code::finish_first(&mut self).await;
                        } else {
                            crate::demo_code::check(&self.tx, &self.cfg.dir, true, "check").await;
                            self.ship();
                        }
                    } else {
                        if first && self.cfg.review {
                            self.review().await;
                        }
                        let ids = self.items.iter().map(|i| i.id.clone()).collect();
                        *shared.page.lock().unwrap() = Some(Page { title: TITLE.into(), ids, placeholders: false });
                        self.ship();
                    }
                    first = false;
                }
            }
        }
    }
}

fn log_line(tx: &Tx, s: &str) {
    let _ = tx.send(Ev::Log(Level::Bad, s.into()));
}
