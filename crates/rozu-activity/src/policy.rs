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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActivityState::*;

    #[test]
    fn fps_by_window_and_state() {
        let orb = Orb::new(Thinking, 0.0);
        let p = FramePolicy::default();
        assert_eq!(p.fps(&orb, 1.0), 60);
        assert_eq!(
            FramePolicy {
                low_power: true,
                ..p
            }
            .fps(&orb, 1.0),
            30
        );
        assert_eq!(
            FramePolicy {
                focused: false,
                ..p
            }
            .fps(&orb, 1.0),
            0
        );
        assert_eq!(
            FramePolicy {
                visible: false,
                ..p
            }
            .fps(&orb, 1.0),
            0
        );
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
        rm.set_state(Waiting, 2.0);
        assert_eq!(p.fps(&rm, 2.05), 60, "cross-fade");
        assert_eq!(p.fps(&rm, 2.2), 0);
    }
}
