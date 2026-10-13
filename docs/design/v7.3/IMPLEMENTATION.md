# v7.3 activity orb: implementation notes

This file covers what changed from v7.2. Anything not listed here (crate layout, the morph, the pulse, the frame policy, event mapping, tokens, and how to run the demo) is the same as in [`../v7.2/IMPLEMENTATION.md`](../v7.2/IMPLEMENTATION.md).

Source of truth: `reference/v7.3-orb.js` and `motion-spec-v7.3.md`.

## What changed

**`crates/rozu-activity/src/generate.rs`**
- Searching, Notes, Waiting, Done, Error and Offline are ported 1:1 from `v7.3-orb.js`:

  | State | Cue |
  |---|---|
  | Searching | A scan lens. The cap axis is normalised, and the cap dots are pushed out ×1.06 in x/y only. |
  | Notes | An ink stroke. The head is at `2πt/1.5`, the comet is 4.4 rad long, and the tail tapers in size (1.25→.95) and α (1→.65). |
  | Waiting | Pause bars, lit .7–1 with the breath. |
  | Done | A halo. There's one ripple, then the silhouette rim stays lit. |
  | Error | A break. 38% of the dots are flung out and lit; the core shrinks 10% and fades. |
  | Offline | A broken vertical ring. The spark shows on tick 2 of every 4, and only a third of the non-ring dots are drawn (`s = 0` for the rest). |

- The reference's `C(p, on, sizeOn, sizeOff, alphaOff)` is the private `cue()` helper.
  - Cue dots: `lit 1`, `s = sizeOn`, `a = 1`.
  - Context dots: `lit 0`, `s = sizeOff`, `a = alphaOff`.
  - Notes overrides `a` and Waiting overrides `lit` on cue dots, as in the JS.
- Thinking, Reading, Editing and Running are untouched. `ease()` is gone because nothing uses it any more; the v7.3 JS inlines `ph*ph`.

**`state.rs`**
- `tempo()`: Notes changes from 3.6s to 3.0s. The stroke itself writes round every 1.5s, which is noted in the doc comment.
- `reduced_motion_time()`:
  - Searching changes from .9 to .5.
  - Done changes from .55 to 1.6.
  - Offline changes from .75 to .35.
  - Notes stays at 1.1, and the other states are unchanged.

**`project.rs`: the dot-size fix**
- `ProjectOptions::apply_state_size` now defaults to `true`, matching v7.3 `drawOrb`:
  - Perspective is `k`.
  - The radius is `base · s · k · …`.
  - A dot is skipped when `s ≤ .02` or `k ≤ .02`.
- `ProjectOptions { apply_state_size: false }` still reproduces the v7.2 bug (every dot at state size 1), for side-by-side comparison. I kept the option, rather than removing it, so the demo can still toggle it.
- Visible effects of the fix in every state, the liked four included:
  - The 1.3× and 1.35× cue dots render at their spec size.
  - Editing's wedge is empty.
  - Offline's hidden cloud is gone.
- The generator outputs of the liked four didn't change. Their drawn sizes did, which is the point of the fix.

**`spikes/gpui-spike/src/bin/activity_orb.rs`**
- Key **S** now starts on, with the status `state size on (v7.3)`. Pressing it switches to `off (v7.2 bug)`.
- The header comment says v7.3.

**Doc and metadata touch-ups:** `lib.rs`, `tokens.rs` and the `Cargo.toml` description now point at v7.3. The token colours are identical; v7.3 only adds `contextAlpha`, `cueSize` and `notesStroke`, which the crate doesn't need as constants.

**Not touched:** `events.rs` (event mapping), `orb.rs` and `policy.rs`.

## Tests (`cargo test -p rozu-activity`: 37 pass)

New in `generate.rs`:
- `liked_four_are_unchanged_from_v7_2`
  - 28 sample points (position, lit, s and α) for Thinking, Reading, Editing and Running.
  - 16 whole-cloud checksums (Σ value·(1 + i/7) over all 120 dots at four times).
  - All captured from the v7.2 code before the port, and asserted to 1e-12 and 1e-9.
- `redesigned_still_frames_show_the_cue`: at each of the six states' reduced-motion frame:
  - There are at least 4 cue dots (lit ≥ .7, α ≥ .6, s ≥ .95) and at least 4 faded dots (lit 0, α ≤ .3, s ≤ .55).
  - Every drawn dot is one or the other.
  - At least 2 cue dots survive the 22px subsample.
- `redesigned_cue_sizes_match_the_spec`: over 6s, cue sizes are in .95–1.4, and context dots are in .45–.55 with α .18–.30.
- `offline_shows_at_most_half_the_dots`: at most 60 of the 120 dots have `s > .02`, at every sampled t.
- `offline_spark_fills_the_gap_on_tick_two`: the gap is only lit during tick 2 of each 1.2s cycle.
- `error_settles`
  - Frames are identical from .98s (when the spring saturates) through 60s.
  - The flung third is 25–50% of the dots, all outside the core.
- `done_settles`: from `SETTLE` (1.3s), every dot is on the .84 shell, and exactly the silhouette dots are lit. Mid-ripple, some dots are pushed out and lit.

New or changed in `project.rs`:
- `state_size_is_applied_by_default` replaces `state_size_option_hides_editing_gap`.
- `state_size_scales_the_radius` is new: `s` 1.3 gives a 1.3× radius, and `s` .02 is skipped.
- `sorted_far_to_near_and_in_bounds` now expects `s ≤ .02` dots to be skipped.

Existing tests that already cover the six states: `every_state_is_finite`, `deterministic`, `reduced_motion_is_static`, and the morph tests.

## Notes and gaps

- **Done keeps yawing.** As in the JS, `view(…, t, .12)` keeps rotating after the ripple, so its frames aren't bit-identical after 1.3s. The orb still settles visually, because the rim halo is rotation-stable, and `Orb::is_settled` / `FramePolicy` stop drawing at `SETTLE`, so the drawn frame freezes. Error has no yaw and is truly static.
- **Still not done:**
  - A numeric cross-check of the six ports against `v7.3-orb.js` in node. It needs an approval this session didn't have.
  - The 22px golden-image test against `screens/v7.3-22px-compare.png`.
  - Launching the demo. It was built, not run.
