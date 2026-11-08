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

## Stage 3.4: bounded multi-character world simulation

A new `src/world.rs` coordinates up to 64 clients with a shared 50 Hz
tick. It runs the tick phase for all players, applies directional
`CCharacterCore::TickDeferred` player pushes and hooked-player drag,
then performs map `MoveBox` integration and sampled tee blocking in
client-ID order. The input API is keyed by client ID and rejects
duplicates/unknown IDs without partially committing a tick. Physics
simulator tick/move phases are reusable by both the isolated and
multi-character paths. Six synthetic regression tests cover tick order,
hook influence, pushing, team/solo restrictions and transactionality.

**Limitations:** This is an offline simulation, not a real 64-client game
world. DDNet's `CTeamsCore` collision logic, `m_MoveRestrictions`, input
freezing/teleport/tune layers, deterministic server reconciliation and
the entire network snapshot prediction loop remain incomplete. The
simple same-team and solo rule is an approximation, not a full port of
DDNet team semantics.

## Stage 4 — actual texture loading and map tile images (first increment)

\`src/texture.rs\` provides a bounded **pure-Rust PNG decoder** for
noninterlaced 8-bit grayscale, RGB, indexed, gray-alpha and RGBA images.
It handles PNG row filters 0-4, palette transparency and chunk CRC
validation via the already-pinned \`flate2\` Rust backend. \`src/map_visual.rs\`
loads original \`mapres/*.png\` images and uncompressed embedded RGBA
map images from official DDNet v3/v4 datafiles. The preview renders
ordinary tile layers with texture atlases (16x16), tile flags, group
offset/parallax, static tint and alpha blending to the CPU framebuffer.

Try:
\`cargo run --locked -- --render-textured-png upstream/ddnet/data/maps/ctf1.map upstream/ddnet/data target/ctf1-textured.png\`

Linux CI opens the original upstream map and mapres images and uploads
a PNG artifact. This is *not* the final DDNet renderer: quads, animated
envelopes, decorative details, lighting, GPU batching, and full
per-layer mapping need additional porting. Physics and protocol remain
independent from this visual subsystem.

## Stage 4 — textured game windows and default tee skin

Both the offline 50 Hz native window and the experimental online 0.6
window now use \`VisualMap\` image-backed map layers if the DDNet
\`data/\` resources are available; otherwise they preserve the
collision-color fallback. The downloaded map retains bounded, CRC
verified DATA bytes so the online window can load embedded visual
textures rather than losing them after collision decoding. The offline
window derives its asset root from the map path; online can also use
\`DDNET_DATA_DIR\`, \`data/\`, or \`upstream/ddnet/data/\`.

The snapshot renderer additionally uses the original
\`skins/default.png\` spritesheet (8x4 grid from upstream
\`datasrc/content.py\`) to paint static body, eyes and feet, with
hook lines behind the tee. This is a simplified skin composition,
not the exact upstream animated character render; player-specific skins,
eye emotes, shadows and foot animation remain to be ported.

The Xvfb offline window smoke test now requires both the original map
textures and default skin, so it cannot silently pass after reverting
to diagnostic rectangles. Native Windows still runs the complete Rust
build, tests and Clippy, without an interactive Windows smoke test.

## Stage 4 / sprite-driven character motion, hook and weapons

The shared CPU compositor \`src/sprite_render.rs\` now uses the original
DDNet 20.2.1 sprite coordinates from \`datasrc/content.py\` for
\`skins/default.png\` (8x4) and \`game.png\` (32x16).
It draws hooked chain segments and hook head, selects all six weapon
body sprites from the full 0.6 Character netobject, rotates weapons by
the captured player aim angle, plays recent-attack muzzle/recoil, turns
eyes, and moves feet from tick-driven gait and horizontal velocity.

The offline sandbox exposes **1..6** for cosmetic weapon selection and
left mouse for a cosmetic attack preview. Combat remains unimplemented.
The online 0.6 window reads real weapon/emote/attack tick snapshots.
Both windows continue to fall back to diagnostic graphics if original
resources are absent. Unit tests validate weapon atlas selection,
hook chain/head, deterministic gait, recent attacks and extreme inputs.

This is a simple tick-driven **visual approximation**, NOT the complete
upstream \`CAnimState\` / walk-jump animation blend, dynamic player
skins, named emotes, weapon sounds, or sprite batching.

## Stage 5 — native Rust WavPack sound-effect decoding (first increment)

\`src/sound_assets.rs\` now decodes the original \`data/audio/*.wv\`
(WavPack) DDNet 20.2.1 effects via the pure Rust \`wavicle\` 0.1.0
decoder, with explicitly bounded compressed file sizes, block
sample-count preflight, 16-bit mono/stereo restrictions, rate validation,
CRC decoding and resampling. It maps 15 canonical DDNet sound sets by
their original static names; \`SoundBank\` loads and caches decoded sound
samples on demand and can schedule spatial effects in \`AudioMixer\`.
Resampling is nearest neighbor, matching upstream's
\`CSound::RateConvert\` algorithm instead of inventing higher quality
but incompatible sample math.

\`cargo run --locked -- --audio-wv-preview upstream/ddnet/data/audio/wp_gun_fire-01.wv target/gun-fire.wav\`
exports a real DDNet gunshot to 48kHz stereo WAV using the Rust mixer.
Linux CI decodes original weapon and hook effects, verifies WAV metadata,
and still tests the synthetic sample output. The \`wavicle\` dependency
raises MSRV to Rust 1.87 because it uses std integer divisibility APIs.

**Remaining:** no OS audio output device has been wired yet. The CLI
WAV output is genuine original decoded game audio, but it is not real-time
headphone/speaker playback. Voice and world-event scheduling, Opus,
music and sample variants remain for later stages. There is no C++
runtime or FFI in the Rust decoder.

### WavPack codec coverage in 20.2.1 resources

The checked \`wavicle\` decoder supports the original lossless v0x405
assets (for instance gun, shotgun, hammer, weapon switch and jumping).
Several original v0x406 resources (hook attach/no-attach, airjump, laser
fire) use **hybrid WavPack**, which that pure Rust library deliberately
does not implement. Their files now return \`SoundError::Unsupported\`
rather than reporting a corrupt file. The decoder is therefore a
**partial** DDNet sound loader, not yet a drop-in replacement for the
original native WavPack backend. CI validates three real lossless effects
and uploads a playable WAV artifact.

## Stage 5 — native audio output and event playback

\`window\` feature now selects CPAL 0.16.0 to open the host's default
output stream: WASAPI on Windows or ALSA on Linux. A safe Rust
\`AudioDevice\` owns the stream and a shared mixer. The audio callback
supports interleaved i16/f32/u16 samples, mono/stereo/multichannel
layouts, and bounded mixing blocks. It tries the mutex without waiting:
if the game thread is scheduling an effect, it outputs silence rather
than blocking the real-time callback. Original DDNet WavPack samples
are decoded and cached by the game thread. On absent devices or missing
local assets, the game remains interactive but silent.

Offline controls now emit real sounds for jump, weapon change, firing
and hook attachment (if a compatible .wv is available). The online 0.6
prototype derives weapon attacks, jumping and hook attachment from
consecutive *authoritative* snapshots rather than visual interpolation,
preventing repeated sound at display FPS. Positional sounds follow
the local listener.

Linux and Windows CI build and test the CPAL integration but do not
guarantee functional hardware playback on end-user machines. CPAL's
Linux backend uses the system ALSA library (installed in CI); the Rust
client does not link original DDNet C++ code.

## Stage 6 / official master server browser (increment 1)

The Rust \`server_browser.rs\` module now downloads DDNet's public
\`/ddnet/15/servers.json\` via native HTTPS with four official mirror
fallbacks and bounded response/timeouts. It parses genuine master
\`addresses\`, \`info\`, player counts, map names and game type.
**Only** validated \`tw-0.6+udp://\` endpoints are listed as joinable;
0.7-only servers are not silently passed to a 0.6-only transport.
Malformed individual entries are skipped, duplicate IP:ports removed,
results sorted by populated players. Case-insensitive filtering covers
server name, map, mode, region and address.

CLI: \`cargo run -- --servers [FILTER]\`. For deterministic offline
validation: \`cargo run -- --servers-file tests/fixtures/master.json\`.
\`--settings\` and \`--set-name NICK\` manage a small, local JSON
settings file (nickname, filter, hide-empty, last chosen endpoint).
Windowed \`--browse-servers\` provides a clickable/connectable listing.

The **master is discovery only**. Listed server addresses have not been
verified as connectable, and DDNet 0.7, full DDNet extension negotiation,
server join compatibility and full game menus still require porting.

## Stage 6 / favorites and connection preferences

The \`ClientSettings\` JSON store now persists up to 128 unique validated
\`IP:PORT\` favorite 0.6 endpoints, a favorite-only browser flag, a
receive timeout of 5–60 seconds (default 20) and an automatic reconnect
limit of 0–10 attempts (default 3). Existing settings files load
without a migration; invalid favorites and out-of-range options are
ignored on load. Saving rejects malformed endpoints, duplicates and
invalid options. Favorite state is kept independently of the master
list, so \`--favorites\` can inspect it without network access.

In the native browser, press **F** to toggle favorite on the selected
server and **F2** to show only favorites; **F6** continues to hide
empty servers. The row prefix \`+\` identifies a favorite.
The same centralized \`visible_indices\` filter drives both drawing
and Enter selection. Commands:
\`--favorites\`,
\`--favorite add 127.0.0.1:8303\`,
\`--favorite remove 127.0.0.1:8303\`,
\`--set-connection 20 3\`,
\`--connect-last\`.
The native 0.6 client actually uses the persisted timeout and retry
count. No saved password/token is added.

Limitations: selecting a saved favorite whose address is absent from the
current master list is not yet possible in the window. The CLI can list
it. Real public-server handshake compatibility remains experimental.


## Stage 7 / demo file inspection

\`src/demo.rs\` parses bounded \`TWDEMO\` v3-v6 headers from DDNet
20.2.1, including version-specific timeline, big-endian map length,
CRC, duration and optional UUID-tagged SHA-256 extension. It reads at
most 484 bytes of any input .demo; the embedded map is checked against
file size without allocation. Use \`--inspect-demo FILE.demo\`.
Full demo playback, recording and editor UI remain unimplemented.

## CachyOS preview bundle

The workflow \`.github/workflows/cachyos-release.yml\` compiles the
native Rust binary with \`window\` support on Ubuntu 24.04 x86_64 and
packages original DDNet 20.2.1 \`game.png\`, \`mapres/\`, \`audio/\`,
\`skins/default.png\`, and sample maps next to the binary. The
\`run.sh\` launcher starts the master browser by default or opens an
offline sandbox with \`--offline\`.

The archive is tested **after extraction** without Cargo. CI checks
ELF dependencies and starts the compiled binary in Xvfb, rendering
both an offline screenshot and the browser screenshot. This is a
glibc-linked native Linux build (not an AppImage/static build);
CachyOS uses X11 or XWayland and system ALSA libraries. Live DDNet
public-server compatibility is not guaranteed by these smoke tests.

## Stage 7 — demo frame decoding and first screenshot playback primitive

The original \`TWDEMO\` v3-v6 chunk stream now has a bounded,
seek-based decoder in \`src/demo_playback.rs\`. It reads legacy (v3/v4)
and compressed (v5/v6) tick markers, indexes keyframes and all chunk
types, validates payload boundaries and frame ordering without loading
large .demo bodies, and runs each snapshot chunk through the original
static Huffman and DDNet variable integer decompressors. Full snapshots
are reconstructed from \`CSnapshot\` data headers and offsets; delta
snapshots are applied using the already tested vanilla 0.6 decoder.
A random-access frame request begins at the nearest earlier keyframe.

CLI: \`--demo-timeline FILE.demo\` reports frames, keyframes and snapshot
statistics; \`--demo-frame FILE.demo TICK OUT.png\` reconstructs a frame
and draws characters, using an embedded map and original textures
where available. Synthetic test recordings cover v3/v4/v5/v6,
Huffman-encoded snapshots, delta updates, malformed ticks, bounds and
truncation. Demo playback in a native interactive window, audio,
full UUID extended netobjects and editing remain unimplemented.

## Stage 7: interactive native demo player

The window-feature demo viewer supports --play-demo FILE.demo with a keyframe-aware incremental decoder and 50Hz playback clock. Controls: Space pauses, Left/Right steps recorded frames, PageUp/PageDown seeks ten seconds, Up/Down changes speed (0.25x through 4x), Home/End jumps, mouse timeline scrubs, wheel zooms, R restarts and Escape closes. It uses the original map textures and default skin when the recording contains embedded map data, and falls back to a diagnostic view otherwise.

CI creates a genuine Huffman-compressed synthetic demo with 201 tick markers, two keyframes, delta updates and an official map. It launches the native replay window under Xvfb and validates a PNG. The downloadable CachyOS release runs the same test from the extracted archive.

Limitations: not yet verified with a user's actual multiplayer demo. Original demo audio, all UUID extension netobjects, additional visual effects and fully faithful interpolation remain future work.
