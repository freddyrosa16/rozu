# v7.4 activity orb: implementation notes

Source of truth: `reference/v7.4-orb.js` and `motion-spec-v7.4.md`.

## Crate: `crates/rozu-activity`
Pure Rust; deps `serde_json`, plus `rozu-core` behind the default `core` feature.

| Module | Contents |
|---|---|
| `state.rs` | `ActivityState` (10 states): label, hint, tone, dim, `plays_once`. `tempo()` and `reduced_motion_time()` carry the v7.4 values. |
| `generate.rs` | `generate(state, i, t)`: the reference `gen`, ported 1:1. **New:** `RING`, `GLYPH`, `GLYPH_Z`, `GLYPH_R` (dots `0..RING` are the ring, the rest the symbol), and the `ring`, `flat`, `along` and `arc` helpers. |
| `orb.rs` | `Orb` timeline, unchanged from v7.3. |
| `project.rs` | Projection to `DrawDot`s, unchanged. |
| `policy.rs` | `FramePolicy`, unchanged. |
| `events.rs` | Agent events → `Activity { state, label, … }`, unchanged: the states and labels still distinguish Reading from Running, only the orb doesn't. |
| `tokens.rs` | `Palette` and sizes, unchanged. |

Removed: the v7.3 per-state cloud generators and their `cue()` helper.

## GPUI demo
`spikes/gpui-spike/src/bin/activity_orb.rs` builds against the same API (`Orb::new`, `set_state`, `project`) and cycles through the ten states.

```sh
cargo test -p rozu-activity
cd spikes/gpui-spike && cargo run --bin activity_orb
```

## Tests (`cargo test -p rozu-activity`: 44 pass)
- `matches_the_reference`: whole-cloud checksums (Σ value·(1 + i/7) over all 120 dots) for each state at four times, generated from `v7.4-orb.js` in node, to 1e-9.
- `the_ring_is_shared`: in every state the 44 ring dots are at their ring positions (Offline's gap and Error's shake aside) and never fade below α .4.
- `symbols_differ`: every state's still frame differs from every other's.
- Per state: `thinking_sweeps_the_ring_and_turns_the_sphere`, `notes_write_then_hold`, `reading_band_moves_down`, `searching_is_a_lit_magnifier`, `editing_types_then_blinks`, `running_has_two_arcs`, `waiting_breathes`, `done_draws_a_check_then_holds`, `error_shakes_then_holds`, `offline_has_a_gap_and_a_turning_arrow`.
- The morph, pulse, interrupt, settle, reduced-motion, projection and frame-policy tests carry over from v7.3.

## Previews
- `reference/v7.4-sheet.html`: all ten states animating at 96px and in the 22px pill. `?still` shows the reduced-motion frames, `?T=0.9` a fixed time, `?theme=light` the light theme.
- `reference/v7.4-zoom.html?orb=thinking&c=s&ts=0,.5,1`: one orb at several times (`c=s` for 22px).
- `reference/v7.4-morph.html?from=running&to=done`: a morph as a filmstrip.

## Gaps
- The 22px golden-image test against `screens/v7.4-22px-dark.png` is still not written.
- The GPUI demo was not launched here (no display); only the crate was built and tested.
