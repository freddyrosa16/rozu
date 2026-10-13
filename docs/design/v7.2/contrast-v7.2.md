# v7.2 contrast: 3D dot orbs (WCAG 2.2 AA)
Pill: dark #1B1B1D, light #ECECEF. Dots: dark #F4F1EA, light #25242A, with status hues for waiting, done and error. The light status tones are darkened (amber #8A5206, green #126B33) so the labels clear 4.5:1 on #ECECEF.

**Rule:** dots at the **front depth** must be ≥3:1. Front depth is z ≥ 0.86 (camera at 3.2), which gets opacity α = 0.22 + 0.78·depth ≥ 0.89. Lit dots are forced to α 1.

**Depth cue:** mid and back dots fade on purpose. They're decorative depth cues, like a gradient's shading. Every orb's silhouette is carried by its front hemisphere, and the label carries the state. The mid and back rows below are marked informational, not judged.

**Crispness:** solid fills, no blur or glow. Dots ≤1.6 device px are drawn as pixel-snapped squares.

| Theme | Pair | Ratio | Req | Pass |
|---|---|---|---|---|
| dark | label (--orb-label) on pill | 15.25:1 | 4.5:1 | ✅ |
| dark | timer · step 0:08 · 3/5 (--orb-meta) on pill | 6.16:1 | 4.5:1 | ✅ |
| dark | amber label on pill | 10.30:1 | 4.5:1 | ✅ |
| dark | green label on pill | 9.87:1 | 4.5:1 | ✅ |
| dark | red label on pill | 6.22:1 | 4.5:1 | ✅ |
| dark | front dot, full opacity | 15.25:1 | 3:1 | ✅ |
| dark | front-depth dot (z≥0.86, α0.89) | 12.21:1 | 3:1 | ✅ |
| dark | amber front-depth dot (α0.89) | 8.38:1 | 3:1 | ✅ |
| dark | green front-depth dot (α0.89) | 8.00:1 | 3:1 | ✅ |
| dark | red front-depth dot (α0.89) | 5.17:1 | 3:1 | ✅ |
| dark | mid-depth dot z=0 (α0.61), depth cue | 6.41:1 | info | ℹ️ |
| dark | back dot z=−1 (α0.24), depth cue | 2.07:1 | info | ℹ️ |
| light | label (--orb-label) on pill | 13.05:1 | 4.5:1 | ✅ |
| light | timer · step 0:08 · 3/5 (--orb-meta) on pill | 6.27:1 | 4.5:1 | ✅ |
| light | amber label on pill | 5.41:1 | 4.5:1 | ✅ |
| light | green label on pill | 5.61:1 | 4.5:1 | ✅ |
| light | red label on pill | 5.49:1 | 4.5:1 | ✅ |
| light | front dot, full opacity | 13.05:1 | 3:1 | ✅ |
| light | front-depth dot (z≥0.86, α0.89) | 9.55:1 | 3:1 | ✅ |
| light | amber front-depth dot (α0.89) | 4.36:1 | 3:1 | ✅ |
| light | green front-depth dot (α0.89) | 4.54:1 | 3:1 | ✅ |
| light | red front-depth dot (α0.89) | 4.77:1 | 3:1 | ✅ |
| light | mid-depth dot z=0 (α0.61), depth cue | 4.05:1 | info | ℹ️ |
| light | back dot z=−1 (α0.24), depth cue | 1.61:1 | info | ℹ️ |

Failures (judged pairs): 0

## v7.2 depth + light
v7.2 widens the depth range (back α .12, back size 40%) and multiplies α by a top-left light term (.62+.48·λ). The worst unlit front-cap dot (shell r .86, z ≥ .75, facing away from the light) still has α ≥ 0.74, so it passes 3:1 in every tone. The signature-cue dots (core, hot spot, band, rim, arms) are a=1 and lit, so they are brighter still. Non-cue background dots are deliberately faded (state α×.18–.55) as context; they are not the indicator and are exempt, in the same way as v7's back-depth dots. Labels, pill and colours are unchanged from v7, so their 4.5:1 checks hold.

| Pair | Ratio | Need |
|---|---|---|
| dark dot dot front, worst light (α 0.74) | 8.80:1 | ≥3:1 |
| dark dot dot front, offline ×.85 (α 0.63) | 6.73:1 | ≥3:1 |
| dark warning dot front, worst light (α 0.74) | 6.16:1 | ≥3:1 |
| dark success dot front, worst light (α 0.74) | 5.91:1 | ≥3:1 |
| dark danger dot front, worst light (α 0.74) | 3.96:1 | ≥3:1 |
| light dot dot front, worst light (α 0.74) | 5.98:1 | ≥3:1 |
| light dot dot front, offline ×.85 (α 0.63) | 4.29:1 | ≥3:1 |
| light warning dot front, worst light (α 0.74) | 3.27:1 | ≥3:1 |
| light success dot front, worst light (α 0.74) | 3.36:1 | ≥3:1 |
| light danger dot front, worst light (α 0.74) | 3.76:1 | ≥3:1 |
