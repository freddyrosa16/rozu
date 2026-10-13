//! The rozu v7.4 activity indicator: a dot orb, a label and a timer in a pill.
//!
//! Everything here is pure and deterministic, so any renderer (GPUI, a canvas, a golden-image
//! test) draws the same frame for the same time:
//!
//! 1. [`generate`] makes one [`OrbKind`]'s 120 dots at a time `t` (`gen` in
//!    `docs/design/v7.4/reference/v7.4-orb.js`, ported 1:1; `gen` is a keyword in Rust 2024).
//!    The ten [`ActivityState`]s share five orbs: every busy state is [`OrbKind::Working`].
//! 2. [`Orb::sample`] blends orbs over a timeline: the 600ms critically damped morph and the
//!    150ms brighten pulse, or static frames in reduced-motion mode.
//! 3. [`project`] turns the samples into a depth-sorted draw list (≤120 dots) for a given orb
//!    size and scale factor, applying each dot's state size (`base · s · k · …`).
//! 4. [`FramePolicy`] says how often to draw, and [`ActivityTracker`] maps agent events to states
//!    and labels.

mod events;
mod generate;
mod orb;
mod policy;
mod project;
mod state;
pub mod tokens;

pub use events::{Activity, ActivityEvent, ActivityTracker, tool_state};
#[cfg(feature = "core")]
pub use events::{from_agent_event, from_agent_events};
pub use generate::{CAMERA, Dot, N_POINTS, Vec3, fib, generate, hash};
pub use orb::{MORPH, MORPH_END, Orb, PULSE, SETTLE, STAGGER, Sample};
pub use policy::FramePolicy;
pub use project::{DrawDot, MAX_POINTS, POINTS_22, ProjectOptions, project, project_with};
pub use state::{ActivityState, OrbKind, Tone};
