//! The ten activity states (handoff §3) and their static properties.

/// Dot and label colour role. Hue is only used for Waiting, Done and Error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Tone {
    #[default]
    Neutral,
    Warning,
    Success,
    Danger,
}

/// What the agent is doing, as shown by the orb pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityState {
    Thinking,
    Notes,
    Reading,
    Searching,
    Editing,
    Running,
    Waiting,
    Done,
    Error,
    Offline,
}

impl ActivityState {
    /// All states, in the reference sheet's order.
    pub const ALL: [ActivityState; 10] = [
        ActivityState::Thinking,
        ActivityState::Notes,
        ActivityState::Reading,
        ActivityState::Searching,
        ActivityState::Editing,
        ActivityState::Running,
        ActivityState::Waiting,
        ActivityState::Done,
        ActivityState::Error,
        ActivityState::Offline,
    ];

    /// The label used when there's no file, query or command to show.
    pub fn label(self) -> &'static str {
        match self {
            ActivityState::Thinking => "Thinking…",
            ActivityState::Notes => "Jotting down notes…",
            ActivityState::Reading => "Reading…",
            ActivityState::Searching => "Searching…",
            ActivityState::Editing => "Editing…",
            ActivityState::Running => "Running…",
            ActivityState::Waiting => "Waiting for approval",
            ActivityState::Done => "Done",
            ActivityState::Error => "Failed",
            ActivityState::Offline => "Offline · retrying",
        }
    }

    /// The keyboard hint shown in `orbMeta` after the label, if any.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            ActivityState::Waiting => Some("Y / N"),
            ActivityState::Error => Some("R retry"),
            ActivityState::Done | ActivityState::Offline => None,
            _ => Some("Esc stop"),
        }
    }

    /// Dot and label colour.
    pub fn tone(self) -> Tone {
        match self {
            ActivityState::Waiting => Tone::Warning,
            ActivityState::Done => Tone::Success,
            ActivityState::Error => Tone::Danger,
            _ => Tone::Neutral,
        }
    }

    /// Offline dots are drawn at α×.85.
    pub fn dim(self) -> bool {
        self == ActivityState::Offline
    }

    /// The state's loop period in seconds (`loop` in the reference); 0 for the play-once states.
    /// Reading's band sweeps at twice this (4.4s); Offline advances in .3s ticks.
    pub fn tempo(self) -> f64 {
        match self {
            ActivityState::Thinking => 1.6,
            ActivityState::Searching => 2.4,
            ActivityState::Reading => 2.2,
            ActivityState::Notes => 3.6,
            ActivityState::Editing => 1.06,
            ActivityState::Running => 0.8,
            ActivityState::Waiting => 2.6,
            ActivityState::Done | ActivityState::Error => 0.0,
            ActivityState::Offline => 2.4,
        }
    }

    /// Done and Error play once and then settle (frame policy: 0fps after [`crate::SETTLE`]).
    pub fn plays_once(self) -> bool {
        matches!(self, ActivityState::Done | ActivityState::Error)
    }

    /// The time (s) of the static frame drawn in reduced-motion mode (handoff §3). Each one shows
    /// the state's signature cue.
    pub fn reduced_motion_time(self) -> f64 {
        match self {
            ActivityState::Thinking => 0.9,
            ActivityState::Searching => 0.9,
            ActivityState::Reading => 0.5,
            ActivityState::Notes => 1.1,
            ActivityState::Editing => 0.06,
            ActivityState::Running => 0.35,
            ActivityState::Waiting => 0.65,
            ActivityState::Done => 0.55,
            ActivityState::Error => 1.0,
            ActivityState::Offline => 0.75,
        }
    }
}
