# rozu: Handoff spec (CURRENT: v7.3, live activity, flow orbs)
**Status:** current spec as of 2026-10-13 (v7.2 approved and added to rozu; **v7.3** redesigns Searching, Notes, Waiting, Done, Error and Offline to match the liked Thinking/Reading/Editing/Running). v7.2 replaces the v6.x bead indicator and the v5 progress line with a **3D dot-orb activity pill**: a spherical cloud of dots with radial flow, a label, and a timer. Step progress is text in the pill (`3/5`). There's no progress bar.
- Everything v7.2 doesn't redefine comes from v5 (Appendix B: chrome ramp, type, spacing, focus) and then v1 (Appendix A: layout, keyboard map, approvals, accessibility).
- **Superseded:** v6.x (Appendix C), meaning the bead family, the bead tokens and the v5 progress line under the pill.
- **Screens:**
  - `screens/v7.3-01-activity-states-{dark,light}.png` and `@2x`: all ten states plus the dock in context.
  - `screens/v7.3-02-closeup-*`: 4×.
  - `screens/v7.3-22px-compare.png`: the 22px pill orbs at 8× nearest-neighbour.
  - `screens/v7.2-activity-morph-{dark,light}.mp4`: 60fps.
- **Source:** `html/v7.3-orb.js` is the reference implementation (v7.3 fixes the dot-size bug: per-state size `s` now applies). It's pure and deterministic: `sample(timeline, T)` → points, then `drawOrb`. Also `html/v7.css` and `html/v7.2-morph.html`.
- **Specs:** `inspo/motion-spec-v7.3.md` (six redesigned states, size fix), `inspo/motion-spec-v7.2.md` (cues, depth and light, tempo, pulse), on top of `inspo/motion-spec-v7.1.md` (flow language) and `inspo/motion-spec-v7.md` (projection, morph, pill).
- **Contrast:** `contrast-v7.3.md`, 0 failures.
- **Tokens:** CSS load order `tokens.css` → `tokens-v4.css` → `tokens-v5.css` → `tokens-v6.css` → **`tokens-v7.css`**. JSON: `tokens.json` + `tokens-v5.json` + `tokens-v6.json` + **`tokens-v7.json`**.

## Change history
| Version | Date | Change | Status |
|---|---|---|---|
| v1 – v4.1 | Oct 13 | Base system, accents, orb/rose/vortex explorations | Superseded / rejected |
| v5 | Oct 13 | Hero pill + stem + line, pure chrome | Superseded. The hero card was removed in v6 and the line in v7 |
| v6 – v6.2 | Oct 13 | 36-bead chrome indicator family, crisp pass | **Superseded by v7.2** (Appendix C) |
| v6.3 – v6.4 | Oct 13 | Flat monochrome bead explorations | Rejected ("forget this style") |
| v7 | Oct 13 | New from scratch: 3D dot orbs (perspective, depth size/fade, depth sort), a distinct shape per state, no bar | Superseded |
| v7.1 | Oct 13 | One language: every state is a spherical radial-flow cloud. Converge becomes Thinking, and states vary only by flow | Folded into v7.2 |
| v7.2 | Oct 13 | A bold signature cue per state (readable at 22px), stronger depth plus a top-left light, a distinct tempo per state, and a 150ms brighten pulse on change | Approved, added to rozu; partly superseded by v7.3 |
| **v7.3** | Oct 13 | Searching, Notes, Waiting, Done, Error and Offline redesigned to match the liked four (lens, ink stroke, pause bars, halo, break, broken ring). Fixes the JS bug that ignored per-state dot size | **Current** |

## 1. Tokens (tokens-v7, layered after v6; no new hues)
| Token | Value | Use |
|---|---|---|
| color.orbDot | dark #F4F1EA / light #25242A | Neutral dots |
| color.orbPillBg / orbPillBorder | #1B1B1D, #2A2A2E / #ECECEF, #DCDCE0 | Pill |
| color.orbLabel / orbMeta | #F4F1EA, #9A9AA2 / #25242A, #55555C | Label / timer + hint |
| color.warning / success / danger (orb) | dark #FBBF24 #4ADE80 #F87171 / light #8A5206 #126B33 #B91C1C | Status dots + labels (light darkened for 4.5:1) |
| orb.points / points22 | 120 / 60 (max 120) | Dots per orb (≥30px / 22px) |
| orb.camera | 3.2 | Perspective k = F/(F−z) |
| orb.depthAlpha / depthSize | .12→1 / .40→1 | Far→near |
| orb.light | (−.55, .6, .58), α×(.62+.48λ), size×(.9+.2λ) | Top-left light |
| orb.dotBase / dotBase22 | .017·size / .05·size | Dot radius |
| orb.litBoost | r×1.55, α+.5 | Cue dots |
| motion.orbMorph / orbStagger / orbLift | 600ms crit. spring ω7 / 2.5ms per pt / .55 | Morph |
| motion.orbPulse | 150ms, lit + .8·sin(πt/.15) | On every change |
| motion.orbTempo.* | thinking 1.6s, searching 2.4s, reading 2.2s/4.4s, notes 3.6s, editing 530ms, running 800ms, waiting 2.6s, offline 300ms ticks | Per state |
| size.orb / orbLg / activityPill | 22px / 88px / 32px (34px in the dock) | Sizes |
**Deprecated (v6):** `bead.*`, `size.beadDiameter22`, `motion.morph` 480ms, `trail`, `arcBow`, and the v5 line tokens (`size.seg*`, sweep, notch) as pill companions.

