use evdev::{AbsoluteAxisType, EventType, Key};
use std::time::Instant;

/// A raw sample from the touchpad: position + displacement since last sample.
#[derive(Debug, Clone)]
pub struct Sample {
    pub timestamp: Instant,
    pub x: i32,
    pub y: i32,
    pub dx: i32,
    pub dy: i32,
    pub displacement: f64,
}

/// Events produced by the sampler.
pub enum TouchpadEvent {
    FingerDown,
    FingerUp,
    Position(Sample),
}

/// Reads evdev events and produces a stream of TouchpadEvents.
/// Knows nothing about activation logic.
pub struct TouchpadSampler {
    pub finger_down: bool,
    last_pos: Option<(i32, i32)>,
}

impl TouchpadSampler {
    pub fn new() -> Self {
        Self {
            finger_down: false,
            last_pos: None,
        }
    }

    /// Process a batch of evdev events into touchpad events.
    pub fn process_events(&mut self, raw_events: &[evdev::InputEvent]) -> Vec<TouchpadEvent> {
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

        if self.finger_down && (cur_x.is_some() || cur_y.is_some()) {
            let now = Instant::now();
            let (dx, dy) = match self.last_pos {
                Some((lx, ly)) => (cur_x.unwrap_or(lx) - lx, cur_y.unwrap_or(ly) - ly),
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
