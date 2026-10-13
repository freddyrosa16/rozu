# rozu v7.4 motion spec: five orbs

## Why five
v7.3 had a different orb for each of the ten activity states. Two things went wrong with that:
- **They didn't read at 22px.** In the pill most of the white states (Thinking, Notes, Reading, Searching, Editing, Running) came out as similar blobs, so the shape added nothing the label didn't already say.
- **They churned.** A normal agent loop goes read → grep → read → edit → run in a second or two. Each tool call changed the state, and each change started a 600ms morph, so the orb was always mid-morph.

The orb's job is to say whether the agent is busy, needs you, finished, failed or lost its connection. The label says the rest. So v7.4 has one orb per thing you need to know, and every busy state shares the Working orb. Changing between states that share an orb only changes the label.

| Orb | States |
|---|---|
| Working | Thinking, Notes, Reading, Searching, Editing, Running |
| Waiting (amber) | Waiting |
| Done (green) | Done |
| Error (red) | Error |
| Offline (dim) | Offline |

## Shared
- 120 dots (60 at 22px via the `((i>>1)+i)%2==0` subsample), perspective `k = 3.2/(3.2−z)`, depth size and α, the top-left light, the lit boost and the depth sort are unchanged from v7.3, and so are the 600ms morph, the 150ms pulse and the reduced-motion cross-fade.
- Working and Waiting are a sphere: dot `i` sits on Fibonacci-sphere direction `dᵢ`, seen through `view(p, t, yaw) = rotX(rotY(p, t·yaw), .3)`.
- Done, Error and Offline gather the dots into a flat glyph on the plane z = .3, facing the camera, so a change into them reads as the cloud collapsing into a symbol. Dots `0..44` are a faint ring of radius .86 (`lit 0`, `s .5`, `a .3`); dots `44..120` draw the glyph (`lit 1`, `s 1.15`). Both are contiguous index runs, so the 22px subsample keeps an even share of each.

## Orbs
| Orb | Math | Tempo | Still T |
|---|---|---|---|
| **Working**: orbit | Sphere r = .9 + .05·sin(2πt/2.8 + 3dᵧ), yaw .35 rad/s. Orbit frame `q = rotX(p̂, −.75)`; θ = atan2(q_z, q_x); band = exp(−(q_y/.5)²). Head at 2πt/1.4, a twin at +π with gain .55. Tail = 1 − lag/2.4 for lag < 2.4 rad; lit = clamp(1.35·band·max(tail^1.5·gain)). s = .6 + .8·lit, a = .38 + .62·lit | 1.4s lap | .45s |
| **Waiting**: ping | No yaw. r = .86·(1 + .04·sin(2πt/1.8)). γ = angle from the view axis; u = (t/1.8) mod 1; ping = exp(−((γ − 2.2u)/.28)²)·(1 − u); lit = clamp(1.4·ping). Between pings the face glows with the breath: a = .32 + .68·max(lit, .25·breath·max(0, ẑ)) | 1.8s | .5s |
| **Done**: check | Glyph dots spread evenly along the polyline (−.46, .02) → (−.14, −.32) → (.48, .34). Dot at arc fraction u is drawn (lit 1, a 1) once u ≤ clamp((t − .25)/.45), otherwise lit .25, a .35, s .6. A pop of s × (1 + .2·sin(π(t − .7)/.25)) for t in (.7, .95). Static from .95s | once | 1.5s |
| **Error**: cross | Two strokes (±.36, ±.36), half the glyph dots each. The whole glyph shifts by dx = .13·exp(−5s)·sin(28s), s = t − .3, for s in (0, .9). Static from 1.2s | once | 1.5s |
| **Offline**: dashed ring | All 120 dots on the ring, turning once every 2.4s. Six dashes: the last 38% of each sixth has s = 0. Each dash's middle (within .08 of .31) is brighter: lit .6, s .9, a .8; the rest lit 0, s .65, a .45. α × .85 (dim) | 2.4s turn | 0s |

## 22px check
`screens/v7.4-22px.png`: each orb over one loop (t = 0, .23, .47, .7, .93, 1.6s) at 22px, 2× device px, upscaled 5× nearest-neighbour, dark then light. The Working comet is lit in every frame and goes all the way round; the check, the cross and the dashed ring hold their shape at 60 dots.
