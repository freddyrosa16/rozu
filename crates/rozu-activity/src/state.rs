//! The ten activity states (handoff §3), the five orbs that show them, and their static
//! properties.

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

    /// The orb this state shows. Every busy state shares [`OrbKind::Working`]; the label says
    /// which kind of work it is.
    pub fn orb(self) -> OrbKind {
        match self {
            ActivityState::Thinking
            | ActivityState::Notes
            | ActivityState::Reading
            | ActivityState::Searching
            | ActivityState::Editing
            | ActivityState::Running => OrbKind::Working,
            ActivityState::Waiting => OrbKind::Waiting,
            ActivityState::Done => OrbKind::Done,
            ActivityState::Error => OrbKind::Error,
            ActivityState::Offline => OrbKind::Offline,
        }
    }

    /// Dot and label colour.
    pub fn tone(self) -> Tone {
        self.orb().tone()
    }

    /// Offline dots are drawn at α×.85.
    pub fn dim(self) -> bool {
        self.orb().dim()
    }

    /// Done and Error play once and then settle (frame policy: 0fps after [`crate::SETTLE`]).
    pub fn plays_once(self) -> bool {
        self.orb().plays_once()
    }
}

/// One of the five orbs (handoff §3). Each [`ActivityState`] shows one; the orb only morphs when
/// this changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrbKind {
    /// Orbit: a lit comet circles a turning sphere.
    Working,
    /// Ping (amber): the sphere holds still and pings outward from its face.
    Waiting,
    /// Check (green): draws itself once inside a faint ring, then holds.
    Done,
    /// Cross (red): shakes once inside a faint ring, then holds.
    Error,
    /// Dashed ring (dim): turns slowly while the connection retries.
    Offline,
}

impl OrbKind {
    pub const ALL: [OrbKind; 5] = [
        OrbKind::Working,
        OrbKind::Waiting,
        OrbKind::Done,
        OrbKind::Error,
        OrbKind::Offline,
    ];

    /// Dot colour. Hue is only used for Waiting, Done and Error.
    pub fn tone(self) -> Tone {
        match self {
            OrbKind::Waiting => Tone::Warning,
            OrbKind::Done => Tone::Success,
            OrbKind::Error => Tone::Danger,
            OrbKind::Working | OrbKind::Offline => Tone::Neutral,
        }
    }

    /// Offline dots are drawn at α×.85.
    pub fn dim(self) -> bool {
        self == OrbKind::Offline
    }

    /// The loop period in seconds (`loop` in the reference); 0 for the play-once orbs.
    pub fn tempo(self) -> f64 {
        match self {
            OrbKind::Working => 1.4,
            OrbKind::Waiting => 1.8,
            OrbKind::Done | OrbKind::Error => 0.0,
            OrbKind::Offline => 2.4,
        }
    }

    /// Done and Error play once and then settle.
    pub fn plays_once(self) -> bool {
        matches!(self, OrbKind::Done | OrbKind::Error)
    }

    /// The time (s) of the still frame drawn in reduced-motion mode (handoff §5). Each one shows
    /// the orb's signature: the comet, the ping ring, the finished check or cross, the dashes.
    pub fn reduced_motion_time(self) -> f64 {
        match self {
            OrbKind::Working => 0.45,
            OrbKind::Waiting => 0.5,
            OrbKind::Done | OrbKind::Error => 1.5,
            OrbKind::Offline => 0.0,
        }
    }
}

impl From<ActivityState> for OrbKind {
    fn from(state: ActivityState) -> Self {
        state.orb()
    }
}
