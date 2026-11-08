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
| Teeworlds datafile v3/v4 metadata and item/block indexing | Partial | Synthetic fixtures, truncation tests, mutation simulations |
| v4 zlib-compressed data blocks / map layer decoding | Not implemented | None |
| Gravity, directional movement, ground/air jump | Partial | 50 Hz tick-state simulation tests |
| Velocity ramp and projectile position | Math functions only | Unit tests |
| Collision, tiles, freeze, switches, teleports | Not implemented | None |
| Hook, player interactions, full prediction | Not implemented | None |
| Network packets, handshake, snapshots | Not implemented | None |
| Map download, graphics, input devices, UI, audio | Not implemented | None |
| Server browser, gameplay, editor and demos | Not implemented | None |

Passing CI here **does not** mean the game itself is operational.

## Required porting order

1. Keep stable C++ 20.2.1 as a reproducible source of truth.
2. Port packet header, handshake, reliable UDP sequencing and generated net messages;
   test against real server traces and invalid packets.
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
cargo test
cargo run -- --self-test
```

The Linux CI compiles a small C++ test from the **original DDNet header** and
compares its integer encoding output byte-for-byte with the Rust implementation.
This is a limited differential test, not a full client simulation.
