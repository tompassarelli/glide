use crate::sampler::Sample;
use std::collections::VecDeque;
use std::time::Instant;

/// State transitions emitted by an activation algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GlideState {
    Active,
    Inactive,
}

/// An activation algorithm consumes touchpad events and decides
/// when the touchpad is being intentionally used.
pub trait ActivationAlgorithm {
    fn on_finger_down(&mut self);
    fn on_finger_up(&mut self) -> Option<GlideState>;
    fn on_sample(&mut self, sample: &Sample) -> Option<GlideState>;
    /// Current motion ratio in the rolling window, if applicable.
    fn current_motion_ratio(&self) -> Option<f64>;
    fn is_active(&self) -> bool;
}

/// Rolling window algorithm: counts motion-positive samples within a time
/// window and activates when the ratio exceeds a threshold.
pub struct RollingWindowAlgorithm {
    is_active: bool,
    motion_threshold_sq: i32,
    activation_window: std::time::Duration,
    activation_ratio: usize,
    samples: VecDeque<(Instant, bool)>,
}

impl RollingWindowAlgorithm {
    pub fn new(motion_threshold: u16, window_ms: u64, ratio: u16) -> Self {
        let t = i32::from(motion_threshold);
        Self {
            is_active: false,
            motion_threshold_sq: t * t,
            activation_window: std::time::Duration::from_millis(window_ms),
            activation_ratio: ratio as usize,
            samples: VecDeque::with_capacity(64),
        }
    }

    fn reset(&mut self) {
        self.samples.clear();
    }

    fn evict_and_ratio(&mut self, now: Instant) -> (usize, usize, usize) {
        let cutoff = now.checked_sub(self.activation_window).unwrap_or(now);
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }
        let total = self.samples.len();
        let motion_count = self.samples.iter().filter(|(_, m)| *m).count();
        let ratio_pct = if total > 0 { (motion_count * 100) / total } else { 0 };
        (total, motion_count, ratio_pct)
    }

    fn check_activation(&mut self, now: Instant) -> bool {
        let (total, _, ratio_pct) = self.evict_and_ratio(now);

        if total < 2 {
            return false;
        }

        let oldest = self.samples.front().unwrap().0;
        let margin = std::time::Duration::from_millis(20);
        if now.duration_since(oldest) + margin < self.activation_window {
            return false;
        }

        ratio_pct >= self.activation_ratio
    }
}

impl ActivationAlgorithm for RollingWindowAlgorithm {
    fn on_finger_down(&mut self) {
        self.reset();
    }

    fn on_finger_up(&mut self) -> Option<GlideState> {
        self.reset();
        if self.is_active {
            self.is_active = false;
            Some(GlideState::Inactive)
        } else {
            None
        }
    }

    fn on_sample(&mut self, sample: &Sample) -> Option<GlideState> {
        if self.is_active {
            return None;
        }

        let dist_sq = sample.dx * sample.dx + sample.dy * sample.dy;
        let is_motion = dist_sq >= self.motion_threshold_sq;
        self.samples.push_back((sample.timestamp, is_motion));

        if self.check_activation(sample.timestamp) {
            self.is_active = true;
            Some(GlideState::Active)
        } else {
            None
        }
    }

    fn current_motion_ratio(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let total = self.samples.len();
        let motion = self.samples.iter().filter(|(_, m)| *m).count();
        Some(motion as f64 / total as f64)
    }

    fn is_active(&self) -> bool {
        self.is_active
    }
}
