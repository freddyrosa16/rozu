# rozu v7.3 motion spec: six states matched to the liked four
Fred liked **Thinking, Reading, Editing and Running** (unchanged from v7.2, so they're the reference). The other six are redesigned to match them: the same 120-dot cloud, and one bold lit cue (α 1, 1.25–1.4×) against faded context dots (α×.18–.30, .45–.55×), with the same depth and light, comparable motion energy, and their own tempo. Source: `html/v7.3-orb.js` (helper `C(p, on, sizeOn, sizeOff, alphaOff)`).

**JS fix (v7.3):** in v7.2 the projection overwrote the per-state dot size `s` with the perspective factor, so the state size multipliers (gaps, sparse dots, 1.3× cues) never rendered. Perspective is now `k`, and the radius is `base · s · k · …`. The v7.3 renders show the true spec, including Editing's real gap.

| State | Cue (22px + still frame) | Math | Tempo | Still T |
|---|---|---|---|---|
| Searching | SCAN LENS: a lit cap orbits the rim | cap axis ĉ(t) = norm(.78cos a, .5sin a, .45), a = 2πt/2.4; dots with p̂·ĉ > .86 are lit and pushed out ×1.06; the rest flow in (r = 1−.5φ²) faded | 2.4s orbit | .5s |
| Notes | INK STROKE: a lit comet writes round a wavy latitude | band abs(y − .45 sin lon) < .26; lit where the lag behind the head (2πt/1.5) is < 4.4 rad, with the tail fading in size and α; the cloud drifts out (r .7→1) faded | 1.5s stroke, 3.0s drift | 1.1s |
| Waiting (amber) | PAUSE BARS: two lit vertical bands on a frozen cloud | abs(abs(x) − .32) < .12 and abs(y) < .7, lit .7–1 with the breath; scale 1 + .06 sin(2πt/2.6); the flow is frozen | 2.6s breath | .65s |
| Done (green) | HALO: one ripple, then a bright still rim | shell r .84 plus a pole-to-pole ripple (1.1s); silhouette dots (1 − abs(ẑ) > .6) are lit; the interior is faded | once (1.1s) then still | 1.6s |
| Error (red) | BREAK: a lit third is flung out once; the core stays dim | flow stalls .18s; 38% of the dots are flung to r₀ + .6–.75 (spring ζ≈.55, 0.5s) and lit; the rest shrink 10% and fade | once | 1.0s |
| Offline (dim) | BROKEN RING: a lit vertical ring with a gap; a spark sputters | ring abs(x) < .15, gap at y ∈ (−.05, .5); the spark fills the gap on tick 2 of every 4 (.3s ticks); non-ring dots: only a third are shown, very faint; α×.85 dim | .3s ticks | .35s |

22px check: `screens/v7.3-22px-compare.png` (8× nearest-neighbour, dark top, light bottom). Iterations: Searching's cap axis wasn't normalized, so nothing lit; it's fixed. Notes went from a short blob to a long thin stroke. Offline's ring went from horizontal to vertical, so it no longer reads like Notes' stroke.
Morphs, the 150ms pulse, reduced motion, light/depth and the GPUI notes are unchanged from motion-spec-v7.2.md.