## 2. Activity pill + orb
- **Placement:** in the chat dock, above the composer. Pill contents: [orb 22px] [label] [mono timer] [`· 3/5` step] [hint: Esc stop · Y / N · R retry]. There's no bar or line.
- **Pill:** 32px (34px in the dock), fully rounded, `orbPillBg` with a 1px `orbPillBorder`. The label is 500 weight in `orbLabel`, or the status colour for Waiting, Done and Error. The timer and hint use `orbMeta`.
- **Orb:** a spherical 3D cloud with 120 points (60 at 22px, via the subsample `((i>>1)+i)%2==0`). Each dot i has a Fibonacci-sphere direction dᵢ and a phase φᵢ = (t/T + hash i) mod 1, and states differ only in how the flow behaves.
  - **Projection:** perspective k = 3.2/(3.2−z), depth-sorted every frame.
  - **Depth:** α .12→1, size .4→1.
  - **Light:** a top-left light term, then the lit boost, then the per-state α multiplier.
  - **Colour:** neutral dots, with hue only for Waiting, Done and Error. Offline is α×.85.
- **Accessibility:**
  - The pill is `role=status`, `aria-live=polite`, announced once per state.
  - The orb is `aria-hidden`; the label carries the state.

## 3. States (per-state cue and tempo; math in motion-spec-v7.2.md for the liked four, motion-spec-v7.3.md for the other six)
| State | Label | Signature cue (22px + still frame) | Flow / tempo | Reduced-motion frame (T) |
|---|---|---|---|---|
| Thinking | Thinking… | Bright dense core | Converge: dots rush in (φ^.55) and linger at a lit core · 1.6s | .9s |
| Searching | Searching "`<q>`" | Scan lens: a lit cap orbiting the rim | The cap orbits over 2.4s; the faded rest flows in | .5s |
| Reading | Reading `<file>` | Tilted lit diagonal band | Gentle inflow 2.2s; the band slides over 4.4s | .5s |
| Notes | Jotting down notes… | Ink stroke: a lit comet on a wavy latitude | The stroke writes round over 1.5s; the faded cloud drifts out over 3.0s | 1.1s |
| Editing | Editing `<file>` | Tight compressed ball with a right-hand gap | Clicks inward every 530ms | .06s |
| Running | Running `<cmd>` | 2 spiral arms | Fast inflow with a twist that grows inward · 0.8s | .35s |
| Waiting for approval | Waiting for approval · Y / N | Amber pause bars: two lit vertical bands | The frozen cloud breathes ±6% over 2.6s | .65s |
| Done | Done · 42s · 2 files | Green halo: a bright still rim | One pole-to-pole ripple (1.1s), then still | 1.6s |
| Error | Command failed · R retry | Red break: a lit third flung out, dim core | Stalls .18s, flings once (ζ≈.55), then holds | 1.0s |
| Offline / retrying | Offline · retrying in 3s | Dim broken vertical ring with a gap | A spark sputters across the gap on .3s ticks | .35s |

## 4. Motion
- **Morph:** the same point set is used throughout, so a change interpolates point i to point i.
  - Critically damped spring, 600ms (ω=7), with a 2.5ms per-point stagger.
  - Mid-morph z lift of +.55·sin(πq) toward the camera.
  - Lit, size and α multiplier are interpolated. Tone and dim flip at q = .5.
  - The previous state keeps generating during the morph, so nothing restarts, and an interrupt starts from the live sample.
- **Brighten pulse:** on every state change, lit += .8·sin(π·t/.15) over the first 150ms.
- **Tempo:** per state, as in §3. Done and Error play once, then settle.
- **Crispness:** at 22px, dots ≤1.6 device px are drawn as pixel-snapped squares; larger dots are AA discs. Text is regular AA text, never drawn into the orb.

## 5. Reduced motion
- Triggered by the OS setting or `prefers-reduced-motion`.
- Each state shows one static frame (T in §3), and each still frame shows its signature cue.
- State changes use a 150ms cross-fade instead of the morph. The brighten pulse becomes a single 150ms α bump with no movement.
- Done's ripple, Error's scatter, Editing's clicks and Waiting's breath are all off. The label carries the state.

## 6. GPUI notes
- **One instanced draw:** a single quad pipeline with **at most 120 instances** per orb. Instance data: `{pos: vec2, radius, alpha, tone_idx}`.
- **CPU per frame:**
  - `gen(state, i, t)` for each point, ported 1:1 from `v7.2-orb.js`: trig only, under 0.05ms.
  - Morph blend and pulse, projection and light.
  - An insertion sort by z (nearly sorted from frame to frame).
- **Fragment:** a disc SDF with a ~1 device-px smoothstep; at 22px, tiny dots take the pixel-snapped square branch. Snapping uses `window.scale_factor()`.
- **Frame policy:**
  - 60fps while visible and focused; 30fps on battery or thermal pressure.
  - **Pause (0fps) when the window is unfocused or occluded**, and once Done or Error has settled (1.3s). Resume from the current T with no jump.
  - Reduced motion: render only on state change.
- **Testing:** sampling is deterministic. Unit-test `sample()` against the JS (same points at fixed T), and golden-image the 22px orbs against `screens/v7.2-22px-compare.png`.
- **Tauri:** the same JS on `<canvas>` at devicePixelRatio, paused on `visibilitychange` / blur.
- **Theme:** generate from `tokens.json` + v5 + v6 + `tokens-v7.json`.

## 7. Acceptance criteria (v7.2)
1. While the agent runs, the pill shows the orb, the action in words, the timer and the step text. There's no bar, line or hero card.
2. All ten states are distinct at a glance at 22px (see `screens/v7.3-22px-compare.png`), at 1× and 2×, in both themes, by their signature cue (white states) or hue (status).
3. Each orb has ≤120 points. The orb reads as a lit 3D sphere (near/far size and brightness, top-left light).
4. State changes morph point-to-point in 600ms with no restart, plus a 150ms brighten pulse. Each state runs at its own tempo.
5. Every pair in `contrast-v7.3.md` passes. Labels ≥4.5:1. Front dots ≥3:1 at the worst light; the tightest are light amber 3.27, light green 3.36, light red 3.76, and dark red 3.96.
6. Reduced motion: no animation apart from a 150ms cross-fade or α bump, and each still frame shows its cue.
7. Frame policy holds: 60fps focused, 30fps on battery, 0fps when unfocused, occluded or settled. Under 0.2ms CPU and under 0.1ms GPU per frame.
8. Components use only semantic tokens (tokens-v7 on top of v6/v5). No raw hex, and no deprecated bead or line tokens.

