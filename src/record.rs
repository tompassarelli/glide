use serde::Serialize;

/// All JSONL record types. Tagged with "type" for easy filtering in analysis scripts.
#[derive(Serialize)]
#[serde(tag = "type")]
pub enum Record {
    #[serde(rename = "session_start")]
    SessionStart {
        timestamp_ms: f64,
        label: Option<String>,
        motion_threshold: u16,
        activation_window_ms: u64,
        activation_ratio: u16,
        device: String,
        keyboard_device: Option<String>,
    },

    #[serde(rename = "finger_down")]
    FingerDown {
        timestamp_ms: f64,
        episode_id: u64,
    },

    #[serde(rename = "finger_up")]
    FingerUp {
        timestamp_ms: f64,
        episode_id: u64,
        was_active: bool,
    },

    #[serde(rename = "sample")]
    Sample {
        timestamp_ms: f64,
        episode_id: u64,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        displacement: f64,
        is_motion: bool,
        glide_state: String,
        window_motion_ratio: Option<f64>,
        kb_presses_last_500ms: Option<u32>,
        kb_presses_last_1000ms: Option<u32>,
    },

    #[serde(rename = "episode_summary")]
    EpisodeSummary {
        episode_id: u64,
        label: Option<String>,
        start_ms: f64,
        end_ms: f64,
        duration_ms: f64,
        total_samples: u64,
        motion_samples: u64,
        motion_ratio: f64,
        total_displacement: f64,
        mean_displacement: f64,
        max_displacement: f64,
        longest_motion_run: u64,
        activated: bool,
        activation_latency_ms: Option<f64>,
        kb_presses_during: u32,
    },
}

/// Writes JSONL records to stdout, one per line.
pub struct RecordWriter {
    start: std::time::Instant,
}

impl RecordWriter {
    pub fn new(start: std::time::Instant) -> Self {
        Self { start }
    }

    pub fn ts(&self, now: std::time::Instant) -> f64 {
        now.duration_since(self.start).as_secs_f64() * 1000.0
    }

    pub fn emit(&self, record: &Record) {
        if let Ok(json) = serde_json::to_string(record) {
            println!("{json}");
        }
    }
}
