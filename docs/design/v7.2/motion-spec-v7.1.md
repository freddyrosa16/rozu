# rozu v7.1 motion spec: flow orbs
Fred: "keep the searching as the main for thinking, and make others like it." Every state is now one visual object: **a spherical cloud of 120 dots (60 at 22px) with radial flow**. Dot i has a fixed Fibonacci-sphere direction dᵢ and a phase φᵢ = (t/T + hash(i)) mod 1. States differ ONLY in how the flow behaves. Source: `html/v7.1-orb.js` (STATES).
Shared constants: radius 0.86–1.0, camera F=3.2, k=F/(F−z), dot r = base·k·(.55+.45·depth)·(1+.55·lit), α = .22+.78·depth (+.5·lit), view = rX(.3)·rY(yaw·t).

| State | Flow behaviour | Math (r = radius along dᵢ) | Loop |
|---|---|---|---|
| Thinking | CONVERGE: stream shell → core, respawn outside | r = 1 − .86φ²; lit when φ>.72 | 1.6s |
| Searching | WANDER + SWEEP: the focal point drifts; a lit meridian sweeps | p = lerp(dᵢ, f(t), .86φ²), f=(.48cos1.9t, .36sin1.3t, .3sin1.6t); lit = cos⁸(lon − 2πt/1.6) | 3.2s |
| Reading | BANDS: 5 latitude bands take turns flowing inward, top → bottom | active band (t/.36 mod 5): r = 1 − .5·sin(π·u); others are dim at r≈1 | 1.8s |
| Notes | OUTWARD TRAIL: slow outward flow; dots are born bright at the core and fade | r = .1+.9φ; lit = 1−1.8φ; size 1.2→.65 | 3.2s |
| Editing | CLICKS: the cloud pulses inward on a 530ms keystroke | r·(1 − .32·pulse); pulse rises over 64ms, then decays as e^(−9u) | 1.06s |
| Running | VORTEX: fast inflow with a twist that grows as dots fall in; 3 lit arms | rY(φ·3.2 + 3t); arm lit = cos⁶(3(lon+twist)) | 0.8s |
| Waiting (amber) | PAUSED: φ frozen mid-stream; the cloud breathes | r·(1+.07 sin(2πt/2.6)); lit .35–.75 with the breath | 2.6s |
| Done (green) | SETTLE: every dot on the shell (r=.82, still); ONE ripple runs pole to pole | r = .82 + .13·exp(−((y−front)/.18)²), front 1→−1 over 1.1s | once |
| Error (red) | STALL + SCATTER: flow freezes, then scatters outward once and holds, uneven | r₀ + (.28+.25h)·spring((t−.18)/.5), ζ≈.55, jittered direction | once |
| Offline (dim) | SPUTTER: a sparse 1/3 of dots; the flow advances in bursts and stalls | φ steps in .3s ticks (2 moving, 2 stalled); α×.85 | 2.4s |

**Morphs:** the same 120 dots throughout, so morphs are 600ms per-point lerps on a critically damped spring (ω=7), with a 2.5ms/pt stagger and a mid-morph lift of +.55·sin(πq) in z toward the camera. Tone flips at q=.5.
**Reduced motion:** render one static frame per state (the times are in `gen_v7_1.py` FR), cross-fade 150ms, and no flow. Distinguishable static cues: Thinking has a bright core inside the shell; Searching has an off-centre core plus a lit meridian; Reading has one lit band; Notes has a dense core that thins outward; Editing is a contracted dense ball; Running has 3 lit spiral arms; Waiting is amber; Done is a green even shell; Error is red and ragged; Offline is a sparse dim cloud. The label always names the state.
**22px:** 60 dots by the ((i>>1)+i)%2 subsample, base = .05·size; dots ≤1.6 device px draw as pixel-snapped squares, larger ones as AA discs.
**GPUI:** one instanced quad draw, ≤120 instances (under 200), with the instance struct {pos2, radius, alpha, color index}. Positions are computed on the CPU per frame (120 × a few trig ops), z-sorted with an insertion sort (nearly sorted frame to frame), and drawn as an SDF disc in the fragment shader with a 1-device-px smoothstep. Frame policy: 60fps when visible and focused, 30fps on battery, 0fps when occluded or unfocused, or when Done/Error has settled (after 1.3s). Text is ordinary GPUI text (AA), never drawn into the orb layer.
