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
