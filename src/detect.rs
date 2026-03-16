use evdev::{AbsoluteAxisType, Device, Key};
use std::path::PathBuf;

/// A candidate touchpad device found by scanning /dev/input.
pub struct TouchpadCandidate {
    pub path: String,
    pub name: String,
}

impl std::fmt::Display for TouchpadCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {:?}", self.path, self.name)
    }
}

/// Check if a device looks like a touchpad based on capabilities.
/// A touchpad typically has:
///   - BTN_TOOL_FINGER (contact detection)
///   - ABS_X + ABS_Y (absolute position)
///   - is NOT named "kanata" (virtual device)
fn is_touchpad(device: &Device) -> bool {
    let name = device.name().unwrap_or("");
    if name == "kanata" {
        return false;
    }

    let has_finger = device
        .supported_keys()
        .is_some_and(|keys| keys.contains(Key::BTN_TOOL_FINGER));

    let has_abs_xy = device.supported_absolute_axes().is_some_and(|axes| {
        axes.contains(AbsoluteAxisType::ABS_X) && axes.contains(AbsoluteAxisType::ABS_Y)
    });

    has_finger && has_abs_xy
}

/// Scan /dev/input/event* for devices that look like touchpads.
pub fn find_touchpads() -> Vec<TouchpadCandidate> {
    let mut candidates = Vec::new();

    for (path, device) in evdev::enumerate() {
        if is_touchpad(&device) {
            candidates.push(TouchpadCandidate {
                path: path.to_string_lossy().to_string(),
                name: device.name().unwrap_or("unknown").to_string(),
            });
        }
    }

    candidates
}

/// Print all touchpad candidates to stdout.
pub fn list_devices() {
    let candidates = find_touchpads();
    if candidates.is_empty() {
        println!("No touchpad devices found.");
        println!();
        println!("Make sure you have permission to read /dev/input/event* (try running with sudo).");
    } else {
        println!("Touchpad devices found:");
        println!();
        for c in &candidates {
            println!("  {c}");
        }
        println!();
        println!("Use: glide --device <path>");
    }
}

/// Auto-detect a touchpad device. Returns the path if exactly one is found.
/// Returns an error with a helpful message if zero or multiple are found.
pub fn autodetect() -> anyhow::Result<String> {
    let candidates = find_touchpads();
    match candidates.len() {
        0 => {
            anyhow::bail!(
                "No touchpad device found.\n\
                 Make sure you have permission to read /dev/input/event* (try running with sudo).\n\
                 Or specify a device explicitly: glide --device /dev/input/eventX\n\
                 To list candidates: glide --list-devices"
            );
        }
        1 => {
            let c = &candidates[0];
            log::info!("auto-detected touchpad: {} ({})", c.path, c.name);
            Ok(c.path.clone())
        }
        _ => {
            let mut msg = String::from("Multiple touchpad devices found:\n\n");
            for c in &candidates {
                msg.push_str(&format!("  {c}\n"));
            }
            msg.push_str("\nPlease specify one: glide --device <path>");
            anyhow::bail!(msg);
        }
    }
}
