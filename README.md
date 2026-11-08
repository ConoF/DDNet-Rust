# DDNet-Rust

A clean-room, **Rust-first port of the official DDNet client**, targeting gameplay, network and asset compatibility with upstream DDNet. This is **not** CFX Client and does not add gameplay modifications or cheats.

**Upstream baseline:** [DDNet 20.2.1](https://github.com/ddnet/ddnet/tree/20.2.1) (2026-10-27), commit `176fc2f9ab85ace703c6ec10da0249b01995ba94`.

## Status

**Under development; not a playable client.** The checked-in Rust modules are incremental, testable ports of specific upstream functions, not a complete game. Do not mistake a successful `cargo test` for full DDNet compatibility.

## Checkout

```sh
git clone --recurse-submodules https://github.com/ConoF/DDNet-Rust.git
cd DDNet-Rust
cargo test
cargo run -- --self-test
```

`upstream/ddnet` is a **pinned Git submodule** containing the unmodified official C++ source of DDNet 20.2.1. It is a reference and compatibility oracle, **not linked into the Rust executable**. Use `git submodule update --init` to fetch it after an existing clone.

## Project rules

- Keep the behavior and defaults of vanilla DDNet. No CFX modules.
- Use Rust for all new gameplay and client code, with no C++ runtime dependency in the final client.
- Compare each ported subsystem against the pinned upstream C++ baseline with reference vectors, differential tests and integration tests before claiming compatibility.
- Record implemented, partially implemented and missing functionality explicitly in [docs/PORTING.md](docs/PORTING.md).
- Preserve the attribution and licenses of all upstream sources and assets.

The original project is [DDraceNetwork/DDNet](https://github.com/ddnet/ddnet), maintained by its respective contributors. This project is an independent, incomplete Rust port and is not an official DDNet release.
