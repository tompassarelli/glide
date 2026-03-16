use crate::sampler::Sample;
use std::time::Instant;

/// Summary statistics for a completed touch episode.
#[derive(Debug, Clone)]
pub struct EpisodeSummary {
    pub id: u64,
    pub start: Instant,
    pub end: Instant,
    pub duration_ms: f64,
    pub total_samples: u64,
    pub motion_samples: u64,
    pub motion_ratio: f64,
    pub total_displacement: f64,
    pub mean_displacement: f64,
    pub max_displacement: f64,
    pub longest_motion_run: u64,
    pub activated: bool,
    /// Time from episode start to activation, if it activated.
    pub activation_latency_ms: Option<f64>,
    /// Keyboard key presses observed during this episode.
    pub kb_presses_during: u32,
}

/// Tracks the state of a single in-progress episode.
struct EpisodeState {
    id: u64,
    start: Instant,
    total_samples: u64,
    motion_samples: u64,
    total_displacement: f64,
    max_displacement: f64,
    current_motion_run: u64,
    longest_motion_run: u64,
    activated: bool,
    activation_time: Option<Instant>,
    kb_presses_during: u32,
}

/// Segments touchpad interactions into episodes and accumulates per-episode stats.
pub struct EpisodeTracker {
    next_id: u64,
    current: Option<EpisodeState>,
    motion_threshold_sq: i32,
}

impl EpisodeTracker {
    pub fn new(motion_threshold: u16) -> Self {
        let t = i32::from(motion_threshold);
        Self {
            next_id: 0,
            current: None,
            motion_threshold_sq: t * t,
        }
    }

    /// Start a new episode. Returns the episode ID.
    pub fn begin_episode(&mut self, now: Instant) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.current = Some(EpisodeState {
            id,
            start: now,
            total_samples: 0,
            motion_samples: 0,
            total_displacement: 0.0,
            max_displacement: 0.0,
            current_motion_run: 0,
            longest_motion_run: 0,
            activated: false,
            activation_time: None,
            kb_presses_during: 0,
        });
        id
    }

    /// Record a position sample into the current episode.
    /// Returns whether this sample was classified as motion.
    pub fn record_sample(&mut self, sample: &Sample) -> bool {
        let dist_sq = sample.dx * sample.dx + sample.dy * sample.dy;
        let is_motion = dist_sq >= self.motion_threshold_sq;

        if let Some(ep) = &mut self.current {
            ep.total_samples += 1;
            ep.total_displacement += sample.displacement;
            if sample.displacement > ep.max_displacement {
                ep.max_displacement = sample.displacement;
            }

            if is_motion {
                ep.motion_samples += 1;
                ep.current_motion_run += 1;
                if ep.current_motion_run > ep.longest_motion_run {
                    ep.longest_motion_run = ep.current_motion_run;
                }
            } else {
                ep.current_motion_run = 0;
            }
        }

        is_motion
    }

    /// Record that the activation algorithm triggered during this episode.
    pub fn record_activation(&mut self, now: Instant) {
        if let Some(ep) = &mut self.current {
            ep.activated = true;
            ep.activation_time = Some(now);
        }
    }

    /// Record keyboard presses observed during this episode.
    pub fn record_keyboard_presses(&mut self, count: u32) {
        if let Some(ep) = &mut self.current {
            ep.kb_presses_during += count;
        }
    }

    /// End the current episode and return its summary.
    pub fn end_episode(&mut self, now: Instant) -> Option<EpisodeSummary> {
        let ep = self.current.take()?;
        let duration_ms = now.duration_since(ep.start).as_secs_f64() * 1000.0;
        let mean_displacement = if ep.total_samples > 0 {
            ep.total_displacement / ep.total_samples as f64
        } else {
            0.0
        };
        let motion_ratio = if ep.total_samples > 0 {
            ep.motion_samples as f64 / ep.total_samples as f64
        } else {
            0.0
        };
        let activation_latency_ms = ep
            .activation_time
            .map(|t| t.duration_since(ep.start).as_secs_f64() * 1000.0);

        Some(EpisodeSummary {
            id: ep.id,
            start: ep.start,
            end: now,
            duration_ms,
            total_samples: ep.total_samples,
            motion_samples: ep.motion_samples,
            motion_ratio,
            total_displacement: ep.total_displacement,
            mean_displacement,
            max_displacement: ep.max_displacement,
            longest_motion_run: ep.longest_motion_run,
            activated: ep.activated,
            activation_latency_ms,
            kb_presses_during: ep.kb_presses_during,
        })
    }

    /// Get the current episode ID, if one is active.
    pub fn current_episode_id(&self) -> Option<u64> {
        self.current.as_ref().map(|ep| ep.id)
    }
}
