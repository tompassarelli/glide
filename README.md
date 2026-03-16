# Glide

Touchpad motion detection daemon that turns your touchpad into a modifier key. Touch the pad and move your finger — keys remap. Lift your finger — back to normal.

Glide detects intentional touchpad use and sends activation signals to [kanata](https://github.com/jtroo/kanata) over TCP, pressing a virtual key that triggers a layer switch. No touchpad grab, no cursor interference, no libinput dependency.

## How it works

1. Glide monitors your touchpad via evdev (read-only, no grab)
2. When sustained intentional motion is detected, it sends a virtual key press to kanata
3. Kanata activates a layer — your keyboard remaps (e.g. `d`→left-click, `f`→right-click)
4. When your finger lifts, glide sends a release — kanata returns to the base layer

The detection algorithm uses a consecutive motion streak: the finger must show continuous displacement for ~112ms (16 consecutive samples at ~7ms each) before activating. This filters out accidental palm contact and resting fingers while responding quickly to intentional use.

## Installation

### Linux (most distros)

```bash
# Build
git clone https://github.com/tompassarelli/glide
cd glide
cargo build --release

# Find your touchpad
sudo ./target/release/glide --list-devices

# Run (auto-detects touchpad if only one is present)
sudo ./target/release/glide

# Or specify explicitly
sudo ./target/release/glide --device /dev/input/eventX
```

If `--list-devices` shows nothing, make sure you have permission to read `/dev/input/event*` (run with sudo or add your user to the `input` group).

### NixOS (flake)

Add to your flake inputs:
```nix
glide = {
  url = "github:tompassarelli/glide";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Import the NixOS module and enable:
```nix
imports = [ glide.nixosModules.default ];

services.glide = {
  enable = true;
  device = "/dev/input/by-path/your-touchpad-event-mouse";
  # kanataAddress = "127.0.0.1:7070";  # default
  # virtualKey = "pad-touch";           # default
  # motionThreshold = 2;                # default
  # minStreak = 16;                     # default
};
```

### Kanata configuration

Kanata needs a TCP server enabled and virtual key + layer definitions:

```bash
kanata --cfg your-config.kbd --port 7070
```

In your kanata config:
```lisp
(defvirtualkeys
  pad-touch (layer-while-held pad-layer)
)

(deflayer pad-layer
  ;; your remappings here, e.g.:
  ;; d = right click, f = left click
  _    _    _    _    _    _    _    _    _    _    _    _    _    _
  _    _    _    _    _    _    _    _    _    _    _    _    _
  _    _    _    mrgt mlft _    _    _    _    _    _    _    _    _
  _    _    _    _    _    _    _    _    _    _    _    _    _
  _    _    _              _              _    _    _    _
)
```

## CLI options

```
--list-devices        List detected touchpad devices and exit
--device              Touchpad evdev path (auto-detects if omitted)
--kanata-address      Kanata TCP address (default: 127.0.0.1:7070)
--virtual-key         Virtual key name (default: pad-touch)
--motion-threshold    Min displacement per sample to count as motion (default: 2)
--min-streak          Consecutive motion samples to activate (default: 16, ~112ms)
--algorithm           streak (default) or window (legacy rolling window)
--record              Dump JSONL trace data to stdout for analysis
--label               Tag a recording session (e.g. --label intentional)
--keyboard-device     Optional keyboard evdev for context logging
```

## Data collection and tuning

Glide includes tools for collecting labeled touchpad data and analyzing it offline:

```bash
# Collect intentional touchpad use
sudo glide --record --label intentional > intentional.jsonl

# Collect accidental contact while typing
sudo glide --record --label accidental > accidental.jsonl

# Analyze separation between classes
python3 scripts/analyze.py intentional.jsonl accidental.jsonl
```

The analysis script compares episode-level features (duration, motion ratio, displacement stats, longest consecutive motion run) and identifies thresholds that separate intentional from accidental use.

## Architecture

```
┌──────────────┐    ┌──────────────────────┐    ┌─────────┐
│   Touchpad   │───▶│   ActivationAlgorithm │───▶│ Backend │
│   Sampler    │    │  (streak / window)    │    │ (kanata)│
│  (evdev)     │    │                       │    │  (TCP)  │
└──────────────┘    └──────────────────────┘    └─────────┘
       │                                              │
  FingerDown/Up              Active/Inactive      FakeKey
  Position samples           state transitions    Press/Release
```

- **Sampler**: reads evdev, produces position samples. No activation logic.
- **Algorithm**: consumes samples, emits active/inactive. Pluggable — `ConsecutiveStreakAlgorithm` (default) or `RollingWindowAlgorithm`.
- **Backend**: translates state transitions for consumers. `KanataClient` maps active→press, inactive→release over TCP.

## License

MIT
