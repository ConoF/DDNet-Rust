# DDNet 20.2.1: Rust compatibility ledger

**Reference version:** https://github.com/ddnet/ddnet/tree/20.2.1

**Reference SHA:** `176fc2f9ab85ace703c6ec10da0249b01995ba94`

The original C++ code is pinned at `upstream/ddnet` as a Git submodule.
It is for comparing behavior and compiling independent reference tests.
The Rust executable does not use any C++ code or FFI.

| Subsystem | Actual state | Checks |
|---|---|---|
| Original stable source | Pinned | Exact Git commit |
| Rust crate/CLI | Initial scaffold | Build, smoke test in CI |
| Signed Teeworlds variable integers | Implemented | Reference vectors, 165k+ round trips, C++ differential test |
| UTF-8 NUL-terminated strings | Partial, strict | Invalid-input and bounds tests |
| Teeworlds datafile v3/v4 metadata and item/block indexing | Partial | Synthetic fixtures, truncation/fuzz simulation and real DDNet map files |
| v4 zlib-compressed data blocks | Implemented, bounded | zlib round trips, corruption and size-limit tests |
| Vanilla game layer tiles and basic solid/ground queries | Partial | synthetic simulations and real-map CI smoke tests |
| 0.6 packet and chunk header packing/validation | Partial | Fixed vectors, roundtrips, rejected malformed inputs |
| Offline 10-bit vital ACK/sequence window | Partial | Loss, duplication, reordering and wraparound simulations |
| UDP 0.6 transport CONNECT/ACCEPT, security token and bounded resend model | Partial | Real localhost UDP handshake, malformed packet tests |
| DDNet 0.6 system message packer and CRC-checked map chunks | Partial | Golden vectors, chunk replay, CRC32 and invalid maps |
| Native DDNet static Huffman packet codec | Implemented as isolated module | Randomized roundtrips and byte-for-byte C++ differential CI |
| 0.7 networking, complete timed resend, snapshots and gameplay session | Not implemented | None |
| Gravity, directional movement, ground/air jump | Partial | 50 Hz tick-state simulation tests |
| Velocity ramp and projectile position | Math functions only | Unit tests |
| Basic game tiles, ground tests, MovePoint / MoveBox | Partial | Synthetic collision, wall, ground, corner and bounded-motion simulations |
| Front/tele/speedup/switch/tune layer extraction | Partial | Type/offset tests and official map CI fixtures |
| Front/switch/tune/teleport gameplay logic and full collision | Not implemented | None |
| Hook, player interactions, full prediction | Not implemented | None |
| UDP 0.6 handshake probe (not game login) | Partial | Loopback UDP mock server; public server compatibility unverified |
| Snapshot packet headers and multipart reassembly | Partial | Truncation, duplicate, out-of-order 900-byte pieces, 64-part cap |
| Complete delta snapshot decoding and playable game state | Not implemented | None |
| Map downloads, snapshot-aware headless UDP session and NETMSG_INPUT | Partial | Mock server, CRC and input packet tests |
| Pure Rust CPU debug framebuffer for map and tee snapshot overlays | Implemented as diagnostic module | Pixel-level tests, real upstream map PPM regression |
| Native offline window and experimental online snapshot viewer | Partial | Linux Xvfb, Windows build, simulated 50 Hz input; no stable public-server test |
| Pure Rust stereo PCM mixer, looping and positional sound | Partial | Six unit tests, WAV smoke; no audio device or asset decoders |
| Full textured graphics, UI, audio output and asset decoding | Not implemented | None |
| Server browser, gameplay, editor and demos | Not implemented | None |

Passing CI here **does not** mean the game itself is operational.

## Required porting order

1. Keep stable C++ 20.2.1 as a reproducible source of truth.
2. Finish network reliability, Huffman, tokens, handshake and generated net messages;
   test against actual server traces and malformed packets.
3. Finish v4 zlib decompression and DDNet map/tile/layer decoding; then collision,
   movement restrictions, freeze, tune zones and full deterministic character core.
4. Implement an actual Rust game loop, asset manager, renderer, windowing and audio.
5. Connect to vanilla DDNet servers and compare gameplay snapshot-by-snapshot.
6. Port the remaining UI, server browser, demos, downloads, settings and editor.
7. Run end-to-end native compatibility tests on Linux and Windows, regression
   tests for every DDNet feature and FPS/latency/memory benchmarks.

## Commands

```sh
git clone --recurse-submodules https://github.com/ConoF/DDNet-Rust.git
cd DDNet-Rust
git -C upstream/ddnet rev-parse HEAD
cargo test --locked
cargo run -- --self-test
cargo run -- --inspect-map upstream/ddnet/data/maps/ctf1.map
```

The Linux CI compiles a small C++ test from the **original DDNet header** and
compares its integer encoding output byte-for-byte with the Rust implementation.
This is a limited differential test, not a full client simulation.

> Important: a transport-level handshake is only the first part of DDNet's login. No rendering, player session or full compatibility has been implemented.

Headless CLI: `cargo run --locked -- --connect-headless 127.0.0.1:8303` can test transport, map acquisition and wait for the first snapshot. This does not provide graphics or actual gameplay.

