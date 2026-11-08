# DDNet-Rust

A clean-room, **Rust-first port of the official DDNet client**, targeting gameplay, network and asset compatibility with upstream DDNet. This is **not** CFX Client and does not add gameplay modifications or cheats.

**Upstream baseline:** [DDNet 20.2.1](https://github.com/ddnet/ddnet/tree/20.2.1) (2026-10-27), commit `176fc2f9ab85ace703c6ec10da0249b01995ba94`.

## Status

**Under development; not a fully playable DDNet client.** An offline preview and a native server browser run, but live public-server gameplay is experimental. Do not mistake a successful `cargo test` for full DDNet compatibility.

## Download a CachyOS/Linux x86_64 preview

The **CachyOS/Linux x86_64 Preview** GitHub Actions workflow
automatically compiles an x86_64 native binary and packages it with
the original DDNet 20.2.1 textures, a demo map, default skin and audio.

1. Open Actions -> **CachyOS/Linux x86_64 Preview** -> most recent
   successful run -> **ddnet-rust-cachyos-x86_64** artifact.
2. Download and extract the artifact ZIP; inside it is
   **ddnet-rust-cachyos-x86_64.tar.gz** plus a SHA256 checksum.
3. On CachyOS:

\`\`\`bash
tar -xzf ddnet-rust-cachyos-x86_64.tar.gz
cd ddnet-rust-cachyos-x86_64
./run.sh --offline
\`\`\`

To open the experimental browser: \`./run.sh\`.
To inspect a .demo header: \`./run.sh --inspect-demo FILE.demo\`.
To view a recording: \`./run.sh --play-demo FILE.demo\`.
See bundled README-CACHYOS.md for dependencies and controls.

No local Rust compiler, Git clone or manual asset downloads required.
**This preview does not guarantee connection to public DDNet servers.**
The engine still lacks parts of DDNet physics, rendering and protocol.

## Checkout

```sh
git clone --recurse-submodules https://github.com/ConoF/DDNet-Rust.git
cd DDNet-Rust
cargo test --locked
cargo run -- --self-test
cargo run -- --inspect-game-map upstream/ddnet/data/maps/ctf1.map
```

`upstream/ddnet` is a **pinned Git submodule** containing the unmodified official C++ source of DDNet 20.2.1. It is a reference and compatibility oracle, **not linked into the Rust executable**. Use `git submodule update --init` to fetch it after an existing clone.

## Project rules

- Keep the behavior and defaults of vanilla DDNet. No CFX modules.
- Use Rust for all new gameplay and client code, with no C++ runtime dependency in the final client.
- Compare each ported subsystem against the pinned upstream C++ baseline with reference vectors, differential tests and integration tests before claiming compatibility.
- Record implemented, partially implemented and missing functionality explicitly in [docs/PORTING.md](docs/PORTING.md).
- Preserve the attribution and licenses of all upstream sources and assets.

The original project is [DDraceNetwork/DDNet](https://github.com/ddnet/ddnet), maintained by its respective contributors. This project is an independent, incomplete Rust port and is not an official DDNet release.

## Ported components (not a playable client)

- Bounded DATA v3/v4 map reader and zlib decompression, with real upstream
  fixture checks.
- Tilemap game layer extraction, vanilla solid/nohook detection, grounded
  checks, collision stepping in `MoveBox` and `MovePoint`.
- Teeworlds signed integer network codec, 0.6 packet/chunk header codecs,
  and offline 10-bit vital sequence / ACK simulation.

The project has since gained an experimental 0.6 networking window,
offline 50 Hz prototype, CPU texture renderer, audio and server browser.
These are not equivalent to a finished client. Remaining work is
tracked in [docs/PORTING.md](docs/PORTING.md).


## Experimental online gameplay controls (0.6)

To join a server, use `./run.sh --play-online HOST:PORT` from the CachyOS
preview archive. The server browser also offers 0.6 server selection.

- **A/D** or arrows: move, **W/Space**: jump.
- **Left mouse**: fire, **right mouse / E**: hook.
- **1–5**: hammer, gun, shotgun, grenade, laser; mouse wheel changes weapon.
- **+/-**: camera zoom.
- **T**: public chat, **Y**: team chat, Enter to send, Esc to cancel.
- **Tab**: hold to view player scoreboard.
- **F7**: spectate, **F8**: join, **F10**: normal kill/respawn.
- **Esc** outside chat: leave the game window.

Chat is sent as UTF-8, but the current bitmap HUD displays non-Latin
glyphs as fallback characters. Live gameplay remains experimental and does
not yet provide parity with original DDNet physics and server extensions.


### Optional GPU map renderer

`cargo run --release --features gpu -- --play-online HOST:PORT` enables
native wgpu GPU map drawing (Vulkan/DX12/Metal/GLES depending on OS).
The prebuilt CachyOS archive includes this feature. Map tile/quad
geometry is batched, culled against the camera and drawn from VRAM-resident
atlas textures. This is **GPU presentation to the real window**, not a
GPU-to-CPU PNG preview. CPU tee sprites and UI are uploaded as RGBA
overlays until their own GPU paths are ported.

If no GPU adapter is available, the client falls back to CPU rendering.
Force CPU for A/B comparison using `DDNET_RENDERER=software`.
No measured FPS improvement is claimed without a same-machine benchmark.
