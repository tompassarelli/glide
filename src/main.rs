use anyhow::{Context, Result, bail};
use clap::Parser;
use evdev::{AbsoluteAxisType, Device, EventType, Key};
use log::{debug, info, warn};
use std::collections::VecDeque;
use std::io::Write;
use std::net::TcpStream;
use std::time::Instant;

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

    /// Log mode: dump CSV sample data for training/analysis instead of connecting to kanata.
    #[arg(long)]
    log_samples: bool,
}

// =============================================================================
// Layer 1: Sampling — reads evdev, produces touchpad events
// =============================================================================

/// A raw sample from the touchpad: position + displacement since last sample.
#[derive(Debug, Clone)]
struct Sample {
    timestamp: Instant,
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    displacement: f64,
}

/// Events produced by the sampler.
enum TouchpadEvent {
    FingerDown,
    FingerUp,
    Position(Sample),
}

/// Reads evdev events and produces a stream of TouchpadEvents.
/// Knows nothing about activation logic.
struct TouchpadSampler {
    finger_down: bool,
    last_pos: Option<(i32, i32)>,
}

impl TouchpadSampler {
    fn new() -> Self {
        Self {
            finger_down: false,
            last_pos: None,
        }
    }

    /// Process a batch of evdev events into touchpad events.
    fn process_events(&mut self, raw_events: &[evdev::InputEvent]) -> Vec<TouchpadEvent> {
        let mut out = Vec::new();
        let mut cur_x: Option<i32> = None;
        let mut cur_y: Option<i32> = None;

        for ev in raw_events {
            match ev.event_type() {
                EventType::KEY if ev.code() == Key::BTN_TOOL_FINGER.code() => {
                    if ev.value() != 0 {
                        self.finger_down = true;
                        self.last_pos = None;
                        out.push(TouchpadEvent::FingerDown);
                    } else {
                        self.finger_down = false;
                        self.last_pos = None;
                        out.push(TouchpadEvent::FingerUp);
                    }
                }
                EventType::ABSOLUTE if self.finger_down => {
                    match AbsoluteAxisType(ev.code()) {
                        AbsoluteAxisType::ABS_X | AbsoluteAxisType::ABS_MT_POSITION_X => {
                            cur_x = Some(ev.value());
                        }
                        AbsoluteAxisType::ABS_Y | AbsoluteAxisType::ABS_MT_POSITION_Y => {
                            cur_y = Some(ev.value());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Emit a position sample if we got any ABS data this batch
        if self.finger_down && (cur_x.is_some() || cur_y.is_some()) {
            let now = Instant::now();
            let (dx, dy) = match self.last_pos {
                Some((lx, ly)) => {
                    (cur_x.unwrap_or(lx) - lx, cur_y.unwrap_or(ly) - ly)
                }
                None => (0, 0),
            };

            let pos = match self.last_pos {
                Some((lx, ly)) => (cur_x.unwrap_or(lx), cur_y.unwrap_or(ly)),
                None => (cur_x.unwrap_or(0), cur_y.unwrap_or(0)),
            };
            self.last_pos = Some(pos);

            let displacement = ((dx * dx + dy * dy) as f64).sqrt();

            out.push(TouchpadEvent::Position(Sample {
                timestamp: now,
                x: pos.0,
                y: pos.1,
                dx,
                dy,
                displacement,
            }));
        }

        out
    }
}

// =============================================================================
// Layer 2: Activation algorithm — consumes samples, emits state transitions
// =============================================================================

/// State transitions emitted by an activation algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GlideState {
    Active,
    Inactive,
}

/// An activation algorithm consumes touchpad events and decides
/// when the touchpad is being intentionally used.
trait ActivationAlgorithm {
    fn on_finger_down(&mut self);
    fn on_finger_up(&mut self) -> Option<GlideState>;
    fn on_sample(&mut self, sample: &Sample) -> Option<GlideState>;
}

/// Rolling window algorithm: counts motion-positive samples within a time
/// window and activates when the ratio exceeds a threshold.
struct RollingWindowAlgorithm {
    is_active: bool,
    motion_threshold_sq: i32,
    activation_window: std::time::Duration,
    activation_ratio: usize,
    samples: VecDeque<(Instant, bool)>,
}

impl RollingWindowAlgorithm {
    fn new(motion_threshold: u16, window_ms: u64, ratio: u16) -> Self {
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

    fn check_activation(&mut self, now: Instant) -> bool {
        let cutoff = now.checked_sub(self.activation_window).unwrap_or(now);
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }

        if self.samples.len() < 2 {
            return false;
        }

        let oldest = self.samples.front().unwrap().0;
        let margin = std::time::Duration::from_millis(20);
        if now.duration_since(oldest) + margin < self.activation_window {
            return false;
        }

        let total = self.samples.len();
        let motion_count = self.samples.iter().filter(|(_, m)| *m).count();
        let ratio = (motion_count * 100) / total;
        ratio >= self.activation_ratio
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
}

// =============================================================================
// Layer 3: Backends — consume state transitions for external consumers
// =============================================================================

/// A backend receives glide state transitions and translates them
/// into whatever the consumer expects.
trait Backend {
    fn on_state_change(&mut self, state: GlideState);
}

/// Kanata backend: translates activation state into FakeKey press/release
/// over kanata's TCP protocol.
struct KanataClient {
    address: String,
    virtual_key: String,
    stream: Option<TcpStream>,
}

impl KanataClient {
    fn new(address: String, virtual_key: String) -> Self {
        Self {
            address,
            virtual_key,
            stream: None,
        }
    }

    fn ensure_connected(&mut self) -> Result<&mut TcpStream> {
        if self.stream.is_none() {
            info!("connecting to kanata at {}", self.address);
            let stream = TcpStream::connect(&self.address)
                .with_context(|| format!("failed to connect to kanata at {}", self.address))?;
            stream.set_nodelay(true)?;
            self.stream = Some(stream);
            info!("connected to kanata");
        }
        Ok(self.stream.as_mut().unwrap())
    }
}

impl Backend for KanataClient {
    fn on_state_change(&mut self, state: GlideState) {
        let action = match state {
            GlideState::Active => "Press",
            GlideState::Inactive => "Release",
        };

        let msg = format!(
            r#"{{"ActOnFakeKey":{{"name":"{}","action":"{}"}}}}"#,
            self.virtual_key, action
        );

        match self.ensure_connected() {
            Ok(stream) => {
                if let Err(e) = stream.write_all(msg.as_bytes()) {
                    warn!("failed to send to kanata: {e}, will reconnect");
                    self.stream = None;
                } else {
                    debug!("sent to kanata: {msg}");
                }
            }
            Err(e) => {
                warn!("kanata connection failed: {e}");
                self.stream = None;
            }
        }
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    info!("glide starting");
    info!("device: {}", args.device);
    info!(
        "detection: threshold={} window={}ms ratio={}%",
        args.motion_threshold, args.activation_window_ms, args.activation_ratio
    );

    let mut device = Device::open(&args.device)
        .with_context(|| format!("failed to open touchpad device '{}'", args.device))?;

    info!(
        "opened touchpad: {:?} (not grabbed)",
        device.name().unwrap_or("unknown")
    );

    let mut sampler = TouchpadSampler::new();

    let mut algorithm: Box<dyn ActivationAlgorithm> = Box::new(
        RollingWindowAlgorithm::new(
            args.motion_threshold,
            args.activation_window_ms,
            args.activation_ratio,
        ),
    );

    let log_samples = args.log_samples;

    let mut backend: Option<Box<dyn Backend>> = if log_samples {
        println!("# glide sample log");
        println!(
            "# threshold={} window={}ms ratio={}%",
            args.motion_threshold, args.activation_window_ms, args.activation_ratio
        );
        println!("# Ctrl+C to stop");
        println!("# timestamp_ms, event, x, y, dx, dy, displacement");
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

    let start = Instant::now();
    let ts = |now: Instant| now.duration_since(start).as_secs_f64() * 1000.0;

    loop {
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
            match event {
                TouchpadEvent::FingerDown => {
                    algorithm.on_finger_down();
                    if log_samples {
                        println!("{:10.1}, FINGER_DOWN, , , , , ", ts(Instant::now()));
                    } else {
                        debug!("finger down");
                    }
                }
                TouchpadEvent::FingerUp => {
                    if let Some(state) = algorithm.on_finger_up() {
                        if log_samples {
                            println!(
                                "{:10.1}, FINGER_UP_DEACTIVATED, , , , , ",
                                ts(Instant::now())
                            );
                        } else {
                            info!("finger up → {state:?}");
                            backend.as_mut().unwrap().on_state_change(state);
                        }
                    } else if log_samples {
                        println!("{:10.1}, FINGER_UP, , , , , ", ts(Instant::now()));
                    } else {
                        debug!("finger up (was not active)");
                    }
                }
                TouchpadEvent::Position(sample) => {
                    if log_samples {
                        let label = match algorithm.on_sample(&sample) {
                            Some(GlideState::Active) => "ACTIVATED",
                            _ => "SAMPLE",
                        };
                        println!(
                            "{:10.1}, {}, {}, {}, {}, {}, {:.1}",
                            ts(sample.timestamp),
                            label,
                            sample.x,
                            sample.y,
                            sample.dx,
                            sample.dy,
                            sample.displacement,
                        );
                    } else if let Some(GlideState::Active) = algorithm.on_sample(&sample) {
                        info!("state change: Active");
                        backend.as_mut().unwrap().on_state_change(GlideState::Active);
                    }
                }
            }
        }
    }
}
