//! The rozu v7.3 activity indicator: a 3D dot orb, a label and a timer in a pill.
//!
//! Everything here is pure and deterministic, so any renderer (GPUI, a canvas, a golden-image
//! test) draws the same frame for the same time:
//!
//! 1. [`generate`] makes one state's 120-dot cloud at a time `t` (`gen` in
//!    `docs/design/v7.3/reference/v7.3-orb.js`, ported 1:1; `gen` is a keyword in Rust 2024).
//! 2. [`Orb::sample`] blends states over a timeline: the 600ms critically damped morph and the
//!    150ms brighten pulse, or static frames in reduced-motion mode.
//! 3. [`project`] turns the samples into a depth-sorted draw list (≤120 dots) for a given orb
//!    size and scale factor, applying each dot's state size (`base · s · k · …`).
//! 4. [`FramePolicy`] says how often to draw ([`ReducedMotion`]: the OS setting or an in-app
//!    override), [`ActivityTracker`] maps agent events to states
//!    and labels (an open approval prompt → Waiting), and [`Debouncer`] keeps fast tool calls
//!    from flickering the pill. With `core`, `replay` has a scripted turn for demos.

mod debounce;
mod events;
mod generate;
mod orb;
mod policy;
mod project;
#[cfg(feature = "core")]
pub mod replay;
mod state;
pub mod tokens;

pub use debounce::{Debouncer, MIN_DWELL};
pub use events::{Activity, ActivityEvent, ActivityTracker, ApprovalOutcome, tool_state};
#[cfg(feature = "core")]
pub use events::{from_agent_event, from_agent_events};
pub use generate::{CAMERA, Dot, N_POINTS, Vec3, fib, generate, hash};
pub use orb::{MORPH, MORPH_END, Orb, PULSE, SETTLE, STAGGER, Sample};
pub use policy::{FramePolicy, ReducedMotion};
pub use project::{DrawDot, MAX_POINTS, POINTS_22, ProjectOptions, project, project_with};
pub use state::{ActivityState, Tone};
