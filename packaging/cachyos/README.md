# DDNet-Rust for CachyOS (experimental native build)

This is an early preview of the DDNet 20.2.1 Rust port.
**It is NOT a complete replacement for DDNet yet.**

## Installation

Unpack the outer GitHub Actions artifact ZIP first, then:

    tar -xzf ddnet-rust-cachyos-x86_64.tar.gz
    cd ddnet-rust-cachyos-x86_64

Run the offline physics playground:

    ./run.sh --offline

Open the experimental official DDNet 0.6 server browser:

    ./run.sh

The master browser can list servers, but full multiplayer
compatibility is NOT yet implemented or verified.

Other useful commands:

    ./run.sh --self-test
    ./run.sh --settings
    ./run.sh --inspect-demo path/to/file.demo
    ./run.sh --play-demo path/to/file.demo
    ./run.sh --help

Controls in offline mode: A/D walk, W or Space jump, right mouse
or E hook, 1-6 change cosmetic weapon, left mouse attack animation,
mouse wheel zoom, R respawn, Escape exit.

## Libraries and display on CachyOS

    sudo pacman -S --needed alsa-lib libx11 libxext libxrandr libxinerama libxcursor

The window backend uses X11; under Wayland, ensure XWayland is
available. Audio is optional when there is no usable output device.
The binary is linked against glibc and system libraries. It was
built on Ubuntu 24.04 x86_64 and validated on Ubuntu/Xvfb, NOT
tested on a physical CachyOS installation.

This bundle includes original resources from upstream DDNet 20.2.1
with upstream licensing and attribution. See LICENSE-DDNET.txt,
LICENSE-RUST.txt and data/maps/license.txt.

Demo controls: Space pause/resume; Left/Right frame step; PageUp/PageDown +10/-10 seconds; Up/Down speed; Home/End beginning/end; click timeline to seek; mouse wheel zoom; R restart; Esc quit. Audio replay is not yet implemented.


## Hardware GPU backend (experimental)

This package is compiled with `--features gpu`: its online and offline
game map layers use native wgpu (Vulkan first, with other supported backends).
Run `./run.sh --play-online HOST:PORT` or `./run.sh --play-offline data/maps/ctf1.map`.
The client automatically falls back to the software renderer if GPU
initialization fails. To force the software renderer for comparison:

```bash
DDNET_RENDERER=software ./run.sh --play-online HOST:PORT
```

The game still has CPU-rendered tee sprites and HUD uploaded as
separate transparent overlays; this is not the original DDNet GPU
sprite/GUI implementation. The original GPU-render smoke is checked in
CI using `--gpu-offline-smoke data/maps/ctf1.map`.


### Esc menu

Press `Esc` during play to open Game / Players / Server info / Settings.
Mouse and keyboard navigation are supported; `Esc` resumes play.
General settings cover chat and nameplates, Player updates your name,
Graphics sets FPS/zoom, and Sound toggles sounds. The settings persist
to `~/.config/ddnet-rust/settings.json` on Linux (or the appropriate
XDG config directory). Exact original DDNet fonts, all skins and
all menu pages have not yet been ported.


### Original DDNet font assets

The package ships the original DDNet 20.2.1
`data/fonts/DejaVuSans.ttf`. The UI now builds a native reusable
TrueType glyph atlas with proportional Latin, Cyrillic and punctuation
glyphs; native GPU and software fallback share metrics. If the
font asset is missing, the client falls back to the older ASCII bitmap.
