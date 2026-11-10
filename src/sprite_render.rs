//! CPU sprite compositor for DDNet 0.6 skins and game.png.
//!
//! Spritesheet rectangles are sourced from upstream DDNet 20.2.1
//! datasrc/content.py. Pose/timing are a conservative approximation;
//! this is not the original DDNet animation state machine.

use crate::graphics::{Camera, Framebuffer, RenderError, HOOK};
use crate::netobjects::{CharacterCoreNet, CHARACTER_TYPE};
use crate::snapshot_delta::SnapshotState;
use crate::texture::Texture;
use crate::netobjects::{CharacterHudStatus, PickupNet};

pub(crate) const MAX_HOOK_DISTANCE: f32 = 2048.0;
pub(crate) const GRID_SKIN: (usize, usize) = (8, 4);
pub(crate) const GRID_GAME: (usize, usize) = (32, 16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sprite {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

pub(crate) const BODY: Sprite = Sprite { x: 0, y: 0, w: 3, h: 3 };
pub(crate) const FOOT: Sprite = Sprite { x: 6, y: 1, w: 2, h: 1 };
pub(crate) const HOOK_CHAIN: Sprite = Sprite { x: 2, y: 0, w: 1, h: 1 };
pub(crate) const HOOK_HEAD: Sprite = Sprite { x: 3, y: 0, w: 2, h: 1 };

pub(crate) fn weapon_sprite(weapon: i32) -> Option<Sprite> {
    match weapon {
        0 => Some(Sprite { x: 2, y: 1, w: 4, h: 3 }), // hammer
        1 => Some(Sprite { x: 2, y: 4, w: 4, h: 2 }), // gun
        2 => Some(Sprite { x: 2, y: 6, w: 8, h: 2 }), // shotgun
        3 => Some(Sprite { x: 2, y: 8, w: 7, h: 2 }), // grenade
        4 => Some(Sprite { x: 2, y: 12, w: 7, h: 3 }), // laser
        5 => Some(Sprite { x: 2, y: 10, w: 8, h: 2 }), // ninja
        _ => None,
    }
}

/// Original datasrc/content.py game.png cursor sprites, 2x2 atlas
/// cells for each of the six 0.6 weapons. These remain in HUD screen
/// coordinates, independent of the camera or gameplay hook direction.
pub(crate) fn weapon_cursor_sprite(weapon: i32) -> Option<Sprite> {
    let y = match weapon {
        0 => 0,  // hammer
        1 => 4,  // gun
        2 => 6,  // shotgun
        3 => 8,  // grenade
        4 => 12, // laser
        5 => 10, // ninja
        _ => return None,
    };
    Some(Sprite { x: 0, y, w: 2, h: 2 })
}

/// Overlay a cursor from game.png. Missing assets are handled by the caller,
/// not by silently drawing the wrong atlas cells.
pub(crate) fn render_weapon_cursor(
    frame: &mut Framebuffer, game: Option<&Texture>,
    weapon: i32, position: (f32, f32),
) -> bool {
    let Some(sprite) = weapon_cursor_sprite(weapon) else { return false; };
    let Some(atlas) = game.filter(|tex| valid_sheet(tex, GRID_GAME)) else {
        return false;
    };
    if !position.0.is_finite() || !position.1.is_finite() { return false; }
    blit(frame, atlas, GRID_GAME, sprite, position,
        (32.0, 32.0), (0.0, false));
    true
}

pub(crate) const HEALTH_FULL: Sprite = Sprite { x: 21, y: 0, w: 2, h: 2 };
pub(crate) const HEALTH_EMPTY: Sprite = Sprite { x: 23, y: 0, w: 2, h: 2 };
pub(crate) const ARMOR_FULL: Sprite = Sprite { x: 21, y: 2, w: 2, h: 2 };
pub(crate) const ARMOR_EMPTY: Sprite = Sprite { x: 23, y: 2, w: 2, h: 2 };

pub(crate) fn hud_ammo_sprite(weapon: i8) -> Option<Sprite> {
    let y = match weapon {
        1 => 4, 2 => 6, 3 => 8, 4 => 12,
        _ => return None,
    };
    let x = if weapon == 1 { 6 } else { 10 };
    Some(Sprite { x, y, w: 2, h: 2 })
}

/// Original CHud::RenderAmmoHealthAndArmor: 10 icons, 12-pixel spacing,
/// health row 0, armor row 12 and projectile ammo row 24.
/// Only verified local CNetObj_Character values should be used.
pub(crate) fn render_player_hud(
    frame: &mut Framebuffer, game: Option<&Texture>, stats: CharacterHudStatus,
) -> bool {
    let Some(atlas) = game.filter(|tex| valid_sheet(tex, GRID_GAME)) else {
        return false;
    };
    for i in 0..10 {
        let center_x = 5.0 + i as f32 * 12.0;
        let health = if i < usize::from(stats.health) { HEALTH_FULL }
            else { HEALTH_EMPTY };
        let armor = if i < usize::from(stats.armor) { ARMOR_FULL }
            else { ARMOR_EMPTY };
        blit(frame, atlas, GRID_GAME, health, (center_x, 5.0),
            (10.0, 10.0), (0.0, false));
        blit(frame, atlas, GRID_GAME, armor, (center_x, 17.0),
            (10.0, 10.0), (0.0, false));
    }
    if stats.ammo >= 0 {
        if let Some(projectile) = hud_ammo_sprite(stats.weapon) {
            for i in 0..usize::from(stats.ammo as u8) {
                blit(frame, atlas, GRID_GAME, projectile,
                    (5.0 + i as f32 * 12.0, 29.0),
                    (10.0, 10.0), (0.0, false));
            }
        }
    }
    true
}

pub(crate) fn muzzle_sprite(weapon: i32) -> Option<Sprite> {
    match weapon {
        1 => Some(Sprite { x: 8, y: 4, w: 4, h: 2 }),
        2 => Some(Sprite { x: 12, y: 6, w: 4, h: 2 }),
        5 => Some(Sprite { x: 25, y: 0, w: 7, h: 4 }),
        _ => None,
    }
}

/// Visual mapping from DDNet 20.2.1 datasrc/content.py. These are the
/// same atlas cells used by CItems::RenderPickup. Item sizes follow the
/// original normalized sprite dimensions and 64/96/128-unit visual sizes.
pub(crate) fn pickup_art(pickup: PickupNet) -> Option<(Sprite, (f32, f32))> {
    let (sprite, visual_size) = match pickup.kind {
        0 => (Sprite { x: 10, y: 2, w: 2, h: 2 }, 64.0),
        1 => (Sprite { x: 12, y: 2, w: 2, h: 2 }, 64.0),
        2 => (weapon_sprite(i32::from(pickup.subtype))?, 96.0),
        3 => (Sprite { x: 2, y: 10, w: 8, h: 2 }, 128.0),
        4 => (Sprite { x: 15, y: 2, w: 2, h: 2 }, 64.0),
        5 => (Sprite { x: 17, y: 2, w: 2, h: 2 }, 64.0),
        6 => (Sprite { x: 10, y: 10, w: 2, h: 2 }, 64.0),
        7 => (Sprite { x: 19, y: 2, w: 2, h: 2 }, 64.0),
        8 => (Sprite { x: 6, y: 2, w: 2, h: 2 }, 64.0),
        _ => return None,
    };
    let (w, h) = (sprite.w as f32, sprite.h as f32);
    let normal = visual_size / w.hypot(h);
    Some((sprite, (w * normal, h * normal)))
}

/// Deterministic 50Hz equivalent of the original floating pickup motion.
/// Rendering does not modify snapshot positions or the collision world.
pub(crate) fn pickup_position(pickup: PickupNet, tick: u64) -> (f32, f32) {
    let offset = (pickup.x as f32 + pickup.y as f32) / 32.0;
    let phase = tick as f32 / 25.0 + offset;
    (pickup.x as f32 + phase.cos() * 2.5,
     pickup.y as f32 + phase.sin() * 2.5)
}

fn draw_pickup(
    frame: &mut Framebuffer, pickup: PickupNet,
    atlas: &Texture, camera: Camera, tick: u64,
) {
    let Some((sprite, size)) = pickup_art(pickup) else { return; };
    let (px, py) = pickup_position(pickup, tick);
    blit(frame, atlas, GRID_GAME, sprite, to_screen(frame, camera, px, py),
        (size.0 * camera.scale, size.1 * camera.scale), (0.0, false));
}

pub(crate) fn skin_eye(emote: i32) -> Sprite {
    // EMOTE_NORMAL=0, PAIN=1, HAPPY=2, SURPRISE=3, ANGRY=4, BLINK=5.
    let x = match emote {
        1 => 4, 2 => 5, 3 => 7, 4 => 3, _ => 2,
    };
    Sprite { x, y: 3, w: 1, h: 1 }
}

pub(crate) fn valid_sheet(tex: &Texture, grid: (usize, usize)) -> bool {
    tex.width as usize >= grid.0 && tex.height as usize >= grid.1
        && (tex.width as usize).is_multiple_of(grid.0)
        && (tex.height as usize).is_multiple_of(grid.1)
}

fn to_screen(frame: &Framebuffer, camera: Camera, x: f32, y: f32) -> (f32, f32) {
    (
        (x - camera.center.x) * camera.scale + frame.width as f32 * 0.5,
        (y - camera.center.y) * camera.scale + frame.height as f32 * 0.5,
    )
}

/// Draws one atlas sprite with nearest-neighbor scaling, rotation,
/// optional vertical mirror and straight alpha compositing.
/// All raster loops are clipped against the destination framebuffer.
fn blit(
    frame: &mut Framebuffer,
    atlas: &Texture,
    grid: (usize, usize),
    sprite: Sprite,
    center: (f32, f32),
    size: (f32, f32),
    orientation: (f32, bool),
) {
    let (rotation, mirrored_y) = orientation;
    if !valid_sheet(atlas, grid) || !center.0.is_finite() || !center.1.is_finite()
        || !rotation.is_finite() || !size.0.is_finite() || !size.1.is_finite()
        || size.0 <= 0.0 || size.1 <= 0.0
        || sprite.x.checked_add(sprite.w).is_none_or(|n| n > grid.0)
        || sprite.y.checked_add(sprite.h).is_none_or(|n| n > grid.1)
        || sprite.w == 0 || sprite.h == 0
    {
        return;
    }
    let cell_w = atlas.width as usize / grid.0;
    let cell_h = atlas.height as usize / grid.1;
    let source_x = sprite.x * cell_w;
    let source_y = sprite.y * cell_h;
    let source_w = sprite.w * cell_w;
    let source_h = sprite.h * cell_h;
    let radius = 0.5 * size.0.hypot(size.1);
    let left = (center.0 - radius).floor().clamp(0.0, frame.width as f32) as usize;
    let right = (center.0 + radius).ceil().clamp(0.0, frame.width as f32) as usize;
    let top = (center.1 - radius).floor().clamp(0.0, frame.height as f32) as usize;
    let bottom = (center.1 + radius).ceil().clamp(0.0, frame.height as f32) as usize;
    let cosine = rotation.cos();
    let sine = rotation.sin();
    for y in top..bottom {
        for x in left..right {
            let dx = x as f32 + 0.5 - center.0;
            let dy = y as f32 + 0.5 - center.1;
            let u = (dx * cosine + dy * sine) / size.0 + 0.5;
            let mut v = (-dx * sine + dy * cosine) / size.1 + 0.5;
            if mirrored_y { v = 1.0 - v; }
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let tx = source_x + ((u * source_w as f32) as usize).min(source_w - 1);
            let ty = source_y + ((v * source_h as f32) as usize).min(source_h - 1);
            let source = (ty * atlas.width as usize + tx) * 4;
            let alpha = u32::from(atlas.rgba[source + 3]);
            if alpha == 0 { continue; }
            let idx = y * frame.width as usize + x;
            let back = frame.pixels[idx];
            let mut composite = 0;
            for channel in 0..3 {
                let shift = 16 - 8 * channel;
                let backdrop = (back >> shift) & 255;
                let foreground = u32::from(atlas.rgba[source + channel]);
                let result = (foreground * alpha + backdrop * (255 - alpha) + 127) / 255;
                composite = (composite << 8) | result;
            }
            frame.pixels[idx] = composite;
        }
    }
}

fn draw_line(frame: &mut Framebuffer, from: (f32, f32), to: (f32, f32), color: u32) {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let steps = dx.abs().max(dy.abs()).min(4096.0) as usize;
    if steps == 0 { return; }
    for i in 0..=steps {
        let factor = i as f32 / steps as f32;
        let x = (from.0 + factor * dx).round() as i64;
        let y = (from.1 + factor * dy).round() as i64;
        if x >= 0 && y >= 0 && x < i64::from(frame.width)
            && y < i64::from(frame.height)
        {
            frame.pixels[y as usize * frame.width as usize + x as usize] = color;
        }
    }
}

fn draw_hook(
    frame: &mut Framebuffer, character: CharacterCoreNet,
    camera: Camera, game: Option<&Texture>,
) {
    if character.hook_state <= 0 { return; }
    let dx = character.hook_x as f32 - character.x as f32;
    let dy = character.hook_y as f32 - character.y as f32;
    let distance = dx.hypot(dy);
    if !(1.0..=MAX_HOOK_DISTANCE).contains(&distance) { return; }
    let tee = to_screen(frame, camera, character.x as f32, character.y as f32);
    let tip = to_screen(frame, camera, character.hook_x as f32, character.hook_y as f32);
    let Some(atlas) = game.filter(|g| valid_sheet(g, GRID_GAME)) else {
        draw_line(frame, tee, tip, HOOK);
        return;
    };
    let angle = dy.atan2(dx);
    // The hook segment from tip back toward the character advances
    // every 24 units, as CPlayers::RenderHook in DDNet 20.2.1.
    let count = (distance / 24.0).ceil().min(90.0) as usize;
    for index in 1..count {
        let span = index as f32 * 24.0;
        if span >= distance { break; }
        let x = character.hook_x as f32 - dx * span / distance;
        let y = character.hook_y as f32 - dy * span / distance;
        let center = to_screen(frame, camera, x, y);
        blit(frame, atlas, GRID_GAME, HOOK_CHAIN, center,
             (13.0 * camera.scale, 16.0 * camera.scale),
             (angle + std::f32::consts::FRAC_PI_2, false));
    }
    blit(frame, atlas, GRID_GAME, HOOK_HEAD, tip,
         (24.0 * camera.scale, 12.0 * camera.scale), (angle, false));
}

fn draw_tee(
    frame: &mut Framebuffer, character: CharacterCoreNet, words: &[i32],
    camera: Camera, skin: &Texture, game: Option<&Texture>, tick: u64,
) {
    let px = character.x as f32;
    let py = character.y as f32;
    let (_, center_y) = to_screen(frame, camera, px, py);
    let scale = camera.scale;
    let speed = character.velocity_x as f32 / 256.0;
    let walking = speed.abs() > 0.3 && character.jumped & 1 == 0;
    let phase = (tick % 10_000) as f32 * 0.34 + (px / 32.0);
    let stride = if walking { phase.sin() * 7.0 } else { 0.0 };
    let bounce = if walking { phase.cos().abs() * 2.0 } else { 0.0 };
    let center = to_screen(frame, camera, px, py - bounce);
    if center.0 < -128.0 || center.0 > frame.width as f32 + 128.0
        || center_y < -128.0 || center_y > frame.height as f32 + 128.0
    {
        return;
    }
    // Foot motion is deterministic in game ticks, independent of render FPS.
    for (offset_x, offset_y) in [
        (-8.0 + stride, 12.0 - stride.abs() * 0.5),
        (8.0 - stride, 12.0 + stride.abs() * 0.5),
    ] {
        let foot = (center.0 + offset_x * scale, center.1 + offset_y * scale);
        blit(frame, skin, GRID_SKIN, FOOT, foot,
             (15.0 * scale, 12.0 * scale), (0.0, false));
    }
    blit(frame, skin, GRID_SKIN, BODY, center,
         (34.0 * scale, 32.0 * scale), (0.0, false));
    let emote = words.get(20).copied().unwrap_or(0);
    let facing = if character.direction < 0 { -1.0 } else { 1.0 };
    let blink = emote == 5 || tick % 201 < 3;
    blit(frame, skin, GRID_SKIN, skin_eye(emote),
         (center.0 + facing * 6.0 * scale, center.1 - 3.0 * scale),
         (13.0 * scale, if blink { 3.0 * scale } else { 11.0 * scale }),
         (0.0, facing < 0.0));

    let Some(game) = game.filter(|g| valid_sheet(g, GRID_GAME)) else { return; };
    let weapon = words.get(19).copied().unwrap_or(-1);
    let Some(sprite) = weapon_sprite(weapon) else { return; };
    let angle = (character.angle as f32 / 256.0).clamp(-std::f32::consts::PI * 2.0,
                                                     std::f32::consts::PI * 2.0);
    let aim_x = angle.cos();
    let aim_y = angle.sin();
    let mirrored = aim_x < 0.0;
    let attack = words.get(21).copied().unwrap_or(0);
    let attack_age = (tick as i64).saturating_sub(i64::from(attack));
    let recoil = if attack > 0 && (0..=5).contains(&attack_age) {
        (5 - attack_age) as f32 * 1.2
    } else { 0.0 };
    let weapon_center = (
        center.0 + (aim_x * (23.0 - recoil)) * scale,
        center.1 + (aim_y * (23.0 - recoil)) * scale + 2.0 * scale,
    );
    let (w, h) = if weapon == 0 { (32.0, 28.0) }
        else if weapon == 1 { (30.0, 17.0) }
        else { (38.0, 19.0) };
    blit(frame, game, GRID_GAME, sprite, weapon_center,
         (w * scale, h * scale), (angle, mirrored));

    if attack > 0 && (0..=2).contains(&attack_age) {
        if let Some(flash) = muzzle_sprite(weapon) {
            let muzzle = (
                weapon_center.0 + aim_x * w * 0.56 * scale,
                weapon_center.1 + aim_y * w * 0.56 * scale,
            );
            blit(frame, game, GRID_GAME, flash, muzzle,
                 (22.0 * scale, 14.0 * scale), (angle, mirrored));
        }
    }
}

/// Render character netobjects using the original 0.6 skin/game atlases.
/// Hook chains/head render behind tees. Character sprites receive deterministic
/// gait, eye orientation, aim and recent-attack recoil/muzzle animation.
///
/// Falls back to existing diagnostic rendering when skins/default.png is
/// unavailable. No gameplay, network state or collision rules are modified.
pub fn render_animated_snapshot(
    frame: &mut Framebuffer, state: &SnapshotState, camera: Camera,
    skin: Option<&Texture>, game: Option<&Texture>, tick: u64,
) -> Result<usize, RenderError> {
    if !camera.scale.is_finite() || !(0.05..=16.0).contains(&camera.scale)
        || !camera.center.x.is_finite() || !camera.center.y.is_finite()
    {
        return Err(RenderError::InvalidCamera);
    }
    let game = game.filter(|g| valid_sheet(g, GRID_GAME));
    if let Some(atlas) = game {
        // CItems is drawn behind all player hooks and tee sprites.
        for item in &state.items {
            if let Some(pickup) = PickupNet::from_object(item) {
                draw_pickup(frame, pickup, atlas, camera, tick);
            }
        }
    }
    let Some(skin) = skin.filter(|s| valid_sheet(s, GRID_SKIN)) else {
        return frame.render_snapshot(state, camera);
    };
    for item in &state.items {
        if let Some(character) = CharacterCoreNet::from_object(item) {
            draw_hook(frame, character, camera, game);
        }
    }
    let mut count = 0usize;
    for item in &state.items {
        if let Some(character) = CharacterCoreNet::from_object(item) {
            let words = if item.type_id == CHARACTER_TYPE {
                item.words.as_slice()
            } else { &[] };
            draw_tee(frame, character, words, camera, skin, game, tick);
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::BACKGROUND;
    use crate::snapshot_delta::SnapshotObject;

    fn sprite_sheet(w: usize, h: usize, grid: (usize, usize),
                    regions: &[(Sprite, [u8; 4])]) -> Texture {
        let mut rgba = vec![0; w * h * 4];
        for &(rect, color) in regions {
            for y in rect.y * (h / grid.1)..(rect.y + rect.h) * (h / grid.1) {
                for x in rect.x * (w / grid.0)..(rect.x + rect.w) * (w / grid.0) {
                    let at = (y * w + x) * 4;
                    rgba[at..at + 4].copy_from_slice(&color);
                }
            }
        }
        Texture::from_rgba(w as u32, h as u32, rgba).unwrap()
    }

    fn skin() -> Texture {
        sprite_sheet(80, 40, GRID_SKIN, &[
            (BODY, [240, 40, 20, 255]),
            (FOOT, [20, 240, 20, 255]),
            (skin_eye(0), [20, 20, 240, 255]),
        ])
    }

    fn game() -> Texture {
        sprite_sheet(320, 160, GRID_GAME, &[
            (HOOK_CHAIN, [180, 0, 180, 255]),
            (HOOK_HEAD, [255, 255, 0, 255]),
            (weapon_sprite(0).unwrap(), [200, 50, 50, 255]),
            (weapon_sprite(1).unwrap(), [0, 200, 250, 255]),
            (weapon_sprite(2).unwrap(), [240, 80, 0, 255]),
            (weapon_sprite(3).unwrap(), [160, 160, 20, 255]),
            (weapon_sprite(4).unwrap(), [10, 160, 100, 255]),
            (weapon_sprite(5).unwrap(), [110, 20, 200, 255]),
        ])
    }

    fn state(x: i32, y: i32, weapon: i32) -> SnapshotState {
        let mut words = vec![0; 22];
        words[1] = x;
        words[2] = y;
        words[19] = weapon;
        SnapshotState {
            items: vec![SnapshotObject { type_id: 9, id: 0, words }],
        }
    }

    fn camera() -> Camera {
        Camera { center: crate::physics::Vec2::new(100.0, 100.0), scale: 1.0 }
    }

    #[test]
    fn pickup_art_matches_original_all_nine_powerup_types_and_bobs() {
        let pickup = |kind, subtype| PickupNet {
            x: 64, y: 64, kind, subtype,
        };
        for (kind, x, y, w, h) in [
            (0, 10, 2, 2, 2), (1, 12, 2, 2, 2),
            (2, 2, 4, 4, 2), (3, 2, 10, 8, 2),
            (4, 15, 2, 2, 2), (5, 17, 2, 2, 2),
            (6, 10, 10, 2, 2), (7, 19, 2, 2, 2),
            (8, 6, 2, 2, 2),
        ] {
            assert_eq!(pickup_art(pickup(kind, 1)).unwrap().0,
                Sprite { x, y, w, h });
        }
        assert!(pickup_art(pickup(2, 9)).is_none());
        assert!(pickup_art(pickup(9, 0)).is_none());
        assert_eq!(pickup_position(pickup(0, 0), 100),
            pickup_position(pickup(0, 0), 100));
        let (_, size) = pickup_art(pickup(0, 0)).unwrap();
        assert!((size.0 - 64.0 / 2.0f32.sqrt()).abs() < 0.001);
    }

    #[test]
    fn pickup_sprites_render_before_players_and_missing_game_png_is_safe() {
        let icon = pickup_art(PickupNet {
            x: 0, y: 0, kind: 0, subtype: 0,
        }).unwrap().0;
        let game = sprite_sheet(320, 160, GRID_GAME, &[
            (icon, [255, 0, 0, 255]),
        ]);
        let state = SnapshotState {
            items: vec![SnapshotObject {
                type_id: crate::netobjects::PICKUP_TYPE, id: 0,
                words: vec![100, 100, 0, 0],
            }],
        };
        let camera = Camera {
            center: crate::physics::Vec2::new(100.0, 100.0), scale: 1.0,
        };
        let mut frame = Framebuffer::new(200, 200).unwrap();
        frame.clear(BACKGROUND);
        assert_eq!(render_animated_snapshot(&mut frame, &state, camera,
            Some(&skin()), Some(&game), 0).unwrap(), 0);
        assert!(frame.pixels.contains(&0xff0000));
        frame.clear(BACKGROUND);
        render_animated_snapshot(&mut frame, &state, camera,
            None, Some(&game), 0).unwrap();
        assert!(frame.pixels.contains(&0xff0000),
            "pickups still render if the tee skin is missing");
        frame.clear(BACKGROUND);
        render_animated_snapshot(&mut frame, &state, camera,
            Some(&skin()), None, 0).unwrap();
        assert!(!frame.pixels.contains(&0xff0000));
    }

    #[test]
    fn vanilla_hud_status_uses_original_health_armor_and_projectile_icons() {
        let atlas = sprite_sheet(320, 160, GRID_GAME, &[
            (HEALTH_FULL, [255, 0, 0, 255]),
            (HEALTH_EMPTY, [64, 0, 0, 255]),
            (ARMOR_FULL, [0, 0, 255, 255]),
            (ARMOR_EMPTY, [0, 0, 64, 255]),
            (hud_ammo_sprite(1).unwrap(), [0, 255, 0, 255]),
        ]);
        let mut frame = Framebuffer::new(160, 48).unwrap();
        frame.clear(BACKGROUND);
        assert!(render_player_hud(&mut frame, Some(&atlas), CharacterHudStatus {
            health: 2, armor: 1, ammo: 3, weapon: 1,
        }));
        assert_eq!(frame.pixel(5, 5), Some(0xff0000));
        assert_eq!(frame.pixel(29, 5), Some(0x400000));
        assert_eq!(frame.pixel(5, 17), Some(0x0000ff));
        assert_eq!(frame.pixel(29, 17), Some(0x000040));
        assert_eq!(frame.pixel(5, 29), Some(0x00ff00));
        assert_eq!(frame.pixel(53, 29), Some(BACKGROUND));
        assert_eq!(hud_ammo_sprite(0), None);
        assert_eq!(hud_ammo_sprite(5), None);
        let before = frame.pixels.clone();
        assert!(!render_player_hud(&mut frame, None, CharacterHudStatus {
            health: 2, armor: 1, ammo: 3, weapon: 1,
        }));
        assert_eq!(frame.pixels, before);
    }

    #[test]
    fn all_six_cursor_regions_match_upstream_game_png_coordinates() {
        for (weapon, y) in [(0, 0), (1, 4), (2, 6),
            (3, 8), (4, 12), (5, 10)]
        {
            assert_eq!(weapon_cursor_sprite(weapon),
                Some(Sprite { x: 0, y, w: 2, h: 2 }));
        }
        assert_eq!(weapon_cursor_sprite(-1), None);
        assert_eq!(weapon_cursor_sprite(6), None);
    }

    #[test]
    fn cursor_blits_selected_atlas_sprite_without_touching_world_state() {
        let game = sprite_sheet(320, 160, GRID_GAME, &[
            (weapon_cursor_sprite(0).unwrap(), [255, 0, 0, 255]),
            (weapon_cursor_sprite(1).unwrap(), [0, 255, 0, 255]),
        ]);
        let mut frame = Framebuffer::new(64, 64).unwrap();
        frame.clear(BACKGROUND);
        assert!(render_weapon_cursor(&mut frame, Some(&game), 0, (32.0, 32.0)));
        assert_eq!(frame.pixel(32, 32), Some(0xff0000));
        assert!(render_weapon_cursor(&mut frame, Some(&game), 1, (32.0, 32.0)));
        assert_eq!(frame.pixel(32, 32), Some(0x00ff00));
        let before = frame.pixels.clone();
        assert!(!render_weapon_cursor(&mut frame, Some(&game), -1, (32.0, 32.0)));
        assert!(!render_weapon_cursor(&mut frame, None, 1, (32.0, 32.0)));
        assert!(!render_weapon_cursor(&mut frame, Some(&game), 1, (f32::NAN, 0.0)));
        assert_eq!(frame.pixels, before);
    }

    #[test]
    fn draws_all_six_weapon_atlas_regions_and_skips_invalid_weapon() {
        let skin = skin();
        let game = game();
        let mut fingerprints = Vec::new();
        for weapon in -1..=5 {
            let mut frame = Framebuffer::new(220, 200).unwrap();
            assert_eq!(render_animated_snapshot(
                &mut frame, &state(100, 100, weapon), camera(),
                Some(&skin), Some(&game), 80,
            ), Ok(1));
            fingerprints.push(frame.pixels);
        }
        for weapon in 0..6 {
            assert_ne!(fingerprints[weapon], fingerprints[weapon + 1]);
        }
    }

    #[test]
    fn hook_atlas_chain_head_and_retraction_are_rendered() {
        let skin = skin();
        let game = game();
        let mut hooked = state(100, 100, -1);
        hooked.items[0].words[9] = 5;
        hooked.items[0].words[11] = 200;
        hooked.items[0].words[12] = 100;
        let mut frame = Framebuffer::new(220, 200).unwrap();
        render_animated_snapshot(&mut frame, &hooked, camera(),
                                 Some(&skin), Some(&game), 10).unwrap();
        assert!(frame.pixels.contains(&0xb400b4), "hook chain sprite missing");
        assert!(frame.pixels.contains(&0xffff00), "hook head sprite missing");

        let mut unhooked = hooked;
        unhooked.items[0].words[9] = 0;
        let mut frame = Framebuffer::new(220, 200).unwrap();
        render_animated_snapshot(&mut frame, &unhooked, camera(),
                                 Some(&skin), Some(&game), 10).unwrap();
        assert!(!frame.pixels.contains(&0xb400b4));
        assert!(!frame.pixels.contains(&0xffff00));
    }

    #[test]
    fn walking_feet_change_with_ticks_but_idle_pose_is_stable() {
        let skin = skin();
        let mut walking = state(100, 100, -1);
        walking.items[0].words[3] = 1280; // game velocity fixed-point
        let mut initial = Framebuffer::new(220, 200).unwrap();
        let mut later = Framebuffer::new(220, 200).unwrap();
        render_animated_snapshot(&mut initial, &walking, camera(),
                                 Some(&skin), None, 20).unwrap();
        render_animated_snapshot(&mut later, &walking, camera(),
                                 Some(&skin), None, 29).unwrap();
        assert_ne!(initial.pixels, later.pixels);

        walking.items[0].words[3] = 0;
        initial.clear(BACKGROUND);
        later.clear(BACKGROUND);
        render_animated_snapshot(&mut initial, &walking, camera(),
                                 Some(&skin), None, 20).unwrap();
        render_animated_snapshot(&mut later, &walking, camera(),
                                 Some(&skin), None, 29).unwrap();
        assert_eq!(initial.pixels, later.pixels);
    }

    #[test]
    fn recent_weapon_attack_renders_recoil_and_muzzle_without_game_state_changes() {
        let skin = skin();
        let mut atlas = game();
        // Muzzle must be a unique color rather than the weapon body.
        let cell = (atlas.width / 32) as usize;
        for y in 4 * cell..6 * cell {
            for x in 8 * cell..12 * cell {
                let at = (y * atlas.width as usize + x) * 4;
                atlas.rgba[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let mut firing = state(100, 100, 1);
        firing.items[0].words[21] = 98;
        let unchanged = firing.clone();
        let mut active = Framebuffer::new(220, 200).unwrap();
        render_animated_snapshot(&mut active, &firing, camera(),
                                 Some(&skin), Some(&atlas), 100).unwrap();
        assert!(active.pixels.contains(&0xffffff));
        let mut stale = Framebuffer::new(220, 200).unwrap();
        render_animated_snapshot(&mut stale, &firing, camera(),
                                 Some(&skin), Some(&atlas), 120).unwrap();
        assert!(!stale.pixels.contains(&0xffffff));
        assert_eq!(firing, unchanged);
    }

    #[test]
    fn rejects_invalid_camera_and_oversized_hook_coordinates() {
        let mut frame = Framebuffer::new(32, 32).unwrap();
        let invalid = Camera {
            center: crate::physics::Vec2::new(f32::NAN, 0.0),
            scale: 1.0,
        };
        assert_eq!(render_animated_snapshot(
            &mut frame, &state(0, 0, 1), invalid,
            Some(&skin()), Some(&game()), 0,
        ), Err(RenderError::InvalidCamera));
        let mut snap = state(i32::MAX, i32::MIN, 0);
        snap.items[0].words[9] = 5;
        snap.items[0].words[11] = i32::MIN;
        snap.items[0].words[12] = i32::MAX;
        assert_eq!(render_animated_snapshot(
            &mut frame, &snap, camera(),
            Some(&skin()), Some(&game()), 0,
        ), Ok(1));
    }
}
