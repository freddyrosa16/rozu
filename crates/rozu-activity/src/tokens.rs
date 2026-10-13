//! The tokens-v7 values the pill needs (`docs/design/v7.3/tokens-v7.json`; the colours are unchanged from v7.2), as 0xRRGGBB.

use crate::state::Tone;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// `color.orb*` for one theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub dot: u32,
    pub pill_bg: u32,
    pub pill_border: u32,
    pub label: u32,
    pub meta: u32,
    pub warning: u32,
    pub success: u32,
    pub danger: u32,
}

impl Palette {
    pub const fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Palette {
                dot: 0xF4F1EA,
                pill_bg: 0x1B1B1D,
                pill_border: 0x2A2A2E,
                label: 0xF4F1EA,
                meta: 0x9A9AA2,
                warning: 0xFBBF24,
                success: 0x4ADE80,
                danger: 0xF87171,
            },
            Theme::Light => Palette {
                dot: 0x25242A,
                pill_bg: 0xECECEF,
                pill_border: 0xDCDCE0,
                label: 0x25242A,
                meta: 0x55555C,
                warning: 0x8A5206,
                success: 0x126B33,
                danger: 0xB91C1C,
            },
        }
    }

    /// Dot colour for a tone.
    pub const fn dot(&self, tone: Tone) -> u32 {
        match tone {
            Tone::Neutral => self.dot,
            Tone::Warning => self.warning,
            Tone::Success => self.success,
            Tone::Danger => self.danger,
        }
    }

    /// Label colour: the status colour for Waiting, Done and Error.
    pub const fn label(&self, tone: Tone) -> u32 {
        match tone {
            Tone::Neutral => self.label,
            _ => self.dot(tone),
        }
    }
}

/// `size.orb`: the pill's orb (22px optical size).
pub const ORB_PX: f32 = 22.0;
/// `size.orbLg`.
pub const ORB_LG_PX: f32 = 88.0;
/// `size.activityPill` (34 in the dock).
pub const PILL_HEIGHT_PX: f32 = 32.0;
pub const PILL_HEIGHT_DOCK_PX: f32 = 34.0;
