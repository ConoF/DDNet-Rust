# DDNet 20.2.1 visual and frame-time parity audit

**Reference:** official DDNet `20.2.1`, pinned C++ source at
`176fc2f9ab85ace703c6ec10da0249b01995ba94` in
`upstream/ddnet`. Rust baseline after the initial pass:
`9a3239dd15ab852f7bf4ef0b4d10cf6df8e941e3`.

**Status:** major visual and performance parity gaps remain. This is an
evidence-backed source comparison, **not** a claim of screenshot-perfect
equivalence or a frame-time benchmark on a physical CachyOS machine.

## Confirmed gaps

| Area | Official DDNet 20.2.1 | Current Rust subsystem | Gap |
|---|---|---|---|
| Graphics | GPU-accelerated queued commands, texture/buffer batching (`src/engine/client/graphics_threaded.cpp`) | Per-frame, full CPU RGB framebuffer (`src/graphics.rs`, `src/map_visual.rs`) | **P0**: raster cost scales with rendered pixels and layer count |
| Map | Layer groups, clipping, quad layers, envelopes, buffered tiles (`src/game/client/components/maplayers.cpp`; map renderer) | Ordinary textured tile layers and partial parallax; **quad and envelope layers missing** | **P0**: many original backgrounds and effects do not appear |
| Render order | Background layers → world entities/tees → foreground layers → nameplates/HUD (`src/game/client/gameclient.cpp`) | Two-pass tile layers (now ported), approximate tee sprites | **P1**: quad layers, particles, exact sprite ordering not implemented |
| Tees | Full skin, emote, hands, weapons, feet and hook animation (`src/game/client/components/players.cpp`) | Approximate CPU sprites (`src/sprite_render.rs`) | **P1**: poses, effects, per-player custom skins differ |
| GUI | Custom scaled UI, embedded textures, font renderer, localization, scalable menu states (`src/game/client/components/menus.cpp`, `hud.cpp`, `chat.cpp`, `scoreboard.cpp`, `nameplates.cpp`) | `minifb` + bitmap font (`src/online_ui.rs`, `src/server_menu.rs`) | **P0**: menu/HUD structure, typography, Unicode and scaling do not match |
| Movement | Prediction + tuning, input timing/smooth time, authoritative reconciliation | Bounded single-tee prediction with fallback for hooks and neighbors | **P0**: timing, recoil, hooks, teleports, DDNet tune tiles are not equivalent |
| Audio/effects | Full sound event and particle pipeline | Partial sounds and sprite effects | **P2**: visible/audible effects missing |

## Changes already made in this audit pass

- Split the existing ordinary tile layers by the map's Game-layer position;
  render foreground tiles *after* characters (also in offline preview).
- Added a fast direct-write path for opaque map pixels (not a GPU rewrite).
- Derive the snapshot interpolation duration from actual server tick gap
  at 50 Hz, not the previously hardcoded single tick.
- Normalize camera following by elapsed frame time.
- Add a title-bar FPS and **frame + present milliseconds** indicator to
  the online window, so stutters have concrete local measurements.
- Remove permanent prototype control strips from online gameplay; move
  chat lower-left and render native client-info nameplates above tees.

## What still requires a different architecture

A fully faithful DDNet look should **not** be implemented by making the
software framebuffer more complicated. Major follow-up milestones:

1. **P0 GPU backend.** Replace per-pixel CPU tile/sprite rasterization with
   a batched renderer (texture atlas, instanced tiles/quads, sensible
   1920x1080 baseline) and frame profiling. Preserve original z-order.
2. **P0 quad and envelope map renderer.** Implement textured/colored
   `CMapItemLayerQuads`, vertex colors, position/color envelope evaluation,
   group clipping/parallax, animated map entities and graphical tile borders.
3. **P0 true DDNet frame interpolation/prediction.** Tune zones, hooks,
   collision with players, 50Hz input/snapshot scheduling, visual corrections
   and demo/reference tests against the **same** official server.
4. **P1 native GUI port.** Recreate official 20.2.1 menus and UI
   from the pinned sources, using the actual atlas/icons and proper
   Unicode fonts (`data/fonts/`), not a bitmap-font lookalike.
5. **P1 gameplay sprites.** Custom skins, hands, proper animation states,
   weapon offsets, muzzle effects, ghost/particle layers and scoreboard.

## How to reproduce and measure

Compare builds at identical resolution, zoom, map, server, camera and
character positions. On CachyOS, open the current preview:

```bash
./run.sh --play-online SERVER:8303
```

The online window title shows **FPS** and a running average **frame X ms**,
which includes CPU draw and native buffer presentation. Screenshot the
same camera position in official DDNet and the Rust client. A 60 FPS frame
budget is about 16.7 ms. Do not attribute all delays to rendering if the
`frame` number is low: record input/network timing and display update
separately.

The GitHub Actions preview-image artifact tests *offline* asset loading
and PNG validity. It is **not** a real-server performance benchmark or a
pixel-diff test against official DDNet. In the preview screenshot the
static brown tilemap exists but much of the scene remains flat/dark
because quad/envelope backgrounds are not yet drawn.

## Acceptance criteria for each parity subsystem

- Record the reference upstream file and the precise Rust implementation.
- Create a side-by-side frame with the **same official map and world camera**.
- Compare geometry, layer order, colors, fonts, animation cadence and input.
- Add deterministic tests (CRC-verified snapshots, fixed physics vectors,
  screenshot pixel/shape checks) and regression benchmarks.
- Validate actual 60 FPS playability on the target CachyOS machine;
  report measured p50/p95 frame times, not estimated progress percentages.


## 2026-11-08 implementation update

The earlier parity table records the **original audit baseline**, not the
current full capabilities. The following gaps have since narrowed:

- Implemented basic 152-byte CQuad parsing, textured/solid CPU triangles,
  original tile/quad ordering and front/background grouping.
- Added original envelope metadata and 22.10 fixed-point EnvPoints, color
  and position/rotation animation on quads, and tile color-envelope tint.
- Added server-tick vs local map-load clocks, demo/online/offline integration,
  four basic easing curves plus step, and legacy Teeworlds v3 inline Bezier.
- Added pixel-output regression tests and a flat-color quad raster fast path.

**Remaining P0:** A proper GPU map renderer with quad/texture batching,
frame pacing and benchmarks; the complete original DDNet UI/font system;
game-side physics and input/prediction parity. Group clipping and UUID
Bezier handles still need implementation. Do not claim the Rust window is
visually equivalent to 20.2.1 yet.
