# v7.5 activity orb: implementation notes

Source of truth: `reference/v7.5-orb.js` and `motion-spec-v7.5.md`.

## Crate: `crates/rozu-activity`
Pure Rust; deps `serde_json`, plus `rozu-core` behind the default `core` feature.

| Module | Contents |
|---|---|
| `state.rs` | `ActivityState` (10 states): label, hint, tone, dim, `plays_once`, `tempo()`, `reduced_motion_time()`. Unchanged from v7.4. |
| `generate.rs` | **New:** `OrbSize { Large, Small }` with `rings()`, `lon()`, `dots()`, `grid(i)`, `dot()`, `speed()` and `for_px()`; `generate(size, state, i, t)`: the reference `gen`, ported 1:1; `vnoise`, `hash2`, the `globe`, `globe_tilted`, `front`, `round_from`, `geo`, `along` and `symbol` helpers; `PAGE_TILT`; `MAX_DOTS` (284), `R`, `GLYPH_Z`. |
| `orb.rs` | `Orb` timeline. **New:** `Orb::with_size(size, state, start)` (`Orb::new` is Large), `size()`, `dots()`. The stagger is now `hash(i) · STAGGER` (300ms) across the whole cloud, so `MORPH_END` is 1.26s for both sizes. |
| `project.rs` | Projection to `DrawDot`s. The 22px parity subsample is gone: the sample should come from an orb of `OrbSize::for_px(size_px)`. `POINTS_22` is 86, `MAX_POINTS` 284; base radius .011·size (.045·size small). |
| `policy.rs` | `FramePolicy`, unchanged. |
| `events.rs` | Agent events → `Activity { state, label, … }`, unchanged. |
| `tokens.rs` | `Palette` and sizes, unchanged. |

Removed: the v7.4 ring/symbol generators (`RING`, `GLYPH`, `GLYPH_R`, `fib`, `N_POINTS`).

## GPUI demo
`spikes/gpui-spike/src/bin/activity_orb.rs` keeps two orbs on one timeline, `Orb::new` for the 88px orb and `Orb::with_size(OrbSize::Small, …)` for the pill, and cycles through the ten states.

```sh
cargo test -p rozu-activity
cd spikes/gpui-spike && cargo run --bin activity_orb
```

## Tests (`cargo test -p rozu-activity`: 44 pass)
- `matches_the_reference`: whole-cloud checksums (Σ value·(1 + i/7) over all dots) for each state at four times and both sizes, generated from `v7.5-orb.js` in node, to 1e-9.
- `grids_match_the_reference`: 284 and 86 dots, rings in order, dots in order round each ring.
- `busy_states_stay_on_the_globe`, `states_differ`.
- Per state: `thinking_sparks_ripple_on_the_front`, `notes_write_three_lines_then_hold`, `reading_band_moves_down`, `searching_lens_roams_the_front`, `editing_types_deletes_retypes_then_blinks`, `running_spins_fast`, `waiting_breathes`, `done_draws_a_check_then_holds` (both sizes), `error_shakes_then_holds`, `offline_dims_and_drops_rings` (both sizes).
- The morph, pulse, interrupt, settle, reduced-motion, projection and frame-policy tests carry over.

## Previews
- `reference/v7.5-sheet.html`: all ten states animating at 96px and in the 22px pill. `?still` shows the reduced-motion frames, `?T=0.9` a fixed time, `?theme=light` the light theme.
- `reference/v7.5-zoom.html?orb=thinking&c=s&ts=0,.5,1`: one orb at several times (`c=s` for 22px).
- `reference/v7.5-morph.html?from=running&to=done`: a morph as a filmstrip.

## Gaps
- The 22px golden-image test against `screens/v7.5-strip-dark.png` is still not written.
- The GPUI demo was not launched here (no display); only the crate was built and tested.
