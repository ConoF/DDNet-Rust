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
| Map download, graphics, input devices, UI, audio | Not implemented | None |
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
