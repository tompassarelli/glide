use evdev::{Device, EventType};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Non-blocking monitor for a keyboard evdev device.
/// Tracks recent key press timestamps for context logging.
/// Does not grab the device — read-only observation.
pub struct KeyboardMonitor {
    device: Device,
    recent_presses: VecDeque<Instant>,
}

const MAX_HISTORY: Duration = Duration::from_secs(2);

impl KeyboardMonitor {
    pub fn new(dev_path: &str) -> Result<Self, std::io::Error> {
        let device = Device::open(dev_path)?;
        log::info!(
            "opened keyboard device (read-only): {}: {:?}",
            dev_path,
            device.name().unwrap_or("unknown")
        );
        // TODO: set_nonblocking if the evdev crate version supports it.
        // For now, we rely on the touchpad event loop cadence.
        Ok(Self {
            device,
            recent_presses: VecDeque::with_capacity(128),
        })
    }

    /// Non-blocking poll for keyboard events. Call this each iteration
    /// of the main loop.
    pub fn poll(&mut self) {
        let now = Instant::now();

        // Prune old entries
        let cutoff = now.checked_sub(MAX_HISTORY).unwrap_or(now);
        while self.recent_presses.front().is_some_and(|t| *t < cutoff) {
            self.recent_presses.pop_front();
        }

        // Try to read events (non-blocking)
        match self.device.fetch_events() {
            Ok(events) => {
                for ev in events {
                    // EV_KEY with value=1 is a key press
                    if ev.event_type() == EventType::KEY && ev.value() == 1 {
                        self.recent_presses.push_back(now);
                    }
                }
            }
            Err(e) => {
                // EAGAIN (code 11) means no events available — expected for non-blocking
                if e.raw_os_error() != Some(11) && e.raw_os_error() != Some(19) {
                    log::warn!("keyboard poll error: {e}");
                }
            }
        }
    }

    /// Count key presses within the last `duration`.
    pub fn presses_in_last(&self, duration: Duration) -> u32 {
        let now = Instant::now();
        let cutoff = now.checked_sub(duration).unwrap_or(now);
        self.recent_presses.iter().filter(|t| **t >= cutoff).count() as u32
    }
}