---

# Appendix C: v6.2 spec (SUPERSEDED by v7.2 above; the bead indicator family and the progress line under the pill are REPLACED by the orb pill)
**Status:** current spec as of 2026-10-13 (v6.2 approved by Fred). v6.2 replaces the v5 hero card with a compact **activity pill** that shows what the agent is doing in words, plus a 36-bead chrome indicator per state. It sits above the v5 progress line.
- Everything v6.2 doesn't redefine comes from v5 (Appendix B: tokens, line, states, focus) and then v1 (Appendix A: layout, keyboard map, approvals, accessibility).
- **Screens:**
  - `screens/v6.2-01-activity-states-{dark,light}.png` and `@2x`: all ten states plus the in-context dock.
  - `screens/v6.2-02-closeup-*`: 4× bead detail.
  - `screens/v6.2-crispness-compare.png`: 22px at 1×/2×.
  - `screens/v6.2-activity-morph-{dark,light}.mp4`: 60fps.
- **Source:** `html/v6.2-activity.js` (reference implementation: a pure, deterministic `sampleAll(timeline, T)`), `html/v6.2-activity.css`, `html/v6.2-morph.html`.
- **Specs:** `inspo/motion-spec-v6.2.md` (crispness) on top of `inspo/motion-spec-v6.1.md` (states, choreography, pill).
- **Contrast:** `contrast-v6.2.md`, 38 pairs, 0 failures.
- **Tokens:** CSS load order `tokens.css` → `tokens-v4.css` → `tokens-v5.css` → **`tokens-v6.css`**. JSON: `tokens.json` + `tokens-v5.json` + **`tokens-v6.json`**.

## Change history
| Version | Date | Change | Status |
|---|---|---|---|
| v1 – v4.1 | Oct 13 | Base system, accents, orb/rose/vortex explorations | Superseded / rejected (see Appendix B) |
| v5 | Oct 13 | Hero pill + stem + line, pure chrome | Superseded. **Hero card REMOVED in v6**; line, tokens and states still apply |
| v6 | Oct 13 | Hero card removed. Live activity pill: words + a 36-bead indicator family, 10 states, spring morphs | Superseded by v6.1/v6.2 |
| v6.1 | Oct 13 | Craft pass: chrome beads (specular, band, depth), per-state character, arc morphs with nearest-neighbour assignment and trail, Done settle, typing label, width spring, hover detail | Folded into v6.2 |
| **v6.2** | Oct 13 | Crisp pass: 22px optical layouts, pixel snapping at rest, ~1 device px AA, hard specular, sharp band, 2-frame trail, higher floor, 0.5px rim | Superseded by v7.2 |

## 1. Tokens (tokens-v6, layered after v5; no new hues)
| Token | Value | Use |
|---|---|---|
| size.activityIndicator / Lg | 22px / 88px | Pill glyph (22px optical layout) / large layout |
| size.activityPill | 32px (34px in the dock) | Pill height |
| size.beadDiameter22 | 2.4–3.2px, whole device px | Bead size at 22px |
| bead.floor | dark .72 / light .90 | Minimum drawn opacity (≥3:1) |
| bead.stripe | dark .14 / light .24 | Reflection-band dark stripe |
| bead.rim | dark chrome-hi @ .42 / light chrome-lo @ .85, 0.5px | Crisp rim (beads ≥3.5 device px) |
| bead.shadowAlpha | .32 / .12 | Contact shadow (≥6 device px) |
| bead.snapRestThreshold | 0.35 device px/frame | Pixel snapping at rest |
| motion.morph | 480ms, spring ω7 ζ1 | All state morphs |
| motion.morphSettleDone | 1 − e^(−6u)(cos 11u + 0.55 sin 11u) | Done only |
| motion.morphStagger / arcBow | 4ms per slot / 0.32 | Choreography |
| motion.trail | 2 frames, α .12/m | Soft trail |
| motion.labelType / pillWidth / caretBlink | 22ms per char / 320ms spring / 530ms | Pill |
| motion.doneCollapse / hoverDetail | 2000ms / 300ms | Pill |
Bead colours reuse `chrome-hi/mid/lo/spec` (v5) and `warning/success/danger`. **Deprecated:** `size.hero`, `size.vortexHero`, `color.hero*` and the glow tokens.

## 2. Activity pill + indicator (replaces the hero card)
- **Placement:** in the chat dock above the composer: [pill] … "step 3 of 5 · 40%" (mono, muted). Directly below sits the v5 progress line (Appendix B §2, unchanged), and a 1px `notch-ink` tick marks the current segment.
- **Pill:** 32/34px, chrome-rim border. Contents:
  - the 22px indicator;
  - the label (500, primary text, or a status token for status states);
  - the mono tabular elapsed timer;
  - an optional hint (Esc stop, Y / N, R retry).
