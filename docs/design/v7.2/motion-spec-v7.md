# rozu v7: 3D dot-orb activity indicators (DRAFT, from scratch; awaiting Fred's approval; handoff.md is untouched)
**Brief:** in the spirit of MetalForge "thinking-orbs". I couldn't see its visuals, and only the 38 style names in `inspo/metalforge/styles.txt` were available. The names were used only as loose prompts (Gyro, Ribbon, Coil, Converge, Ratchet, Chase, Rock, Burst, Crystal, Drain). All the math and the look are rozu's own; no MetalForge shader was obtained or replicated.

**No progress bar.** Step progress is text in the pill ("0:08 · 3/5").

## 1. System
- **One point cloud:** **N = 120 points** (≤200). Each state is a function `gen(i, t) → {p: [x,y,z], lit ∈ [0,1], s}` in a unit-radius object space, including the state's own 3D rotation.
- **Camera:** perspective, distance F = 3.2. Projection `k = F / (F − z)`, `x' = c + x·k·R`, `y' = c − y·k·R`, where R = 0.4·size.
- **Depth:** `d = clamp((z + 1.2) / 2.4)`.
  - Radius `= base · s · (0.55 + 0.45·d) · (1 + 0.55·lit)`, with base = 0.017·size at ≥30px and 0.05·size at 22px.
  - Opacity `α = 0.22 + 0.78·d + 0.5·lit` (clamped). Offline is additionally ×0.85.