Login order is checked against original `src/game/client/gameclient.cpp`: `CON_READY` -> `Cl_StartInfo` -> `Sv_ReadyToEnter` -> `ENTERGAME`. A mock protocol server tests this order; public-server compatibility is not yet established.

Collision-only visualization: `cargo run --locked -- --render-debug-map upstream/ddnet/data/maps/ctf1.map target/ctf1-debug.ppm`. This PPM is not the vanilla DDNet graphical renderer.

## Optional offline desktop window

Use `cargo run --features window -- --play-offline upstream/ddnet/data/maps/ctf1.map`. Keys: A/D/arrows move, W/Space jump, E/right mouse hook, wheel zoom, R respawn, Escape exit. This is only a collision-map movement prototype: it does not show vanilla DDNet graphics, support live gameplay, editor, audio or networking.

## Experimental online snapshot window

Run `cargo run --features window -- --play-online HOST:8303` for a DDNet 0.6 server. The window uses Rust UDP handshake, map transfer, snapshot deltas and sends neutral/keyboard input at 50Hz. This is experimental and has not been verified against a public server; it is **not** a feature-complete or stable DDNet client. Key bindings match the offline viewer. Only collision tiles and abstract tee markers are rendered.

## Audio diagnostics

The Rust-only PCM engine ports the core stereo mix, master/channel volume, mono/stereo handling, loop transition and basic circular/rectangular spatial attenuation from DDNet 20.2.1. Run `cargo run --locked -- --audio-smoke target/audio-smoke.wav` to export a synthetic 440 Hz sound. The output is a test waveform, not DDNet's original audio assets. Audio hardware playback, Opus/WavPack decoding, real-time callbacks and parity across all DDNet edge cases are still outstanding.

## Network snapshot display interpolation

The experimental online window now blends character/hook coordinates between consecutive decoded 0.6 snapshots. Large position changes (over 256 world pixels), missing objects and hook transitions snap immediately. This does **not** modify server snapshots, prediction or input acknowledgements. The smoothing is diagnostic and is not yet a verified port of DDNet's full interpolation/prediction implementation.

## Reliable transport maintenance (audit step 2)

The native 0.6 transport now retries its **oldest** unacknowledged vital
chunk after one second even without an explicit peer RESEND request. A
15-second unacknowledged limit closes this prototype connection; upstream
DDNet has a **configurable** connection timeout instead. The online window
and headless CLI poll this retransmission timer. ACKed chunks are removed
from the queue without duplicate inserts; sequence wraparound is covered
by deterministic simulated-clock tests. Full reconnection, ACK recovery
and packet timing parity with upstream are not yet complete.

### Connection recovery policy

The online diagnostic window now starts a **new** 0.6 handshake after a
15-second unacknowledged vital timeout or 20 seconds without a **valid**
packet. The recovery clears pending ACKs, map transfer and decoded snapshot
history, preserving login settings; input tick/ACK counters also reset.
Up to three consecutive reconnect attempts are allowed, with the counter
reset after a decoded snapshot. Explicit server disconnect/kick messages do
not trigger automatic reconnect. This remains prototype recovery, not
full DDNet failover or timeout protection.

## Stage 3 / movement physics audit, part 1

The local 50 Hz simulator now implements the original `CCharacterCore::Move`
horizontal `VelocityRamp` around `MoveBox`: adjust horizontal velocity
for collision movement, then undo the factor so subsequent ticks retain
the underlying velocity. The default tuning values are 550/2000/1.4.
Configurable ground/wall elasticity now reaches `MoveBox`, and bouncy
ground contact resets the consumed air-jump bit like the original
`CCharacterCore::Move`. Synthetic tests cover fast movement, disabling
the ramp, bouncy landing and invalid tuning. Complete physics parity
requires follow-on work on interaction, switches, teleports and player
prediction; these tests do not establish full game-level equivalence.

## Stage 3 / hook physics audit, part 2

Verified the isolated static-tile hook against DDNet 20.2.1
\`CCharacterCore::Tick\` and \`IntersectLineTeleHook\` for two edge cases:
zero target input still launches a stationary flying hook, and exceeding
the configured hook length changes the state to retract **without**
advancing the hook tip to the clamped endpoint, even if a solid wall
lies at that endpoint. Three regression tests cover launch, the 380px
range cutoff and boundary-wall behavior. This remains *partial* physics:
hooking other characters, teleporting hooks, through/front tiles and
DDNet-specific movement restrictions are still unimplemented.

## Stage 3.3 — hooking a second character (partial)

\`Hook::tick_with_players\` accepts a bounded, validated list of visible
character cores (0..63 slots), with external team/solo/super eligibility.
The hook chooses the nearest eligible target along the map-truncated
segment and gives player intersection priority over the wall, mirroring
\`CCharacterCore::Tick\`. It then tracks the hooked tee's position,
releases on disappearance or permission loss, and applies the 60-tick
player-only timeout. Separate \`Hook::apply_player_drag\` implements the
two-body componentwise saturated drag of \`TickDeferred\`, without
disabling the existing single-tee static-wall simulator.

Deliberate limits: there is no owned \`CWorldCore\` yet, so no actual
multiplayer tick orchestration, full team collision restrictions,
telehook behavior, or player-player collision response. Those need
a multi-core world integration rather than pretending a single-tee
simulator provides complete hooking.
