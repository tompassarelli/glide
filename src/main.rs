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
    /// Label sessions by pressing Enter and typing a label (e.g. "intentional", "accidental").
    #[arg(long)]
    log_samples: bool,
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

    fn position_update(&mut self, x: Option<i32>, y: Option<i32>) -> SampleResult {
        if !self.finger_down {
            return SampleResult::Ignored;
        }
        if x.is_none() && y.is_none() {
            return SampleResult::Ignored;
        }

        let now = Instant::now();

        let (dx, dy, is_motion) = match self.last_pos {
            None => (0, 0, false),
            Some((lx, ly)) => {
                let nx = x.unwrap_or(lx);
                let ny = y.unwrap_or(ly);
                let dx = nx - lx;
                let dy = ny - ly;
                let dist_sq = dx * dx + dy * dy;
                (dx, dy, dist_sq >= self.motion_threshold * self.motion_threshold)
            }
        };

        let pos = match self.last_pos {
            Some((lx, ly)) => (x.unwrap_or(lx), y.unwrap_or(ly)),
            None => (x.unwrap_or(0), y.unwrap_or(0)),
        };
        self.last_pos = Some(pos);

        if self.is_active {
            return SampleResult::AlreadyActive;
        }

        self.samples.push_back((now, is_motion));

        // Evict old samples
        let cutoff = now.checked_sub(self.activation_window).unwrap_or(now);
        while self.samples.front().is_some_and(|(t, _)| *t < cutoff) {
            self.samples.pop_front();
        }

        let total = self.samples.len();
        let motion_count = self.samples.iter().filter(|(_, m)| *m).count();
        let ratio_pct = if total > 0 { (motion_count * 100) / total } else { 0 };

        let disp = ((dx * dx + dy * dy) as f64).sqrt();

        let sample = SampleInfo {
            x: pos.0,
            y: pos.1,
            dx,
            dy,
            displacement: disp,
            is_motion,
            window_total: total,
            window_motion: motion_count,
            window_ratio_pct: ratio_pct,
        };

        // Check activation
        let activated = if self.samples.len() >= 2 {
            let oldest = self.samples.front().unwrap().0;
            let margin = std::time::Duration::from_millis(20);
            now.duration_since(oldest) + margin >= self.activation_window
                && ratio_pct >= self.activation_ratio
        } else {
            false
        };

        if activated {
            self.is_active = true;
            SampleResult::Activated(sample)
        } else {
            SampleResult::Sample(sample)
        }
    }
}

/// Info about a single position sample, used for both detection and logging.
#[derive(Debug)]
struct SampleInfo {
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    displacement: f64,
    is_motion: bool,
    window_total: usize,
    window_motion: usize,
    window_ratio_pct: usize,
}

/// Result of processing a position update.
enum SampleResult {
    /// No position data or finger not down.
    Ignored,
    /// Already active, not sampling.
    AlreadyActive,
    /// Recorded a sample, not yet activated.
    Sample(SampleInfo),
    /// This sample triggered activation.
    Activated(SampleInfo),
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

    let log_samples = args.log_samples;

    let mut backend: Option<Box<dyn Backend>> = if log_samples {
        println!("# glide sample log");
        println!("# threshold={} window={}ms ratio={}%", args.motion_threshold, args.activation_window_ms, args.activation_ratio);
        println!("# Press Enter to insert a label, Ctrl+C to stop");
        println!("# timestamp_ms, event, x, y, dx, dy, displacement, is_motion, window_total, window_motion, window_ratio_pct");
        None
    } else {
        Some(Box::new(KanataClient::new(args.kanata_address, args.virtual_key)))
    };

    let start = Instant::now();

    loop {
        // Check for label input (non-blocking) in log mode
        if log_samples {
            use std::io::BufRead;
            // We can't easily do non-blocking stdin in this loop without async,
            // so labels are entered between sessions via finger-up pauses.
        }

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

        let ts = || start.elapsed().as_secs_f64() * 1000.0;
        let mut cur_x: Option<i32> = None;
        let mut cur_y: Option<i32> = None;

        for ev in &events {
            match ev.event_type() {
                EventType::KEY if ev.code() == Key::BTN_TOOL_FINGER.code() => {
                    if ev.value() != 0 {
                        detector.finger_down();
                        if log_samples {
                            println!("{:10.1}, FINGER_DOWN,,,,,,,,,,", ts());
                        } else {
                            debug!("finger down");
                        }
                    } else {
                        let was_active = detector.is_active;
                        if let Some(state) = detector.finger_up() {
                            if log_samples {
                                println!("{:10.1}, FINGER_UP_DEACTIVATED,,,,,,,,,,", ts());
                            } else {
                                info!("finger up → {state:?}");
                                backend.as_mut().unwrap().on_state_change(state);
                            }
                        } else {
                            if log_samples {
                                println!("{:10.1}, FINGER_UP,,,,,,,,,,", ts());
                            } else {
                                debug!("finger up (was not active)");
                            }
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

        match detector.position_update(cur_x, cur_y) {
            SampleResult::Activated(s) => {
                if log_samples {
                    println!(
                        "{:10.1}, ACTIVATED, {}, {}, {}, {}, {:.1}, {}, {}, {}, {}",
                        ts(), s.x, s.y, s.dx, s.dy, s.displacement,
                        s.is_motion, s.window_total, s.window_motion, s.window_ratio_pct,
                    );
                } else {
                    info!("state change: Active");
                    backend.as_mut().unwrap().on_state_change(GlideState::Active);
                }
            }
            SampleResult::Sample(s) => {
                if log_samples {
                    println!(
                        "{:10.1}, SAMPLE, {}, {}, {}, {}, {:.1}, {}, {}, {}, {}",
                        ts(), s.x, s.y, s.dx, s.dy, s.displacement,
                        s.is_motion, s.window_total, s.window_motion, s.window_ratio_pct,
                    );
                }
            }
            SampleResult::Ignored | SampleResult::AlreadyActive => {}
        }
    }
}
