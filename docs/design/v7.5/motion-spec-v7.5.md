# rozu v7.5 motion spec: a dotted globe per state

## Why
Fred pointed at libraries.dev/orbs ("make the orbs like this but don't copy"). What that family gets right: one calm object, a sphere of small matte dots on lines of latitude, far side dim, with the state carried by what the sphere does rather than by a symbol; and a separately tuned point set for the inline size instead of a scaled one. v7.5 takes that frame and gives it rozu's own ten behaviours, rozu's projection, light and morph, and keeps the v7.4 check and cross that Fred liked.

## Shared
- Perspective `k = 3.2/(3.2−z)`, depth size and α, the top-left light, the lit boost, the depth sort, the 600ms morph, the 150ms pulse and the reduced-motion cross-fade are unchanged from v7.3/v7.4.
- **Globe:** radius R = .9, tilted .3 rad toward the camera. Ring j of `rings` sits at latitude φ = −π/2 + π(j+.5)/rings and holds n = max(3, round(lon·cos φ)) dots at longitudes 2πk/n. `grid[i] = (φ, λ, j)`; `dir(i) = (cos φ cos λ, sin φ, cos φ sin λ)`; `globe(i, yaw, r) = rotX(rotY(r·dir(i), yaw), .3)`; `front(i, yaw)` = the dot's angle from the camera axis after the spin, 0 at the front, ±π at the back; `roundFrom(i)` = the fraction 0..1 round a ring from the front (writing order); `geo(i, φ0, λ0)` = the geodesic angle to a surface point.
- **Sizes:** Large (≥30px) rings 15, lon 30, N 284, dot ×1, speed ×1. Small (22px) rings 9, lon 15, N 86, dot ×1.15, speed ×1.15. Every `s` below is multiplied by the size's dot factor; every spin by its speed factor.
- **Morph stagger:** dot i starts hash(i)·300ms after the change (a dissolve across the globe rather than a sweep), so both sizes finish together (MORPH_END = 1.26s).
- **After Waiting:** Done and Error don't morph. With `breath(i, b)` = Waiting's globe at breath b (radius R(.96 + .06b), lit .2 + .5b, s .9 + .25b, α .6 + .4b), the segment draws `breath(i, b + (1 − b)·e)` where b is Waiting's breath on Waiting's own clock and e = crit(1.6·tin/1.3), so it fills to b = 1 by the 1.3s settle; tone flips at once, with the 150ms pulse. Reduced motion: the still frame is `breath(i, 1)`. From any other state Done draws the check and Error the cross as before.
- **Painter:** base radius .011·size (.045·size at 22px); the rest as v7.4.

## States
| State | Position | Lit / size / α | Tempo | Still T |
|---|---|---|---|---|
| Thinking | `globe(i, .35t)` | Two sparks k = 0, 1 with period T = 1.6s, offset T/2: age a = (t + kT/2) mod T, cycle c; origin φ0 = .6(2·hash2(c, k+1) − 1), λ0 = yaw + π/2 + 1.1(2·hash2(k+1, c) − 1) (on the front); d = geo(i, φ0, λ0); ripple = exp(−((d − 1.1a)/.3)²)·(1 − a/T)^1.5; spark = max(0, 1 − a/.4) for d < .25; lit = max over both; s .85 + .45·lit, α .45 + .55·lit | 1.6s | .55s |
| Reading | `globe(i, .25t)` | yb = π/2 − 1.15π·((t mod 2.2)/2.2) + .07; on = exp(−((φ − yb)/.17)²); lit on, s .8 + .5·on, α .35 + .65·on | 2.2s | .6s |
| Searching | `globe(i, .3t)` | a = 2πt/2.4; lens centre φc = .35·sin(.7a), λc = .9·sin a (in front-relative longitude); d = hypot(φ − φc, (front(i) − λc)·cos φc); lit = clamp((.42 − d)/.12); s .85 + .55·lit, α .4 + .6·lit | 2.4s | .3s |
| Editing | `globe(i, 0)` | Writing order o = (j + roundFrom(i))/rings, from the bottom ring up. ph = t mod 2.6, w = min(1, ph/2). Written (o < w): fresh = max(0, 1 − (w − o)·rings/1.5); lit .75 + .25·fresh, s 1.15 + .5·fresh, α .9 + .1·fresh; the newest dot on its ring ((w − o)·rings·ringLen(j) < 1, while ph < 2) is the caret: lit 1, s 1.7. Unwritten: lit 0, s .75, α .3. Held full from 2s, then from 2.3s everything fades together over .3s: q = (ph − 2.3)/.3, lit .75(1 − q), s 1.15 − .4q, α .9 − .6q | 2.6s | 1.1s |
| Running | `globe(i, 2.4t)` | s = .5 + .5·sin(2φ + λ − 2πt/.9); lit = clamp(1.4s − .4); lit×.75, s .9 + .25·lit, α .5 + .5·lit | .9s | .3s |
| Waiting (amber) | `globe(i, 0, R·(.96 + .06b))`, b = .5 + .5·sin(2πt/1.8) | lit .2 + .5b, s .9 + .25b, α .6 + .4b | 1.8s | .45s |
| Done (green) | `breath(i, clamp(t/.5))` | Fills from the small dim globe to the full bright one over .5s; pop s × (1 + .15·sin(π(t − .5)/.3)) for t in (.5, .8); then holds | once | 1.5s |
| Error (red) | `breath(i, 1)` | The full globe; whole frame shifts dx = .13·e^(−5s)·sin(28s), s = t − .3, for s in (0, .9); static from 1.2s | once | 1.5s |
| Offline (dim) | `globe(i, .12t, .92R)` | slot = floor(t/.4); a dot is off (s 0, α 0) when hash2(j, slot) < .42 or hash2(i, slot) < .12; otherwise lit 0, s .85, α .45 | 2.4s | .3s |

`vnoise` is smooth value noise on an integer lattice (`hash2` corners, smoothstep blend); `hash2(a, b) = frac(sin(12.9898a + 78.233b)·43758.5453)`.

## 22px check
`screens/v7.5-strip-{dark,light}.png`: each state at t = 0, .3, .6, .9, 1.2, 1.5, 2, 2.6s, at 96px (left) and at 22px, 2× device px (right).
