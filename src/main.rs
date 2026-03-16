mod algorithm;
mod backend;
mod episode;
mod keyboard;
mod record;
mod sampler;

use algorithm::{
    ActivationAlgorithm, ConsecutiveStreakAlgorithm, GlideState, RollingWindowAlgorithm,
};
use anyhow::{Context, Result, bail};
use backend::{Backend, KanataClient};
use clap::Parser;
use episode::EpisodeTracker;
use evdev::Device;
use log::{debug, info, warn};
use record::{Record, RecordWriter};
use sampler::{TouchpadEvent, TouchpadSampler};
use std::time::{Duration, Instant};

/// Touchpad motion detection daemon.
/// Detects intentional touchpad use via sustained motion analysis
/// and emits activation signals to consumers like kanata.
#[derive(Parser)]
#[command(name = "glide")]
struct Args {
    /// Touchpad evdev device path
    #[arg(
        short = 'd',
        long,
        default_value = "/dev/input/by-path/platform-AMDI0010:03-event-mouse"
    )]
    device: String,

    /// Kanata TCP server address (ip:port)
    #[arg(short = 'a', long, default_value = "127.0.0.1:7070")]
    kanata_address: String,

    /// Kanata virtual key name to press/release on activation
    #[arg(short = 'k', long, default_value = "pad-touch")]
    virtual_key: String,

    /// Min Euclidean displacement (device abs units) per evdev report to count as motion
    #[arg(long, default_value_t = 2)]
    motion_threshold: u16,

    /// Rolling window (ms) for evaluating motion ratio
    #[arg(long, default_value_t = 200)]
    activation_window_ms: u64,

    /// Required percentage (0-100) of motion-positive samples in the window
    #[arg(long, default_value_t = 50)]
    activation_ratio: u16,

    /// Activation algorithm: "streak" (default, experimental) or "window" (rolling window).
    #[arg(long, default_value = "streak")]
    algorithm: String,

    /// [streak] Minimum consecutive motion-positive samples to activate.
    /// At ~7ms/sample, 16 ≈ 112ms. Based on labeled data where intentional
    /// episodes had min streak 19 and accidental had max streak 13.
    #[arg(long, default_value_t = 16)]
    min_streak: u32,

    /// Record JSONL trace data to stdout for offline analysis.
    /// Disables the kanata backend.
    #[arg(long)]
    record: bool,

    /// Label for this recording session (stored in JSONL output).
    #[arg(long)]
    label: Option<String>,

    /// Keyboard evdev device for context logging (optional, read-only, no grab).
    #[arg(long)]
    keyboard_device: Option<String>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    info!("glide starting");
    info!("device: {}", args.device);
    info!("motion threshold: {}", args.motion_threshold);

    let mut device = Device::open(&args.device)
        .with_context(|| format!("failed to open touchpad device '{}'", args.device))?;

    info!(
        "opened touchpad: {:?} (not grabbed)",
        device.name().unwrap_or("unknown")
    );

    let mut kb_monitor = args.keyboard_device.as_ref().and_then(|path| {
        match keyboard::KeyboardMonitor::new(path) {
            Ok(m) => Some(m),
            Err(e) => {
                warn!("failed to open keyboard device '{path}': {e}");
                None
            }
        }
    });

    let mut sampler = TouchpadSampler::new();
    let mut algorithm: Box<dyn ActivationAlgorithm> = match args.algorithm.as_str() {
        "streak" => {
            info!(
                "algorithm: consecutive_streak (min_streak={}, threshold={})",
                args.min_streak, args.motion_threshold
            );
            Box::new(ConsecutiveStreakAlgorithm::new(
                args.motion_threshold,
                args.min_streak,
            ))
        }
        "window" => {
            info!(
                "algorithm: rolling_window (threshold={}, window={}ms, ratio={}%)",
                args.motion_threshold, args.activation_window_ms, args.activation_ratio
            );
            Box::new(RollingWindowAlgorithm::new(
                args.motion_threshold,
                args.activation_window_ms,
                args.activation_ratio,
            ))
        }
        other => {
            anyhow::bail!("unknown algorithm '{other}', expected 'streak' or 'window'");
        }
    };
    let mut episodes = EpisodeTracker::new(args.motion_threshold);

    let recording = args.record;
    let start = Instant::now();
    let writer = RecordWriter::new(start);

    let mut backend: Option<Box<dyn Backend>> = if recording {
        writer.emit(&Record::SessionStart {
            timestamp_ms: 0.0,
            label: args.label.clone(),
            algorithm: algorithm.name().to_string(),
            motion_threshold: args.motion_threshold,
            min_streak: if args.algorithm == "streak" { Some(args.min_streak) } else { None },
            activation_window_ms: if args.algorithm == "window" { Some(args.activation_window_ms) } else { None },
            activation_ratio: if args.algorithm == "window" { Some(args.activation_ratio) } else { None },
            device: args.device.clone(),
            keyboard_device: args.keyboard_device.clone(),
        });
        None
    } else {
        info!(
            "kanata: {} (virtual key: {})",
            args.kanata_address, args.virtual_key
        );
        Some(Box::new(KanataClient::new(
            args.kanata_address,
            args.virtual_key,
        )))
    };

    let label = args.label.clone();

    loop {
        // Poll keyboard (non-blocking)
        if let Some(kb) = &mut kb_monitor {
            kb.poll();
        }

        // Fetch touchpad events (blocking)
        let raw_events: Vec<_> = match device.fetch_events() {
            Ok(evs) => evs.collect(),
            Err(e) => {
                if e.raw_os_error() == Some(19) {
                    bail!("touchpad device disconnected");
                }
                warn!("fetch error: {e}");
                continue;
            }
        };

        for event in sampler.process_events(&raw_events) {
            let now = Instant::now();

            match event {
                TouchpadEvent::FingerDown => {
                    let eid = episodes.begin_episode(now);
                    algorithm.on_finger_down();

                    if recording {
                        writer.emit(&Record::FingerDown {
                            timestamp_ms: writer.ts(now),
                            episode_id: eid,
                        });
                    } else {
                        debug!("finger down (episode {})", eid);
                    }
                }

                TouchpadEvent::FingerUp => {
                    let was_active = algorithm.is_active();
                    let state_change = algorithm.on_finger_up();
                    let eid = episodes.current_episode_id().unwrap_or(0);

                    // Record keyboard presses that happened during this episode
                    if let Some(kb) = &kb_monitor {
                        episodes.record_keyboard_presses(kb.presses_in_last(Duration::from_secs(2)));
                    }

                    if let Some(summary) = episodes.end_episode(now) {
                        if recording {
                            writer.emit(&Record::EpisodeSummary {
                                episode_id: summary.id,
                                label: label.clone(),
                                start_ms: writer.ts(summary.start),
                                end_ms: writer.ts(summary.end),
                                duration_ms: summary.duration_ms,
                                total_samples: summary.total_samples,
                                motion_samples: summary.motion_samples,
                                motion_ratio: summary.motion_ratio,
                                total_displacement: summary.total_displacement,
                                mean_displacement: summary.mean_displacement,
                                max_displacement: summary.max_displacement,
                                longest_motion_run: summary.longest_motion_run,
                                activated: summary.activated,
                                activation_latency_ms: summary.activation_latency_ms,
                                kb_presses_during: summary.kb_presses_during,
                            });
                        }
                    }

                    if recording {
                        writer.emit(&Record::FingerUp {
                            timestamp_ms: writer.ts(now),
                            episode_id: eid,
                            was_active,
                        });
                    }

                    if let Some(state) = state_change {
                        if !recording {
                            info!("finger up → {state:?}");
                            backend.as_mut().unwrap().on_state_change(state);
                        }
                    } else if !recording {
                        debug!("finger up (was not active)");
                    }
                }

                TouchpadEvent::Position(sample) => {
                    let eid = episodes.current_episode_id().unwrap_or(0);
                    let is_motion = episodes.record_sample(&sample);
                    let state_change = algorithm.on_sample(&sample);

                    if state_change == Some(GlideState::Active) {
                        episodes.record_activation(now);
                    }

                    if recording {
                        writer.emit(&Record::Sample {
                            timestamp_ms: writer.ts(sample.timestamp),
                            episode_id: eid,
                            x: sample.x,
                            y: sample.y,
                            dx: sample.dx,
                            dy: sample.dy,
                            displacement: sample.displacement,
                            is_motion,
                            glide_state: if algorithm.is_active() {
                                "active".into()
                            } else {
                                "inactive".into()
                            },
                            window_motion_ratio: algorithm.current_motion_ratio(),
                            kb_presses_last_500ms: kb_monitor
                                .as_ref()
                                .map(|kb| kb.presses_in_last(Duration::from_millis(500))),
                            kb_presses_last_1000ms: kb_monitor
                                .as_ref()
                                .map(|kb| kb.presses_in_last(Duration::from_millis(1000))),
                        });
                    }

                    if let Some(state) = state_change {
                        if !recording {
                            info!("state change: {state:?}");
                            backend.as_mut().unwrap().on_state_change(state);
                        }
                    }
                }
            }
        }
    }
}