- **Label:** types in at 22ms/char with a 1px caret while typing. The width springs over 320ms and is clipped, so text never spills.
- **Hover / focus:** a 3px soft ring, and after 300ms a detail popover: file and line range, ± diff, step name, elapsed time, shortcuts. It dismisses on leave or Esc and is focus-reachable.
- **Indicator family:** 36 identical chrome beads. A state is a target layout; beads not needed merge in (s = 0).
  - **Light:** one top-left light (shared with the line's done-segment sheen).
  - **Per bead:** hard specular, a reflection band whose horizon is set by screen y, depth z (±12% size), depth-sorted.
  - **Hue:** only for status: amber waiting/offline, green done, red error.
- **Accessibility:**
  - The pill is `role=status`, `aria-live=polite`, announced once per state (not per character or timer tick).
  - The indicator is `aria-hidden`.
  - The line keeps `role=progressbar` with `aria-valuetext`.

## 3. States
| State | Label | 22px glyph | Motion | Loop | Reduced-motion frame |
|---|---|---|---|---|---|
| Thinking | Thinking… | 9-bead loose cluster | Organic drift (incommensurate sines) + diagonal depth wave | 2.4s wave, non-repeating drift | Static cluster |
| Notes | Jotting down notes… | 3 ragged lines + pen bead | Pen writes, lifts at line end | 2.2s | Half-written |
| Reading | Reading `<file>` | Page with folded corner + text rows | Eye fixes row by row (step) | 1.8s | Row 2 lit |
| Searching | Searching "`<q>`" | Lens ring + handle | Hops between 3 spots with a settle; glint orbits the rim | 1.8s | First spot |
| Editing | Editing `<file>` | I-beam + typed chars | Types, then a hard-cut blink | 3.3s / 530ms | Caret on |
| Running | Running `<cmd>` | `›` + typed chars + cursor + scrollback | Cursor steps 150ms per cell | 1.2s | 3 chars |
| Waiting for approval | Waiting for approval · Y / N | Amber pause bars | Breath in depth only | 2.6s | z = 0 |
| Done | Done · 42s · 2 files | Green check, short limb first | Settle bounce (only state that bounces); pill collapses after 2s | once | Check |
| Error | Command failed · R retry | Red ✕ | 3-cycle damped shake (360ms), then still | once | ✕ |
| Offline / retrying | Offline · retrying in 3s | Amber 260° arc | Turns; lead bead tries the gap; countdown | 2.4s | Static arc |

## 4. Motion
- **Morph:** on a state change, beads are assigned to new slots by greedy nearest-neighbour from their live positions (visible first), which avoids crossings.
  - Each bead travels an arc bowed 0.32 × travel away from the glyph centre.
  - Spring: 480ms ω7 with no overshoot. Done uses the settle spring.
  - Stagger: 4ms per slot in draw order, so glyphs draw themselves.
  - Size, z and opacity follow the non-overshooting progress. Hue swaps per bead at q = 0.5.
  - The previous state keeps animating as beads leave it, so nothing restarts. Interrupts start from the current position.
- **Trail:** 2 ghost samples (1/60s apart, α 0.12/0.06, r × 0.75), only while a bead moves faster than 0.025 unit/frame.
- **Crispness** (detail in motion-spec-v6.2.md):
  - **22px optical layouts.** Bead Ø 2.4–3.2px, a whole number of device px.
  - **Pixel snapping at rest.** A resting bead (not morphing and under 0.35 device px/frame) snaps to the device grid: odd Ø → pixel centre, even Ø → pixel corner. Moving beads stay sub-pixel.
  - **Tiny resting beads** (≤5 device px) are exact pixel masks.
  - **Larger beads** get a ~1 device px smoothstep edge and a tight body gradient.
  - **Specular:** r × 0.13 hard (a single device pixel when tiny; none on tiny light-theme beads).
  - **Band:** the horizon edge is 2% wide.
  - **0.5px rim.**
  - **Contact shadow:** only at ≥6 device px. There's no glow or bloom.
- Pill timings are as in §2. Line timings are unchanged from Appendix B §4 (step fill 240ms, sweep 2200ms). The pill no longer travels: there's no stem or vortex.

## 5. Reduced motion
- Triggered by the OS setting or `prefers-reduced-motion`.
- Each state shows its static frame (§3) with a 120ms cross-fade between states.
- There are no arcs, trails, typing, drift, shake, bounce or width spring (the width snaps).
- All beads are at rest, so all are pixel-snapped.
- The line sweep is static (Appendix B §5). Labels carry all state.

## 6. GPUI notes
- **One instanced draw:** a single pipeline with quad instances: 36 beads plus up to 72 trail ghosts. Instance data: `pos, r, alpha, z, tone, kind (bead | trail | pixel-mask), snapped`.
- **CPU side:** `gen_state`, spring, arc, trail and assignment are ported 1:1 from `v6.2-activity.js`.
  - The assignment runs once per transition (≈20µs).
  - Snapping and the whole-device-px diameter use `window.scale_factor()`.
- **SDF edges:** the fragment shader draws a disc SDF with `smoothstep(r−0.5/scale, r+0.5/scale, d)` (~1 device px AA).
  - Body: a 3-stop radial gradient from tokens.
  - Band: a step on y with a 1-device-px smoothstep.
  - Specular: a hard disc. Rim: 1 device px.
  - Contact shadow: a second SDF ellipse in the same shader.
  - Tiny resting beads take the exact pixel-mask branch with no AA.
- **Budget:** under 0.2ms CPU and under 0.1ms GPU per frame at 1× and 2×.
- **Frame policy:**
  - 60fps while an animated state is visible and the window is focused; 30fps on battery or thermal pressure.
  - 0fps when unfocused or occluded (one static frame), and once Done/Error have settled.
  - Resume from the current T with no jump.
- **Testing:** sampling is deterministic, so golden-image tests can compare the 22px glyphs at scale 1 and 2 against `screens/v6.2-crispness-compare.png` crops.
- **Tauri:** the same JS on `<canvas>` at devicePixelRatio, paused on `visibilitychange`.
- **Theme:** generate `theme.rs` from `tokens.json` + `tokens-v5.json` + `tokens-v6.json`.

## 7. Acceptance criteria (v6.2)
1. No hero card. While the agent runs, the pill shows the action in words, with the right state glyph, the timer and the v5 line beneath. Percent = done/total (v5 rule).
2. All ten states are identifiable by silhouette alone at 22px, at 1× and 2×, in both themes. Hue is only an extra cue for status.
3. Resting beads are on the device-pixel grid: no sub-pixel blur in a static frame at 1× or 2× (golden image). Moving beads are sub-pixel with no stepping.
4. State changes morph with no restart and no bead crossings, in ≤630ms. Only Done overshoots. Trails are ≤2 frames.
5. The label types in, the width springs without clipping text, and screen readers announce each state once.
6. All pairs in `contrast-v6.2.md` pass: label/text ≥4.5:1, beads at the floor ≥3:1 including the band stripe (tightest: dark bead lo edge 3.02, dark red 3.94, light amber 4.09).
7. Reduced motion: zero animation frames apart from a 120ms cross-fade; every state still has a meaningful static frame.
8. Frame policy holds: 0fps when unfocused, occluded or settled; under 0.2ms CPU and under 0.1ms GPU per frame; 60/30fps caps.
9. Components use only semantic tokens (tokens-v6 on top of v5). No raw hex, and no hero tokens are referenced.

---

# Appendix B: v5 spec (SUPERSEDED by v6.2, itself superseded by v7.2; the hero card and progress line are REMOVED)
> **Removed in v6:** the 40% hero card (eyebrow, step title, big %, stage and glow, vortex, 40px hero pill, 2×12 stem) and its tokens (`size.hero`, `size.vortexHero`, `color.hero*`, glow). **Still current from v5:** the chrome ramp and semantic tokens, type, spacing, elevation, §3 component states (except the hero pill row), the progress line (segments, notch, honest fill, current ring, sweep), focus rules and contrast-v5.md.
**Status:** current spec as of 2026-10-13. v5 supersedes v1–v4.1 wherever they conflict. Everything v5 does not redefine (layout regions, keyboard map, component states, approval and plan behaviour, accessibility rules) still comes from the v1 spec in Appendix A.
**Screens:** `screens/v5-01-hero-{dark,light}.png` (chat mid-job with the hero) and `screens/v5-02-states-polish-{dark,light}.png` (hover/focus/pressed/disabled, hero states, transitions). Source: `html/v5-*.html`, `html/v5-hero.css`, `html/v5-states.css`. Still frames are rendered with reduced motion, which by spec holds the sweep mid-segment.
**Tokens:** load `tokens.css` → `tokens-v4.css` → `tokens-v5.css` (CSS). JSON: `tokens.json` (v1 base primitives and neutrals) + `tokens-v5.json` (v5 additions and overrides). v1 base files are intentionally unchanged. `tokens-v4.css` is loaded only for its size, motion and status tokens; its Tide accent vars are overridden by v5.

## Change history
| Version | Date | Change | Status |
|---|---|---|---|
| v1 | Oct 13 | Base system, blue accent, 7 screens | Superseded (layout, keyboard map and states still apply) |
| v1.1 | Oct 13 | Dried-rose taupe accent, plan panel, accept-all undo | Superseded |
| v2 | Oct 13 | Inspo-led orb + timeline draft | Rejected (too close to references) |
| v3 | Oct 13 | Rose petal mark, Ledger | Rejected (rose motif dropped). The Ledger was kept |
| v4 / v4.1 | Oct 13 | Vortex pill, honest step progress (hex, then thin depth line), Tide then Iris accent | Superseded |
| **v5** | Oct 13 | Hero pill + stem + line sharing one light, **pure chrome** accent, type and elevation upgrade, polish states | **Current** |

## 1. Tokens
### Chrome ramp (neutral cool steel; no hue tint anywhere)
| Token | Hex | Use |
|---|---|---|
| chrome.50 | #F2F4F7 | dark: highlight / specular band, **current ring** |
| chrome.100 | #DDE1E7 | dark: primary hover |
| chrome.200 | #C9CED6 | dark: accent text, focus, primary button bg |
| chrome.300 | #B9BFC8 | dark: mid stop |
| chrome.500 | #7A818C | dark: lowest stop (contrast judged here) |
| chrome.550 | #666D78 | light: lightest stop (contrast judged here) |
| chrome.650 | #555C67 | light: mid stop |
| chrome.750 | #3E4550 | light: accent text, focus, primary button bg |
| chrome.850 | #2E343D | light: deepest stop, **current ring**, shine bands |
| chrome.900 | #23282F | light: primary pressed |
| track-now (dark / light) | #23272F / #EAEDF2 | current segment track |
| spec | #FFFFFF | dark specular, and in light the **current segment's** core only |
Semantic mapping (`accent-fg/bg/on`, `focus`, `chrome-hi/mid/lo/spec`, `seg-*`, `notch-ink`, `ledger-current-bar`) per theme is in `tokens-v5.json` → `theme`. Text never sits on a gradient. Primary buttons use a solid chrome tone.

### Type scale
12 / 13 / 15 / 20 / 28 px (`font.size.*`). The hero step title is 20/600, tracking −0.015em. The hero percentage is 28 mono 500 tabular. The eyebrow is 12/600 caps, +0.06em. Body line-height is 1.6, headings 1.2. UI text stays at 13.

### Spacing, radius, sizes
Adds space 7/10/16 (28/40/64) and radius-xl 16 (hero card). Sizes:
- hero 800, conversation column 760
- hero pill 40, vortex 24
- segment 6, **current segment 10**
- notch gap 8, notch tick 12
- stem 12×2
- glow 260×28 (kept smaller than the gap between line and labels, so no text sits on glow)
- sweep 72% of the segment

### Elevation
app → surface → hero card.
- Dark raises surfaces by lightness plus a 1px inset top highlight (`--hero-shadow`, `--hero-rim`) and deep soft shadows.
- Light uses white surfaces with layered soft shadows (`--shadow-1/2/3`) and a white inner rim.
- Borders are 1px hairlines (`--hero-border`) with an inner highlight. No elevation through colour hue.

### Motion
v4 tokens (`dur-instant 80`, `fast 120`, `base 180`, `slow 240`, `dur-fill 240`, `dur-vortex 2800`) plus `dur-sweep 2200` (`ease-sweep` .45,0,.55,1) and `dur-travel 320` (`ease-emphasized` .2,0,0,1.15). Full table in §4.

## 2. Hero component (pill + stem + line)
Sits in the dock above the composer. Structure, top to bottom: eyebrow "STEP 3 OF 5 · ⌘⇧P steps", step title, % on the right, then the **stage**, then step labels on the same 5-column grid as the segments.
- **Line:** one segment per plan step, separated by 8px gaps with a 12px notch tick (`notch-ink`: dark #9A9AA2, light #6E6E76).
  - **Empty:** matte track with a micro-hex texture.
  - **Done:** polished chrome (top highlight, specular band, dark reflection band, bounce). In light there's no white on done segments.
  - **Current (honest):** not filled. It's **10px tall vs 6px** (shape cue), has a **1.5px ring** (dark #F2F4F7 / light #2E343D), and holds a specular sweep with the brightest core (dark: white band; light: white core between #2E343D bands).
  - **Failed:** danger edge with a hatch texture.
  - **Waiting for approval:** warning ring, with the sweep hidden.
- **Pill:** 40px, chrome-rim border, contains the vortex (24px), the action label, mono elapsed time and a stop hint. It's anchored above the current segment's centre (`--now-x`).
- **Stem:** 2×12 chrome rim gradient joining the pill to the current segment.
- **Shared light:** a single light position (`--now-x`) drives the glow pool at the stem's foot, the pill's lower rim highlight and the sweep's centre. They move together on a step change (`dur-travel`, `ease-emphasized`).
- **Rules:** a segment fills only when its step completes. Light, sweep and glow exist only while running. No text on glow. The label and "N%" are always visible, and percent = done/total.
- **Accessibility:** the pill is `role=status`, `aria-live=polite`, announcing label changes only. The line is `role=progressbar` with `aria-valuenow=done`, `aria-valuemax=total` and `aria-valuetext="Step 3 of 5, 40%"`.

## 3. States (see v5-02)
| Component | default | hover | focus-visible | pressed | current / active | disabled |
|---|---|---|---|---|---|---|
| Ledger row | muted | hover veil | 2px focus ring, offset −2 | press veil | 2px chrome edge bar + primary text + % | 42% opacity |
| Tool-call header | hairline card | veil | ring inset 2px | press veil | open: chevron ▴, body shown | muted + "queued" |
| Primary button | solid chrome tone + 1px spec top | chrome.100 / lighter | ring 2px offset 2 | chrome.300 / chrome.900 + inset shadow | — | 42% |
| Secondary button | hairline | veil | ring | veil | — | 42% |
| Composer | hairline | border-strong | focus-within ring; Send enables only with text | — | streaming: Send→Stop | locked during approval, with the reason shown |
| Hero pill | — | — | stop hint focusable | — | running / waiting (warning, light paused) / done (check, then fades after 2s) / error (danger, R retry) | — |
| Segment | empty | — | — | — | current / done / failed / held | — |
Focus ring: 2px, offset 2px, `focus` token (dark #C9CED6 12.3:1, light #3E4550 9.7:1).

## 4. Transitions
| Trigger | Property | Duration | Easing | Reduced motion |
|---|---|---|---|---|
| Hover enable/disable | bg, border, text, opacity | dur-fast 120 | ease-standard | instant |
| Press | bg, inset shadow | dur-instant 80 | ease-standard | instant |
| Keyboard focus | outline (not animated) | 0 | — | same |
| Composer focus | border, outline | dur-base 180 | ease-standard | instant |
| Tool card open / close | chevron, body height | dur-slow 240 / dur-base 180 | ease-standard / ease-exit | instant, no rotation |
| Step completes | segment fill → chrome | dur-fill 240 | ease-standard | instant fill |
| Pill + stem travel | left (glow follows) | dur-travel 320 | ease-emphasized | jump |
| Current-step sweep | translateX; glow opacity | dur-sweep 2200 loop | ease-sweep | **static, centred under the stem** |
| Vortex spin | rotate loop | dur-vortex 2800 | linear | frozen frame |
| Pill state change | border, icon cross-fade | dur-base 180 | ease-standard | instant swap |
| Step fails | edge → danger, sweep off | dur-fill 240 | ease-exit | instant |

## 5. Reduced motion
OS setting (macOS `accessibilityDisplayShouldReduceMotion`, Windows `UISettings.AnimationsEnabled`, GNOME `enable-animations`) or `prefers-reduced-motion`: no loops. Sweep and glow are static at the segment centre and the vortex holds a frame. Position changes jump. Labels carry all state.

## 6. GPUI notes
- **Line, stem and glow:** one quad with one fragment shader. The segment index comes from x. Use a rounded-rect SDF per segment (the current segment is taller) and a notch stripe. Chrome = a 4–5-stop vertical gradient lookup from tokens plus a specular term `exp(-((y-y0)/w)²)`. Sweep = a band whose x is driven by `t`, masked to the current segment. Glow = `exp(-d²k)` falloff from the stem foot, with no blur passes. Micro-hex is drawn only when the cell is ≥1.5 device px.
- **Pill:** a normal GPUI element with a chrome-rim border (gradient quad). It reads the same `light_x` uniform as the line for its rim highlight. The vortex is ≤64 quads, or a shader quad.
- **Frame policy:** frames only while a step is running **and** the window is focused **and** the element is visible. 60fps on mains, 30fps on battery or thermal pressure, 0fps otherwise (one static frame). Reduced motion keeps `t` constant.
- **Tokens:** generate `theme.rs` (`Hsla` consts per theme) from `tokens.json` + `tokens-v5.json` in build.rs/xtask. Gradients become stop arrays. AccessKit exposes the pill as a live status and the line as a progress indicator.
- **Tauri:** the v5 HTML/CSS is near-production markup. Pause animations on `visibilitychange`.

## 7. Acceptance criteria (v5)
1. Fill is honest: a segment is chrome only after its step completes. The sweep never advances the fill, and % = done/total.
2. Current is identifiable without hue in both themes: 10px vs 6px height, a ring, and the brightest core. Ring or core differs from done by ≥3:1 (dark 3.56, light 5.22).
3. The pill, stem, glow and sweep share one light position and move together within one frame on a step change.
4. All pairs in `contrast-v5.md` pass. The light done/empty and shine/track pairs are ≥3.8:1, and the dark notch is ≥3:1 even under glow.
5. No text sits on a gradient or glow, and buttons use solid chrome tones.
6. With reduced motion there are zero animation frames and a static centred highlight, and the label still conveys state.
7. Unfocused or occluded window → 0fps. Running and focused → ≤60fps, with the hero adding under 1ms GPU per frame at 1440×900.
8. Every focusable element shows the 2px ring. Tab order runs ledger → conversation → hero stop control → composer.
9. Components reference only semantic tokens; there's no raw hex outside the token files.

---

# Appendix A: v1 handoff spec (SUPERSEDED where it conflicts with v5 above)
Screens: `screens/*.png` (1440×900, dark + light; 01 and 02 show the plan; 08 shows the accept-all undo toast). Source HTML: `html/`. Tokens: `tokens.json` (W3C DTCG), `tokens.css`. Generators: `gen.py` (tokens + contrast), `screens.py` (renders).

**Rule:** components reference only **semantic** tokens (`bg-surface`, `text-muted`, `accent-bg`…), never primitives.

## Layout
| Region | Spec |
|---|---|
| Titlebar | height `size.titlebar` 40, bg-surface, border-subtle bottom. Holds session title, model chip, ctx% + cost, pane toggles |
| Sidebar | `size.sidebar` 248, collapsible ⌘B, rows `size.row` 28, groups Today/Yesterday/7 days |
| Conversation | max-width 760, space-8 top padding, gap space-4 |
| Composer | 760 wide, radius-lg, border-strong, bg-elevated, min 44px text area, grows to 40% of height |
| Right pane | 560–720 resizable (default 620), tabs Terminal / Changes / (later) Search. Auto-collapses below 1200px window width |
| Min window | 960×600 |

## Components & states
| Component | States |
|---|---|
| Session row | default, hover (bg-hover), selected, focus ring, running (accent dot), error (danger dot + tooltip), long title ellipsis |
| Message (user/assistant) | streaming (caret, aria-live polite per paragraph), complete, stopped ("Stopped · Resume"), error (danger card + Retry R) |
| Thinking | collapsed "Thought for Ns" (default), expanded, live ("Thinking…" shimmer, static under reduced motion) |
| Tool-call card | queued, running (elapsed timer, no spinner <300ms), success ✓, failed ✕ + stderr, denied, grouped (3+ consecutive calls become "5 tool calls ▸"), output truncated at 12 lines + "Show all", copy |
| Approval card | pending (warning border, focus on Approve), approved, denied, auto-allowed (rule badge), expired. Always shows the exact command or diff, sandbox mode, and network state |
| Composer | empty, typing, @-mention popover, / command popover, streaming (Send becomes Stop/Esc), disabled-offline (queues), attachment chips |
| Model menu | connected, current ✓, not connected (warning sub-label, ↵ sign in), loading models (skeleton rows), error |
| Diff | unified/split, file tree with +/− counts, hunk accept/reject/accepted/rejected, conflict, binary file, large file collapsed |
| Terminal | focused, unfocused (dim cursor), agent read-only tab, exited (code badge), scrollback search ⌘F |
| Command palette | empty query (recents), results grouped, no results (create session CTA), prefix modes > @ # |
| Plan panel | Pinned above the composer while the agent runs, also mirrored as a right-pane "Plan" tab. Expanded or collapsed (⌘⇧P, remembered per session). Header shows "N of M · step K in progress". Step states: pending ○ (text-secondary), in progress ● (accent-fg, medium weight, elapsed time), done ✓ (success icon, muted text), failed ✕ (danger + "Retry step"), skipped ⊘, needs approval ⚠ (links to the approval card). The agent may revise the plan; a revised step briefly shows "updated". aria: list with aria-current="step"; changes announced politely |
| Accept-all undo toast | After Accept all (⌘↵), a checkpoint is created and a toast appears bottom-right above the pane footer: "✓ Accepted N files · M hunks [Undo ⌘Z] ✕" with a 10s progress bar (paused on hover or focus; no bar under reduced motion). ⌘Z or Undo reverts to the checkpoint and the diff returns to review. After the toast closes, "Restore checkpoint #n" stays in the turn menu and in ⌘K. States: shown, hover-paused, undoing (inline progress), undone ("Changes restored"), failed (danger: "Couldn't restore: file modified outside rozu. Open diff") |
| Toast | info, undo (10s), error |

## Keyboard map
| Shortcut | Action |
|---|---|
| ⌘K | Command palette |
| ⌘N | New session |
| ⌘/ | Model/provider menu (1–9 select) |
| ↵ / ⇧↵ | Send / newline |
| Esc | Stop streaming · close overlay (focus returns to trigger) |
| Y / N / A / E | Approve / Deny / Always allow / Edit (when approval focused; ⌘⇧Y jumps to pending approval) |
| ⌘J | Toggle terminal pane · Ctrl+` focus composer ↔ terminal |
| ⌘⇧P | Expand/collapse plan |
| ⌘Z (after Accept all) | Undo accept-all → restore checkpoint |
| ⌘D | Toggle Changes pane · H / X accept/reject hunk · ]c [c next/prev hunk · ⌘↵ accept all |
| ⌘B · ⌘\ | Toggle sidebar · right pane |
| ⌘⇧S | Sessions list (/ filter, R rename, ⌘⌫ archive, ⌘D duplicate) |
| ⌘1…9 | Jump to session N |
| ⌘, | Settings |
Linux/Windows: ⌘ maps to Ctrl.

Focus order: sidebar → conversation → composer → right pane. F6 cycles regions.

## Motion
| Trigger | Property | Duration | Easing |
|---|---|---|---|
| Overlay open/close | opacity + 4px translateY | base 180 / fast 120 | standard / exit |
| Pane toggle | width | slow 240 | standard |
| Hover/press | background | instant 80 | standard |
| Token streaming | none (text appends, no fades) | | |
| Tool card expand | height | fast 120 | standard |
With reduced motion, all durations are 0. The caret blink is replaced by a solid caret.

## Contrast (WCAG 2.2 AA): all 34 pairs pass
Accent: **dried-rose brand palette** taken from `assets/brand/dried-rose` + `favicon.svg`/`styles.css`. The palette is ivory #F1F1F1, warm taupe #C0B8B0 and graphite #292725/#0D0D0D. Ramp `color.rose.100…950`: 100 #F1F1F1 · 300 #C0B8B0 · 500 #8A8178 · 600 #6B6259 (derived darker taupe for light-theme text) · 800 #292725 · 950 #0D0D0D.
- Dark: accent-fg = rose.300, primary button = ivory (rose.100) with graphite text, focus = rose.300.
- Light: accent-fg = rose.600 (#C0B8B0 on white is only 1.9:1, so it can't be used as text), primary button = graphite (rose.800) with ivory text, focus = rose.800.

| Theme | Foreground | Background | Ratio | Req | Pass |
|---|---|---|---|---|---|
| dark | text/primary | bg/app | 16.77:1 | 4.5:1 | ✅ |
| dark | text/primary | bg/elevated | 14.67:1 | 4.5:1 | ✅ |
| dark | text/secondary | bg/surface | 11.75:1 | 4.5:1 | ✅ |
| dark | text/muted | bg/app | 6.95:1 | 4.5:1 | ✅ |
| dark | text/muted | bg/elevated | 6.09:1 | 4.5:1 | ✅ |
| dark | text/muted | bg/hover | 5.40:1 | 4.5:1 | ✅ |
| dark | accent/fg | bg/app | 9.92:1 | 4.5:1 | ✅ |
| dark | accent/fg | bg/elevated | 8.68:1 | 4.5:1 | ✅ |
| dark | accent/fg | bg/surface | 9.31:1 | 4.5:1 | ✅ |
| dark | accent/bg | bg/elevated | 15.05:1 | 3:1 | ✅ |
| dark | focus | bg/surface | 9.31:1 | 3:1 | ✅ |
| dark | accent/on | accent/bg | 17.21:1 | 4.5:1 | ✅ |
| dark | success | bg/elevated | 9.76:1 | 4.5:1 | ✅ |
| dark | danger | bg/elevated | 6.15:1 | 4.5:1 | ✅ |
| dark | warning | bg/elevated | 10.18:1 | 4.5:1 | ✅ |
| dark | focus | bg/app | 9.92:1 | 3:1 | ✅ |
| dark | border/strong | bg/app | 3.84:1 | 3:1 | ✅ |
| light | text/primary | bg/app | 19.43:1 | 4.5:1 | ✅ |
| light | text/primary | bg/elevated | 19.43:1 | 4.5:1 | ✅ |
| light | text/secondary | bg/surface | 10.55:1 | 4.5:1 | ✅ |
| light | text/muted | bg/app | 7.39:1 | 4.5:1 | ✅ |
| light | text/muted | bg/elevated | 7.39:1 | 4.5:1 | ✅ |
| light | text/muted | bg/hover | 6.38:1 | 4.5:1 | ✅ |
| light | accent/fg | bg/app | 5.97:1 | 4.5:1 | ✅ |
| light | accent/fg | bg/elevated | 5.97:1 | 4.5:1 | ✅ |
| light | accent/fg | bg/surface | 5.58:1 | 4.5:1 | ✅ |
| light | accent/bg | bg/elevated | 14.88:1 | 3:1 | ✅ |
| light | focus | bg/surface | 13.90:1 | 3:1 | ✅ |
| light | accent/on | accent/bg | 13.17:1 | 4.5:1 | ✅ |
| light | success | bg/elevated | 5.02:1 | 4.5:1 | ✅ |
| light | danger | bg/elevated | 6.47:1 | 4.5:1 | ✅ |
| light | warning | bg/elevated | 4.92:1 | 4.5:1 | ✅ |
| light | focus | bg/app | 14.88:1 | 3:1 | ✅ |
| light | border/strong | bg/app | 5.05:1 | 3:1 | ✅ |

Failures: 0

Earlier fixes still apply: `border-strong` = gray.500 in both themes. Because the accent is near-neutral, accent is never the only signal. Selected and running states also use a dot, weight, ✓ ● ○ ✕ glyphs, or a focus outline. Diff lines use +/− gutters.

## Accessibility
Targets are ≥24px. Focus ring is 2px `focus` with a 2px offset. Approvals and errors are announced assertively. Streaming is announced politely, per paragraph rather than per token. Text scales to 200% (layout uses rem/em, and GPUI should scale through rem_size). Use the OS font-scale setting.

## Rust implementation notes
- **Tokens pipeline:** use `tokens.json` as the source of truth. A build.rs (or a small `xtask`) generates `tokens.css` for Tauri and `theme.rs` for GPUI: `pub struct Theme { bg_app: Hsla, … }` with `DARK` and `LIGHT` consts. Spacing and radii become `Pixels`, durations become `Duration`, easing becomes a cubic-bezier fn.
- **GPUI (preferred):** store `Theme` as a `Global` and switch it on the OS appearance change (`window.appearance()`). Use `uniform_list` for the session list and terminal grid, and virtualize the conversation. Text runs come from `TextSystem` with the mono font for code. Wire AccessKit early, because GPUI accessibility is still limited, so treat it as a risk.
- **Tauri 2:** the HTML in `html/` is close to production markup. Load `tokens.css` and switch themes with `data-theme` from `prefers-color-scheme`. Use xterm.js with a Rust PTY (portable-pty) over a Channel. Use a native title bar overlay on macOS.
- **Streaming:** coalesce tokens to one repaint per frame. Don't auto-scroll when the user has scrolled up more than 80px; show "Jump to latest" instead.
- **Providers:** the switcher reads a `Provider` trait registry (OpenAI/ChatGPT OAuth, Anthropic key, xAI key). Each model carries ctx size, tool support and price, and those drive the sub-labels.

## Acceptance criteria (excerpt)
1. Every screen element maps to a token. A grep for hex values in the components finds none.
2. Esc during streaming stops within 100 ms and keeps the partial text.
3. With an approval pending, pressing Y runs the command and N sends the denial to the agent. Neither needs the mouse.
4. ⌘/ followed by a digit switches the model, and the next turn's request uses that provider.
5. A resize storm (100 resize events per second) leaves the terminal unbroken.
7. The plan always reflects the current step within 1 frame of a tool call starting. A failed step shows ✕ and a Retry.
8. After Accept all, ⌘Z within 10s restores every file byte-for-byte to the checkpoint, and the diff re-opens.
6. Both themes pass every contrast pair above, verified by `python3 gen.py`.