- **Depth sort:** painter's order by z every frame.
- **Colour:** dots are monochrome `--orb-dot` (dark #F4F1EA, light #25242A). Waiting, Done and Error use `--warning`, `--success` and `--danger`.
- **22px optical size:** 60 points (subsample keeps i where `((i >> 1) + i) mod 2 = 0`, so both parities survive) with larger dots.
- **Crispness:** dots ≤1.6 device px are drawn as **pixel-snapped squares**. Larger dots are AA discs with solid fill. No blur, glow, gradient or bloom.
- **Pill:** #1B1B1D / #ECECEF, 1px border #2A2A2E / #DCDCE0. Contents: the 22px orb, the label (`--orb-label`, 500), and mono tabular meta (timer · step count, or a hint). The label cross-fades over 120ms on a state change. `role=status`, `aria-live=polite`; the orb is `aria-hidden`.

## 2. States (math · timing)
| State | Name | Math | Loop | Reduced-motion frame |
|---|---|---|---|---|
| Thinking… | **Gyro** | 3 rings × 40 points, radius 0.86. Ring k is rotated `rX(1.05k + t(0.55 + 0.25k))`, then `rY(2.1k + t(0.35 + 0.15k))`, then tilted `rX(0.35)`. Lit bead per ring: `cos(a − t(2.2 + 0.4k) − 2k)^10`. | 4.8s (incommensurate, never repeats exactly) | t = 1.3s |
| Jotting down notes… | **Ribbon** | 3 lines × 40 points (lengths 1 / 0.7 / 0.85) in a sheet: `y = 0.5 − 0.5·row + 0.05 sin(14u + 2row + 2t)`, `z = 0.18 sin(4u + row)`, then `rX(−0.55)`, `rY(0.45 sin 0.7t)`. Written progressively (3.3 rows per loop); unwritten points have s = 0; the pen point is lit at 1.5×. | 2.7s | t = 1.6s (2 lines + part) |
| Reading `<file>` | **Coil** | Helix: `a = 3.5·2π·u + 1.4t`, radius 0.52, `y = 0.9 − 1.8u`, then `rX(0.42)`, `rZ(−0.2)`. Scan band `lit = exp(−((y − c)/0.17)²)` with c sweeping top → bottom. | 1.8s | Band at 60% |
| Searching | **Converge** | Fibonacci sphere directions. Each dot has phase `φ = (t/1.6 + hash(i)) mod 1` and radius `1 − 0.86φ²`, streaming inward and respawning on the shell. Lit when φ > 0.72; size 0.7 → 1.2. Slow yaw 0.5 rad/s. | 1.6s | t = 0.7s |
| Editing `<file>` | **Ratchet** | 84-point ring (radius 0.8, every 7th point a tooth at 0.98, lit). It rotates in **sprung steps** of 30° every 0.4s (elastic, ζ ≈ 0.55), tilted `rX(−1.05)`. A 36-point vertical caret at z = 0.6 blinks with a hard cut every 530ms. | 0.4s step / 1.06s blink | Caret on |
| Running `<cmd>` | **Chase** | (2,3) torus knot: `r = 0.52 + 0.24 cos 3φ`, `(r cos 2φ, r sin 2φ, 0.3 sin 3φ)·1.2`, yaw 0.55t, tilt 0.55. **Three lit heads** a third apart chase along it, each with a 7% linear tail. | 1.2s | Heads at 0 / ⅓ / ⅔ |
| Waiting for approval | **Rock** (amber) | Two dotted 3D columns (a pause sign): 6-point rings × 10 levels, radius 0.1, at x = ±0.42. They **rock** about Y (±0.5 rad, 5.2s) while a swell `sin(2πt/2.6 − 2.5y)` rises through them in brightness. Calm, no translation. | 2.6s / 5.2s | Columns static |
| Done | **Burst** (green) | The core bursts to a Fibonacci sphere (radius 0.86) on the elastic spring `sprO(t/0.55)`, **one overshoot**, then turns gently (yaw 0.4 rad/s). All dots lit. | once (settles ~0.9s) | Full sphere |
| Error | **Crystal ✕** (red) | Two dotted bars × 60 points along (±u, ±u)·0.78 with a fine 3D twist (radius 0.035, z 0.05), turned `rY(0.5)`. **One damped 3-cycle shake** (±0.08, 360ms), then still. | once | ✕ still |
| Offline / retrying | **Drain** (dim mono) | A 75% broken orbit (radius 0.82, a gentle z wave), `rX(1.0)`, `rZ(0.3)`, turning once per 2.4s. A dim lit pulse travels along it every 1.2s and the last dots swell as it **tries the gap**. Opacity ×0.85; the label counts down. | 2.4s / 1.2s | Arc static |

All ten silhouettes are distinct: tangled rings, wavy lines, spring, starburst cloud, cog + caret, knot, twin columns, sphere, ✕, open arc.

## 3. Morph
- **Same index set:** on a state change, point i interpolates from the **previous state's live position** (it keeps animating, so nothing restarts) to the new state's live position.
- **Timing:** critically damped spring, `q = 1 − (1 + 7u)e^(−7u)` with `u = (t_in − i·2.5ms) / 600ms`. The index stagger is ≤300ms.
- **Lift:** mid-morph, points lift toward the camera by `0.55·sin(πq)`, so the cloud "breathes out" and in: a 3D arc, with growth from depth.
- **Interpolated:** lit and s interpolate. Tone swaps at q = 0.5 (e.g. monochrome → amber).
- **Interrupts:** these use the current blended position.

## 4. Reduced motion
- No loops and no morph travel. Each state shows its static frame (table above).
- A state change is a **120ms cross-fade** between frames.
- The label carries the state.

## 5. GPUI notes
- **One instanced draw:** 120 instances (60 at 22px) of a single quad. Per instance: `pos (vec2, projected), radius, alpha, tone_idx`.
- **CPU per frame:** `gen` + rotation + projection + morph for 120 points is ≈ 120 × ~60 flops (negligible), plus a 120-element **depth sort** (insertion sort, as the order barely changes frame to frame).
- **Fragment shader:** a disc SDF, `alpha · smoothstep(r + 0.5/scale, r − 0.5/scale, d)`. Tiny dots (≤1.6 device px) take a branch that fills the snapped pixel square exactly. A single flat colour per instance: no gradient, no glow.
- **Budget:** under 0.1ms CPU and under 0.05ms GPU per frame.
- **Frame policy:**
  - 60fps while animating, the window is focused and the pill is visible; 30fps on battery.
  - 0fps when unfocused or occluded, or after Done/Error settle.
  - Resume from the current T with no jump. Reduced motion → no frames apart from the cross-fade.
- **Alternatives:**
  - WebGL/wgpu: the same instancing, or a single-quad shader looping over a ≤200-point uniform array.
  - Tauri: `<canvas>` 2D as in `html/v7-orb.js`, paused on `visibilitychange`.

## 6. Draft tokens (`tokens-v7.css`)
- `--orb-dot`, `--orb-pill-bg`, `--orb-pill-border`, `--orb-label`, `--orb-meta`
- Light status overrides: amber #8A5206, green #126B33
- `--orb-points` 120 / `--orb-points-22` 60, `--orb-camera` 3.2
- `--dur-orb-morph` 600ms, `--orb-stagger` 2.5ms
- `--size-orb` 22px / `--size-orb-lg` 88px (cards use 96px)

## 7. Renders
- `screens/v7-01-activity-states-{dark,light}{,@2x}.png`
- `screens/v7-02-closeup-{dark,light}{,@2x}.png`: 4× (384px) Gyro, Chase, Burst.
- `screens/v7-activity-morph-{dark,light}.mp4`: 60fps, 14.6s, 1920×1080 (deviceScaleFactor 2), deterministic T = frame/60. All ten states with live morphs.
