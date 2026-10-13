# v7.4 activity orb: implementation notes

Source of truth: `reference/v7.4-orb.js` and `motion-spec-v7.4.md`.

## Crate: `crates/rozu-activity`
Pure Rust; deps `serde_json`, plus `rozu-core` behind the default `core` feature.

| Module | Contents |
|---|---|
| `state.rs` | `ActivityState` (10 states): label, hint (`Y / N`, `R retry`, `Esc stop`), and `orb()`. **New:** `OrbKind` (5 orbs) with `tone`, `dim`, `tempo`, `plays_once` and `reduced_motion_time`. `ActivityState::tone/dim/plays_once` delegate to its orb. |
| `generate.rs` | `generate(orb, i, t)`: the reference `gen`, ported 1:1. |
| `orb.rs` | `Orb` timeline. `set_state(state, T)` records the state and only starts a morph when `state.orb()` changes (it returns whether it did). `state()` is the latest state, `kind()` the orb on screen. Morph, pulse, interrupts, reduced motion and settling are unchanged. |
| `project.rs` | Projection to `DrawDot`s, unchanged. |
| `policy.rs` | `FramePolicy`, unchanged. |
| `events.rs` | Agent events → `Activity { state, label, … }`, unchanged: the states and labels still distinguish Reading from Running, only the orb doesn't. |
| `tokens.rs` | `Palette` and sizes, unchanged. |

Removed: `ActivityState::tempo()` and `ActivityState::reduced_motion_time()` (now on `OrbKind`), and the v7.3 per-state generators.

## GPUI demo
`spikes/gpui-spike/src/bin/activity_orb.rs` still builds against the same API (`Orb::new`, `set_state`, `project`). It cycles through the ten states, so the orb visibly holds the Working orb across the busy steps and only morphs into Waiting, Done, Error and Offline.

```sh
cargo test -p rozu-activity
cd spikes/gpui-spike && cargo run --bin activity_orb
```

## Tests (`cargo test -p rozu-activity`)
- `matches_the_reference`: whole-orb checksums (Σ value·(1 + i/7) over all 120 dots) for each orb at four times, generated from `v7.4-orb.js` in node, to 1e-9.
- Per orb: `working_comet_is_always_visible_and_goes_round`, `working_is_a_sphere`, `waiting_pings_from_the_face`, `done_draws_a_check_then_holds`, `error_shakes_then_holds`, `offline_is_a_dashed_ring`.
- `busy_states_share_the_working_orb`: Thinking → Reading → Searching → Editing → Running → Notes never morphs and never restarts the orb's clock; Waiting does morph.
- The morph, pulse, interrupt, settle, reduced-motion, projection and frame-policy tests carry over, switched to orb changes.

## Previews
- `reference/v7.4-sheet.html`: all five orbs animating at 120px and in the 22px pill. `?still` shows the reduced-motion frames, `?T=0.9` a fixed time, `?theme=light` the light theme.
- `reference/v7.4-zoom.html?orb=working&c=s&ts=0,.5,1`: one orb at several times (`c=s` for 22px).
- `reference/v7.4-morph.html?from=working&to=done`: a morph as a filmstrip.

## Gaps
- The 22px golden-image test against `screens/v7.4-22px.png` is still not written.
- The GPUI demo was not launched here (no display); only the crate was built and tested.
