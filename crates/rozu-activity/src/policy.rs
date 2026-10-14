//! Frame policy (handoff §6): 60fps focused, 30fps on battery, 0fps when unfocused, occluded
//! or settled; reduced motion draws only around a change.

use std::time::Duration;

use crate::orb::Orb;

/// What the host knows about the window and power state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePolicy {
    pub focused: bool,
    /// False when the window is occluded or minimised.
    pub visible: bool,
    /// On battery or under thermal pressure.
    pub low_power: bool,
}

impl Default for FramePolicy {
    fn default() -> Self {
        FramePolicy {
            focused: true,
            visible: true,
            low_power: false,
        }
    }
}

impl FramePolicy {
    /// Frames per second for `orb` at `t`; 0 means draw nothing until something changes (a new
    /// state, focus, visibility). The host's animation clock should stop while this is 0.
    pub fn fps(&self, orb: &Orb, t: f64) -> u32 {
        if !self.focused || !self.visible || !orb.is_animating(t) {
            0
        } else if self.low_power {
            30
        } else {
            60
        }
    }

    /// The time between frames, or `None` when paused.
    pub fn interval(&self, orb: &Orb, t: f64) -> Option<Duration> {
        match self.fps(orb, t) {
            0 => None,
            fps => Some(Duration::from_secs_f64(1.0 / f64::from(fps))),
        }
    }
}

/// Reduced motion (handoff §6): the OS setting (macOS Reduce motion), unless the user has
/// overridden it in the app. An override sticks, so later OS changes don't move the effective
/// value, until the user toggles back to what the OS says, which drops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReducedMotion {
    /// The OS setting, as last read.
    pub os: bool,
    /// The in-app override; `None` follows the OS.
    pub manual: Option<bool>,
}

impl ReducedMotion {
    pub fn new(os: bool) -> Self {
        ReducedMotion { os, manual: None }
    }

    /// Whether to draw in reduced motion.
    pub fn effective(&self) -> bool {
        self.manual.unwrap_or(self.os)
    }

    /// Flips the effective value (the demo's M key). Flipping back to the OS value drops the
    /// override, so the OS setting applies again.
    pub fn toggle(&mut self) {
        let on = !self.effective();
        self.manual = (on != self.os).then_some(on);
    }

    /// Records a new OS reading; returns whether the effective value changed.
    pub fn set_os(&mut self, os: bool) -> bool {
        let before = self.effective();
        self.os = os;
        self.effective() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActivityState::*;

    #[test]
    fn reduced_motion_os_flag_and_override() {
        for os in [false, true] {
            for manual in [None, Some(false), Some(true)] {
                let rm = ReducedMotion { os, manual };
                assert_eq!(rm.effective(), manual.unwrap_or(os), "{rm:?}");
            }
        }
        assert!(
            ReducedMotion::new(true).effective(),
            "follows the OS at startup"
        );

        let mut rm = ReducedMotion::new(true);
        rm.toggle();
        assert_eq!(
            rm,
            ReducedMotion {
                os: true,
                manual: Some(false)
            }
        );
        rm.toggle();
        assert_eq!(rm, ReducedMotion::new(true));

        let mut rm = ReducedMotion::new(false);
        assert!(rm.set_os(true));
        assert!(rm.effective());
        assert!(!rm.set_os(true), "same reading");

        let mut rm = ReducedMotion::new(false);
        rm.toggle();
        assert!(rm.effective());
        assert!(!rm.set_os(true));
        assert!(!rm.set_os(false));
        assert!(rm.effective());
        rm.toggle();
        assert_eq!(rm, ReducedMotion::new(false));
        assert!(rm.set_os(true), "following the OS again");
    }

    #[test]
    fn fps_by_window_and_state() {
        let orb = Orb::new(Thinking, 0.0);
        let p = FramePolicy::default();
        for (policy, fps) in [
            (p, 60),
            (
                FramePolicy {
                    low_power: true,
                    ..p
                },
                30,
            ),
            (
                FramePolicy {
                    focused: false,
                    ..p
                },
                0,
            ),
            (
                FramePolicy {
                    visible: false,
                    ..p
                },
                0,
            ),
        ] {
            assert_eq!(policy.fps(&orb, 1.0), fps, "{policy:?}");
        }
        assert_eq!(
            p.interval(&orb, 1.0),
            Some(Duration::from_secs_f64(1.0 / 60.0))
        );

        let mut done = Orb::new(Running, 0.0);
        done.set_state(Done, 1.0);
        assert_eq!(p.fps(&done, 1.5), 60);
        assert_eq!(p.fps(&done, 2.5), 0, "settled");
        assert_eq!(p.interval(&done, 2.5), None);

        let mut rm = Orb::new(Thinking, 0.0);
        rm.set_reduced_motion(true);
        assert_eq!(p.fps(&rm, 1.0), 0);
        rm.set_state(Reading, 2.0);
        assert_eq!(p.fps(&rm, 2.05), 60, "cross-fade");
        assert_eq!(p.fps(&rm, 2.2), 0);
    }
}
