# v7.2 activity orb: implementation notes

## What's implemented

**`crates/rozu-activity`** (workspace crate, pure Rust; deps: `serde_json`, plus `rozu-core` behind the default `core` feature):

| Module | Contents |
|---|---|
| `state.rs` | `ActivityState` (10 states): default label, hint (`Y / N`, `R retry`, `Esc stop`), `Tone`, dim, tempo, reduced-motion frame time (handoff §3). |
| `generate.rs` | `generate(state, i, t)`: the reference `gen` ported 1:1 (fib sphere, hash, `view`, `sprO`, every per-state cue). It's called `generate` because `gen` is a keyword in Rust 2024. |
| `orb.rs` | `Orb` timeline: `set_state(state, T)`, `sample(T)` / `sample_into`. Implements the 600ms critically damped morph (ω=7, 2.5ms stagger, z lift .55·sin πq), the tone/dim flip at q=.5, interpolated lit/s/α, and the 150ms brighten pulse. Also has reduced-motion mode, `is_settled` (Done/Error after 1.3s) and `is_animating`. |
| `project.rs` | `project(points, size_px, scale)` → `Vec<DrawDot { x, y, radius, alpha, tone, pixel_square }>`. Covers perspective (F=3.2), depth size .4→1 and α .12→1, the top-left light, the lit boost, the per-dot α, offline ×.85, the 22px subsample `((i>>1)+i)%2==0` (60 dots), the 120 cap, the stable far→near sort, and pixel squares for dots under 1.6 device px at ≤30px. `DrawDot::square(scale)` returns the snapped rect. |
| `policy.rs` | `FramePolicy { focused, visible, low_power }.fps(&orb, T)`: 60 focused, 30 on low power, 0 when unfocused, occluded or settled, and 0 in reduced motion except during the 150ms change. |
| `events.rs` | `ActivityEvent` (a mirror of the agent events), `ActivityTracker`, `tool_state(name)`. With `core`: `From<&AgentEvent>`, `From<&ApprovalRequest>`, `from_agent_event`, `from_agent_events`. |
| `tokens.rs` | `Palette` (tokens-v7 `color.orb*`, dark/light), sizes (22 / 88 / 32 / 34px). |

Event mapping:
- `TextDelta` → Thinking.
- `ToolCallStarted` is matched on whole words of the tool name, in snake, kebab or camel case:
  - notes/todo → Notes
  - grep/search/glob/find → Searching
  - write/edit/patch → Editing
  - shell/bash/exec/run → Running
  - read/list/view → Reading
- Approval requested → Waiting; the decision → back to the tool's state.
- `ToolCallFinished { is_error }` → Error (`Command failed`, `Edit failed`, …), unless the call was denied. A successful finish → Thinking.
- `TurnFinished`: Completed → Done (detail `N files` edited); MaxSteps → Error; Cancelled → idle (`None`).
- Labels take the file, query or command from `args_json`: `Reading README.md`, `Searching "set_size"`, `Running cargo test`. They're clipped to 32 chars, and long paths are shortened to the file name.

**`spikes/gpui-spike/src/bin/activity_orb.rs`**: a GPUI 0.2 demo (path dep, `default-features = false`).
- Shows an 88px orb and the 34px dock pill: [22px orb] [label] [mono timer] [· n/10] [detail] [hint].
- Cycles all ten states every 3.2s. The labels come from a scripted turn run through `ActivityTracker`.
- Each dot is one `paint_quad`: a disc via `corner_radii`, or a device-pixel-snapped square.
- The animation clock only advances while `FramePolicy` says to draw. It stops when the window loses focus (`observe_window_activation`) and after Done/Error settle, so it resumes without a jump.

## Run

```sh
cargo test -p rozu-activity
cd spikes/gpui-spike && cargo run --bin activity_orb
# if linking fails on the Xcode license: DEVELOPER_DIR=/Library/Developer/CommandLineTools cargo run --bin activity_orb
```

Demo keys:
- **M**: reduced motion
- **Space**: pause the cycle
- **← / →**: previous / next state
- **T**: dark / light theme
- **B**: low power (30fps)
- **S**: apply the state size multiplier (see Gaps)

## Deviations and gaps

- **State size `s` isn't applied by default.** In the reference `drawOrb`, `{...b, s: F/(F-z)}` overwrites each dot's state `s` with the perspective factor. So the approved renders don't have the spec's 1.3× core dots, Editing's empty wedge or Offline's "sparse third".
  - `project` matches the reference.
  - `ProjectOptions { apply_state_size: true }` (key **S** in the demo) applies `s` as the spec text describes.
  - Design needs to pick one.
- **Interrupts blend from the live sample.** The reference blends from `gen(prev)`, which jumps if a change interrupts a morph that's still running. The Rust version recurses into the previous segment while its morph is unfinished, as handoff §4 asks. Once a morph has finished, the two are identical.
- **Reduced-motion cross-fade:** to stay at ≤120 dots, the 150ms change is a per-dot dissolve (dot i switches at `hash(i+101)`) with an α bump of `×(1+.6·sin πx)`. No dot moves.
- **No "approval pending" agent event.** `AgentEvent::Approval` arrives after the decision, so Waiting has to come from the host's `ApprovalGate` via `ActivityEvent::from(&ApprovalRequest)`.
- **Not done:**
  - A numeric cross-check against `v7.2-orb.js`. Running node needed an approval this session didn't have.
  - The 22px golden-image test against `screens/v7.2-22px-compare.png`.
  - A real single instanced draw with an SDF fragment shader. The demo issues ≤180 `paint_quad`s, which GPUI batches.
  - Battery and occlusion detection. `low_power` and `visible` are inputs; the demo toggles low power by hand.
  - The demo was built but not launched here.
