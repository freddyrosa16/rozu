# rozu v7.2 motion spec: lit flow orbs with signature cues
Builds on v7.1 (one spherical cloud of 120 dots, 60 at 22px, with radial flow). Source: `html/v7.2-orb.js`. Fred approved three changes:

## 1. One bold signature cue per state (reads at 22px and in the still frame)
| State | Cue | Math | Tempo |
|---|---|---|---|
| Thinking | bright dense core | r = 1 − .88·φ^.55 (dots rush in, then linger); r<.42 → lit, 1.3×; the rest α×.4, .6× | 1.6s loop |
| Searching | off-centre hot spot (upper right) | p = lerp(d, f, .96·φ^.6), f ≈ (.78, .62, .1) drifting ±.05 on a 2.4s circle; converged dots lit, 1.3×; the rest α×.3 | 2.4s, slow drift |
| Reading | tilted lit band | in view space, v = .75y − .66x; dots with abs(v − c) < .17 are lit (1.2×); c = .25·sin(2πt/4.4); the rest α×.55 | 4.4s band sweep, 2.2s inflow |
| Notes | hollow centre, bright rim | shell r = .78 → 1.0 (slow outward); fresnel rim = 1 − abs(z)/abs(p) > .55 → lit, 1.2×; face-on dots α×.3 | 3.6s, slowest |
| Editing | tight compressed ball with a gap | r ≈ .66·(1 − .22·pulse); a ±.75 rad screen-space wedge on the right is empty (a caret notch); click every 530ms | 530ms clicks |
| Running | 2 spiral arms | arms = cos(2·lon(d)) > .55; twist rY(2.6φ + 2.5t) grows with inflow, so the arms wind into spirals; arm dots lit, 1.35×; the rest α×.18 | 0.8s, fastest |
| Waiting | amber, flow frozen, breathing | unchanged from v7.1 | 2.6s breath |
| Done | green, still shell, one ripple | unchanged | once (1.1s ripple) |
| Error | red, stall then scatter | unchanged | once (.18s stall + .5s scatter) |
| Offline | dim sparse third, sputter | unchanged | .3s ticks, 2 moving / 2 stalled |

Verified with `screens/v7.2-22px-compare.png`: all ten 22px pill orbs side by side at 8× nearest-neighbour, dark (top) and light (bottom), from the 1x sheets. Iteration log: (a) Searching read like Thinking, so the hot spot moved from (.42, .3) to (.78, .62) and its stream dots dimmed; (b) Editing's gap was invisible, so the wedge went from ±.42 to ±.75 rad and the ball r from .55 to .66; (c) Running's arms were mush, so the non-arm dots dropped to α×.18 and .45×.

## 2. Depth and light
- Near-to-far: size factor .4 + .6·depth (was .55/.45), α .12 + .88·depth (was .22/.78). depth = clamp((z+1.2)/2.4).
- Light: λ = ½ + ½·dot(p̂, L̂), L = (−.55, .6, .58) (top-left, toward the viewer). α ×= .62 + .48λ; size ×= .9 + .2λ. Dots stay flat discs (no per-dot gradient), so they stay crisp.
- Per-dot α multiplier `a` (state-defined), applied after the lit boost. 22px dots ≤1.6 device px are still pixel-snapped squares.

## 3. Tempo and change pulse
Tempos (above) are all different, from .53s clicks to 4.4s bands. On every state change, all dots get lit += .8·sin(π·t/.15) for the first 150ms (it rides on top of the 600ms morph). Morph: a critically damped spring (ω=7) with a 2.5ms/pt stagger, a +.55·sin(πq) z lift, and `a` interpolated across the morph. 60fps.

## Reduced motion
A static frame per state (times in `gen_v7_2.py` FR), with a 150ms cross-fade instead of the morph. The change pulse becomes a single 150ms α bump (no motion). The signature cue is present in every still frame.

## GPUI notes
One instanced quad draw, ≤120 instances (under 200), instance = {pos2, radius, alpha, color idx}. The CPU does gen + light + projection + insertion sort per frame. The fragment shader draws an SDF disc with a 1-device-px smoothstep. Frame policy: 60fps visible and focused, 30fps on battery, 0fps when occluded or unfocused, or after Done/Error settle (1.3s). Labels are GPUI AA text, never drawn into the orb layer.
