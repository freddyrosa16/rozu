# rozu v7.4 motion spec: one dotted ring, ten symbols

## Why
Fred's read of v7.3: the busy states looked alike, the ring wasn't a full dotted circle, Offline needed work, and Done and Error were right. So v7.4 keeps Done and Error, and gives every state the frame they use: a fully dotted ring that never moves, with a symbol inside it. The symbol says what the agent is doing, and it reads at 22px because it's a symbol, not a cloud.

## Shared
- 120 dots, perspective `k = 3.2/(3.2−z)`, depth size and α, the top-left light, the lit boost, the depth sort, the 600ms morph, the 150ms pulse and the reduced-motion cross-fade are unchanged from v7.3.
- **Ring:** dots `0..44`, radius .86 on the plane z = .3, clockwise from 12 o'clock. At rest `lit 0`, `s .85`, `a .55`; a lit ring dot is `s .85 + .4·lit`, `a max(.55, lit)`. The ring dots are at the same positions in every state, so a morph only moves the symbol.
- **Symbol:** dots `44..120` (76), each at arc-length fraction `u = (i − 44)/75` along the symbol's path (`along(strokes, u)`), on the plane z = .3. Lit symbol dots are `lit 1`, `s 1.0–1.15`, `a 1`; not-yet-drawn dots are `lit 0`, `s .45`, `a .16`.
- Both runs are contiguous, so the 22px subsample `((i>>1)+i)%2==0` keeps 22 ring dots and 38 symbol dots.

## States
| State | Symbol path | Motion | Tempo | Still T |
|---|---|---|---|---|
| Thinking | Fibonacci sphere of 76 dots, r .46, `rotX(rotY(p, 1.1t), .45)`; lit = clamp(.8·z/.46) × .7 | Ring sweep: head at 2πt/1.6 clockwise, tail 2.2 rad, lit = (1 − lag/2.2)² | 1.6s | .55s |
| Notes | Three lines y = .26, 0, −.26 from x −.44 to .44 (the last to .16) | w = clamp((t mod 2.6)/1.9); dots with u ≤ w are drawn; the head dot (u within .04 of w) is ×1.5 | 2.6s | 1.2s |
| Reading | Four lines y = .3, .1, −.1, −.3 (the last to x .2) | Band yb = .42 − .84·((t mod 2.2)/2.2); lit = exp(−((y − yb)/.09)²) | 2.2s | .6s |
| Searching | Lens: circle r .24 round c (first 74% of dots); handle from c + (.17, −.17) to c + (.4, −.4) | c = (−.08, .08) + .12·(cos a, sin a), a = 2πt/2.4 | 2.4s | .3s |
| Editing | Caret: first 14% of dots, x + .05, y −.24…26; line at y −.2 from x −.46 to .46 | w = clamp((t mod 2.4)/1.6), caret x = −.46 + .92w, line dots drawn while x ≤ caret; once w = 1 the caret blinks off on odd 200ms ticks (`s 0`) | 2.4s | 1.1s |
| Running | Play triangle (−.2, .34) → (−.2, −.34) → (.36, 0) → back | Two ring heads at 2πt/.9 and +π, tails 1.4 rad | .9s | .3s |
| Waiting (amber) | Pause bars x = ±.17, y ±.32 | b = .5 + .5·sin(2πt/1.8); ring lit .25 + .55b; bars `s 1.05 + .15b` | 1.8s | .45s |
| Done (green) | Check (−.46, .02) → (−.14, −.32) → (.48, .34) | Dots with u ≤ clamp((t − .25)/.45) drawn; pop `s × (1 + .2·sin(π(t − .7)/.25))` for t in (.7, .95); static from .95s | once | 1.5s |
| Error (red) | Cross: two strokes (±.36, ±.36) | Whole frame shifts dx = .13·e^(−5s)·sin(28s), s = t − .3, for s in (0, .9); static from 1.2s | once | 1.5s |
| Offline (dim) | Ring dots within .55 rad of 12 o'clock have `s 0` (the gap); arrow: arc r .4 over 1.6π from angle rot + π/2 (80% of dots) plus two barbs of .24 back from the tip at ±.8 rad | rot = −2πt/2.4 | 2.4s | .3s |

## 22px check
`screens/v7.4-22px-{dark,light}.png`: each state at t = 0, .3, .6, .9, 1.2, 1.5s, at 96px (left) and at 22px, 2× device px, upscaled 3× nearest-neighbour (right).
