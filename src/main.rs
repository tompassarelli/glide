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
}

/// Internal state change from the motion detector.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GlideState {
    Active,
    Inactive,
}

/// Detects sustained touchpad motion using a rolling window of timestamped samples.
struct MotionDetector {
    finger_down: bool,
    is_active: bool,
    last_pos: Option<(i32, i32)>,
    samples: VecDeque<(Instant, bool)>,
    motion_threshold: i32,
    activation_window: std::time::Duration,
    activation_ratio: usize,
}

impl MotionDetector {
    fn new(threshold: u16, window_ms: u64, ratio: u16) -> Self {
        Self {
            finger_down: false,
            is_active: false,
            last_pos: None,
            samples: VecDeque::with_capacity(64),
            motion_threshold: i32::from(threshold),
            activation_window: std::time::Duration::from_millis(window_ms),
            activation_ratio: ratio as usize,
        }
    }

    fn reset(&mut self) {
        self.last_pos = None;
        self.samples.clear();
    }

    fn finger_down(&mut self) {
        self.finger_down = true;
        self.reset();
    }

    fn finger_up(&mut self) -> Option<GlideState> {
        self.finger_down = false;
        self.reset();
        if self.is_active {
            self.is_active = false;
            Some(GlideState::Inactive)
        } else {
            None
        }
    }

    fn position_update(&mut self, x: Option<i32>, y: Option<i32>) -> Option<GlideState> {
        if !self.finger_down || self.is_active {
            return None;
        }
        if x.is_none() && y.is_none() {
            return None;
        }

        let now = Instant::now();

        let is_motion = match self.last_pos {
            None => false,
            Some((lx, ly)) => {
                let nx = x.unwrap_or(lx);
                let ny = y.unwrap_or(ly);
                let dx = nx - lx;
                let dy = ny - ly;
                let dist_sq = dx * dx + dy * dy;
                dist_sq >= self.motion_threshold * self.motion_threshold
            }
        };

        // Update position
        match self.last_pos {
            Some((lx, ly)) => {
                self.last_pos = Some((x.unwrap_or(lx), y.unwrap_or(ly)));
            }
            None => {
                self.last_pos = Some((x.unwrap_or(0), y.unwrap_or(0)));
            }
        }

        self.samples.push_back((now, is_motion));

        // Evict old samples
        let cutoff = now.checked_sub(self.activation_window).unwrap_or(now);
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }

        // Check activation
        if self.samples.len() < 2 {
            return None;
        }

        let oldest = self.samples.front().unwrap().0;
        let margin = std::time::Duration::from_millis(20);
        if now.duration_since(oldest) + margin < self.activation_window {
            return None;
        }

        let total = self.samples.len();
        let motion_count = self.samples.iter().filter(|(_, m)| *m).count();
        let ratio = (motion_count * 100) / total;

        if ratio >= self.activation_ratio {
            self.is_active = true;
            Some(GlideState::Active)
        } else {
            None
        }
    }
}

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

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    info!("glide starting");
    info!("device: {}", args.device);
    info!("kanata: {} (virtual key: {})", args.kanata_address, args.virtual_key);
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

    let mut detector = MotionDetector::new(
        args.motion_threshold,
        args.activation_window_ms,
        args.activation_ratio,
    );

    let mut backend: Box<dyn Backend> =
        Box::new(KanataClient::new(args.kanata_address, args.virtual_key));

    loop {
        let events: Vec<_> = match device.fetch_events() {
            Ok(evs) => evs.collect(),
            Err(e) => {
                if e.raw_os_error() == Some(19) {
                    bail!("touchpad device disconnected");
                }
                warn!("fetch error: {e}");
                continue;
            }
        };

        let mut cur_x: Option<i32> = None;
        let mut cur_y: Option<i32> = None;

        for ev in &events {
            match ev.event_type() {
                EventType::KEY if ev.code() == Key::BTN_TOOL_FINGER.code() => {
                    if ev.value() != 0 {
                        detector.finger_down();
                        debug!("finger down");
                    } else {
                        if let Some(state) = detector.finger_up() {
                            info!("finger up → {state:?}");
                            backend.on_state_change(state);
                        } else {
                            debug!("finger up (was not active)");
                        }
                    }
                }
                EventType::ABSOLUTE if detector.finger_down => {
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

        if let Some(state) = detector.position_update(cur_x, cur_y) {
            info!("state change: {state:?}");
            backend.on_state_change(state);
        }
    }
}
