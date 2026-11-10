//! Native GPU geometry for DDNet 0.6 tee bodies, eyes, feet, weapons,
//! muzzle flashes and hook chains. Reuses the CPU renderer's atlas regions
//! and timing approximations; no per-pixel CPU tee rasterization is required.
//! Original DDNet pose, skins and emote parity are still incomplete.

use crate::gpu_scene::{GpuBatch, GpuVertex, MAX_FRAME_VERTICES};
use crate::graphics::{Camera, RenderError, HOOK};
use crate::netobjects::{
    CharacterCoreNet, LaserNet, PickupNet, ProjectileNet, SwitchStateNet,
    CHARACTER_TYPE,
};
use crate::snapshot_delta::SnapshotState;
use crate::sprite_render::{
    Sprite, GRID_SKIN, GRID_GAME, BODY, FOOT, HOOK_HEAD, HOOK_CHAIN,
    MAX_HOOK_DISTANCE, skin_eye, weapon_sprite, muzzle_sprite, valid_sheet,
    pickup_art, pickup_position, pickup_orientation, pickup_visible,
    projectile_visual,
};
use crate::texture::Texture;

#[derive(Debug, Clone, Copy)]
pub struct SpriteTextureSlots {
    pub white: usize,
    pub skin: usize,
    pub game: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuTeeScene {
    pub vertices: Vec<GpuVertex>,
    pub batches: Vec<GpuBatch>,
    pub characters: usize,
}

struct TeeBuilder {
    scene: GpuTeeScene,
    width: f32,
    height: f32,
    slots: SpriteTextureSlots,
}

impl TeeBuilder {
    fn push(
        &mut self, atlas: usize, uv: [[f32; 2]; 4], center: (f32, f32),
        size: (f32, f32), orientation: (f32, bool), color: [u8; 4],
    ) {
        let (angle, flip_y) = orientation;
        if !center.0.is_finite() || !center.1.is_finite()
            || !size.0.is_finite() || !size.1.is_finite() || !angle.is_finite()
            || size.0 <= 0.0 || size.1 <= 0.0
            || self.scene.vertices.len() > MAX_FRAME_VERTICES - 6
        {
            return;
        }
        let (sin, cos) = angle.sin_cos();
        let pts = [(-0.5, -0.5), (0.5, -0.5), (-0.5, 0.5), (0.5, 0.5)];
        let mut corners = [GpuVertex {
            clip: [0.0; 2], uv: [0.0; 2], color,
        }; 4];
        for (i, (px, py)) in pts.into_iter().enumerate() {
            let dx = px * size.0;
            let dy = py * size.1;
            let x = center.0 + dx * cos - dy * sin;
            let y = center.1 + dx * sin + dy * cos;
            let mut tex_uv = uv[i];
            if flip_y {
                // Flip vertical atlas coordinate in sprite-local space; the
                // CPU blitter uses exactly the same inverse v transform.
                tex_uv[1] = uv[if i == 0 { 2 } else if i == 1 { 3 }
                                else if i == 2 { 0 } else { 1 }][1];
            }
            corners[i] = GpuVertex {
                clip: [2.0 * x / self.width - 1.0, 1.0 - 2.0 * y / self.height],
                uv: tex_uv,
                color,
            };
        }
        if corners.iter().all(|v| v.clip[0] < -1.0)
            || corners.iter().all(|v| v.clip[0] > 1.0)
            || corners.iter().all(|v| v.clip[1] < -1.0)
            || corners.iter().all(|v| v.clip[1] > 1.0)
        {
            return;
        }
        let start = self.scene.vertices.len() as u32;
        for index in [0, 1, 2, 1, 3, 2] {
            self.scene.vertices.push(corners[index]);
        }
        if let Some(last) = self.scene.batches.last_mut() {
            if last.texture == atlas && last.start + last.count == start {
                last.count += 6;
                return;
            }
        }
        self.scene.batches.push(GpuBatch {
            texture: atlas, start, count: 6, clip: None,
        });
    }

    fn sprite(
        &mut self, atlas: usize, grid: (usize, usize), region: Sprite,
        center: (f32, f32), size: (f32, f32), orientation: (f32, bool),
    ) {
        if region.w == 0 || region.h == 0
            || region.x.checked_add(region.w).is_none_or(|v| v > grid.0)
            || region.y.checked_add(region.h).is_none_or(|v| v > grid.1)
        {
            return;
        }
        let u0 = region.x as f32 / grid.0 as f32;
        let u1 = (region.x + region.w) as f32 / grid.0 as f32;
        let v0 = region.y as f32 / grid.1 as f32;
        let v1 = (region.y + region.h) as f32 / grid.1 as f32;
        self.push(atlas, [[u0,v0],[u1,v0],[u0,v1],[u1,v1]],
            center, size, orientation, [255; 4]);
    }

    fn line(&mut self, start: (f32,f32), end: (f32,f32), color: u32) {
        let dx = end.0 - start.0;
        let dy = end.1 - start.1;
        let length = dx.hypot(dy);
        if !length.is_finite() || !(0.05..=32768.0).contains(&length) { return; }
        self.push(self.slots.white, [[0.0,0.0],[1.0,0.0],
            [0.0,1.0],[1.0,1.0]],
            ((start.0 + end.0)*0.5, (start.1 + end.1)*0.5),
            (length, 1.0), (dy.atan2(dx), false), [
                ((color>>16)&255) as u8,
                ((color>>8)&255) as u8,
                (color&255) as u8,
                255,
            ]);
    }

    fn screen(&self, camera: Camera, x: f32, y: f32) -> (f32,f32) {
        (
            (x - camera.center.x) * camera.scale + self.width * 0.5,
            (y - camera.center.y) * camera.scale + self.height * 0.5,
        )
    }

    fn hook(
        &mut self, character: CharacterCoreNet, camera: Camera, game: bool,
    ) {
        if character.hook_state <= 0 { return; }
        let dx = character.hook_x as f32 - character.x as f32;
        let dy = character.hook_y as f32 - character.y as f32;
        let distance = dx.hypot(dy);
        if !(1.0..=MAX_HOOK_DISTANCE).contains(&distance) { return; }
        let start = self.screen(camera, character.x as f32, character.y as f32);
        let tip = self.screen(camera, character.hook_x as f32, character.hook_y as f32);
        if !game {
            self.line(start, tip, HOOK);
            return;
        }
        let angle = dy.atan2(dx);
        let count = (distance / 24.0).ceil().min(90.0) as usize;
        for index in 1..count {
            let span = index as f32 * 24.0;
            if span >= distance { break; }
            let x = character.hook_x as f32 - dx * span / distance;
            let y = character.hook_y as f32 - dy * span / distance;
            let center = self.screen(camera, x, y);
            self.sprite(self.slots.game, GRID_GAME, HOOK_CHAIN, center,
                (13.0 * camera.scale, 16.0 * camera.scale),
                (angle + std::f32::consts::FRAC_PI_2, false));
        }
        self.sprite(self.slots.game, GRID_GAME, HOOK_HEAD, tip,
            (24.0 * camera.scale, 12.0 * camera.scale), (angle, false));
    }

    fn tee(
        &mut self, character: CharacterCoreNet, words: &[i32],
        camera: Camera, tick: u64, game: bool,
    ) {
        let px = character.x as f32;
        let py = character.y as f32;
        let scale = camera.scale;
        let speed = character.velocity_x as f32 / 256.0;
        let walking = speed.abs() > 0.3 && character.jumped & 1 == 0;
        let phase = (tick % 10_000) as f32 * 0.34 + px / 32.0;
        let stride = if walking { phase.sin() * 7.0 } else { 0.0 };
        let bounce = if walking { phase.cos().abs() * 2.0 } else { 0.0 };
        let center = self.screen(camera, px, py - bounce);
        if center.0 < -128.0 || center.0 > self.width + 128.0
            || center.1 < -128.0 || center.1 > self.height + 128.0
        {
            return;
        }
        for (offset_x, offset_y) in [
            (-8.0 + stride, 12.0 - stride.abs() * 0.5),
            (8.0 - stride, 12.0 + stride.abs() * 0.5),
        ] {
            let foot = (center.0 + offset_x * scale, center.1 + offset_y * scale);
            self.sprite(self.slots.skin, GRID_SKIN, FOOT, foot,
                (15.0 * scale, 12.0 * scale), (0.0, false));
        }
        self.sprite(self.slots.skin, GRID_SKIN, BODY, center,
            (34.0 * scale, 32.0 * scale), (0.0, false));
        let emote = words.get(20).copied().unwrap_or(0);
        let facing = if character.direction < 0 { -1.0 } else { 1.0 };
        let blink = emote == 5 || tick % 201 < 3;
        self.sprite(self.slots.skin, GRID_SKIN, skin_eye(emote),
            (center.0 + facing * 6.0 * scale, center.1 - 3.0 * scale),
            (13.0 * scale, if blink { 3.0 * scale } else { 11.0 * scale }),
            (0.0, facing < 0.0));
        if !game { return; }
        let weapon = words.get(19).copied().unwrap_or(-1);
        let Some(sprite) = weapon_sprite(weapon) else { return; };
        let angle = (character.angle as f32 / 256.0)
            .clamp(-std::f32::consts::TAU, std::f32::consts::TAU);
        let aim_x = angle.cos();
        let aim_y = angle.sin();
        let mirrored = aim_x < 0.0;
        let attack = words.get(21).copied().unwrap_or(0);
        let age = (tick as i64).saturating_sub(i64::from(attack));
        let recoil = if attack > 0 && (0..=5).contains(&age) {
            (5-age) as f32 * 1.2
        } else { 0.0 };
        let weapon_center = (
            center.0 + aim_x * (23.0-recoil) * scale,
            center.1 + aim_y * (23.0-recoil) * scale + 2.0*scale,
        );
        let (w,h) = if weapon == 0 { (32.0,28.0) }
            else if weapon == 1 { (30.0,17.0) }
            else { (38.0,19.0) };
        self.sprite(self.slots.game, GRID_GAME, sprite, weapon_center,
            (w*scale,h*scale), (angle, mirrored));
        if attack > 0 && (0..=2).contains(&age) {
            if let Some(muzzle) = muzzle_sprite(weapon) {
                let at = (
                    weapon_center.0 + aim_x * w * 0.56 * scale,
                    weapon_center.1 + aim_y * w * 0.56 * scale,
                );
                self.sprite(self.slots.game, GRID_GAME, muzzle, at,
                    (22.0*scale,14.0*scale), (angle, mirrored));
            }
        }
    }
}

/// All chain sprites are behind *all* characters, mirroring the CPU
/// renderer's two-pass ordering. The font/HUD remains a separate overlay.
pub fn build_gpu_tees(
    state: &SnapshotState, camera: Camera, tick: u64,
    viewport: (u32, u32), skin: Option<&Texture>, game: Option<&Texture>,
    slots: SpriteTextureSlots,
) -> Result<GpuTeeScene, RenderError> {
    let mut result = GpuTeeScene::default();
    build_gpu_tees_into(&mut result, state, camera, tick, viewport,
        (skin, game), slots)?;
    Ok(result)
}

/// Rebuild the native tee/hook batches without reallocating their CPU
/// vertex storage. The caller keeps this scene across presentation frames.
/// The same two-pass ordering as build_gpu_tees is guaranteed.
pub fn build_gpu_tees_into(
    output: &mut GpuTeeScene,
    state: &SnapshotState, camera: Camera, tick: u64,
    viewport: (u32, u32), textures: (Option<&Texture>, Option<&Texture>),
    slots: SpriteTextureSlots,
) -> Result<(), RenderError> {
    if !(0.05..=16.0).contains(&camera.scale)
        || !camera.center.x.is_finite() || !camera.center.y.is_finite()
        || viewport.0 == 0 || viewport.1 == 0
    {
        return Err(RenderError::InvalidCamera);
    }
    output.vertices.clear();
    output.batches.clear();
    output.characters = 0;
    let (skin, game) = textures;
    if skin.is_none_or(|s| !valid_sheet(s, GRID_SKIN)) {
        // Preserve reusable capacity, but never present stale tee sprites.
        return Ok(());
    }
    let game = game.is_some_and(|s| valid_sheet(s, GRID_GAME));
    let mut b = TeeBuilder {
        scene: std::mem::take(output),
        width: viewport.0 as f32, height: viewport.1 as f32, slots,
    };
    // Match software item rendering: projectiles and laser beams are
    // world-space geometry drawn behind tee hooks/skins and before HUD.
    // These shapes cost no full-screen pixel upload.
    let ddnet_laser_type = LaserNet::extended_type(state);
    for item in &state.items {
        let laser = LaserNet::from_object(item).or_else(|| {
            ddnet_laser_type.and_then(|kind| LaserNet::from_extended_object(item, kind))
        });
        if let Some(laser) = laser {
            let from = b.screen(camera, laser.from_x as f32, laser.from_y as f32);
            let to = b.screen(camera, laser.to_x as f32, laser.to_y as f32);
            let dx = to.0 - from.0;
            let dy = to.1 - from.1;
            let length = dx.hypot(dy);
            if length.is_finite() && (0.1..=32_768.0).contains(&length) {
                let normal = (-dy / length, dx / length);
                for offset in -5i32..=5 {
                    let shift = (normal.0 * offset as f32,
                        normal.1 * offset as f32);
                    let color = if offset.abs() >= 4 {
                        0x2c6592
                    } else {
                        0xb4ebff
                    };
                    b.line((from.0 + shift.0, from.1 + shift.1),
                        (to.0 + shift.0, to.1 + shift.1), color);
                }
            }
        }
    }
    if game {
        let ddnet_projectile_type = ProjectileNet::extended_type(state);
        for item in &state.items {
            let projectile = ProjectileNet::from_object(item).or_else(|| {
                ddnet_projectile_type.and_then(|kind|
                    ProjectileNet::from_extended_object(item, kind))
            });
            if let Some(projectile) = projectile {
                if let Some(((x, y), angle, sprite)) =
                    projectile_visual(projectile, tick)
                {
                    let center = b.screen(camera, x, y);
                    b.sprite(b.slots.game, GRID_GAME, sprite, center,
                        (32.0 * camera.scale, 32.0 * camera.scale),
                        (angle, false));
                }
            }
        }
        let ddnet_type = PickupNet::extended_type(state);
        let switches = SwitchStateNet::from_snapshot(state);
        for item in &state.items {
            let pickup = PickupNet::from_object(item).or_else(|| {
                ddnet_type.and_then(|kind| PickupNet::from_extended_object(item, kind))
            });
            if let Some(pickup) = pickup.filter(|&p|
                pickup_visible(p, switches, tick))
            {
                if let Some((sprite, size)) = pickup_art(pickup) {
                    let (x, y) = pickup_position(pickup, tick);
                    let center = b.screen(camera, x, y);
                    b.sprite(b.slots.game, GRID_GAME, sprite, center,
                        (size.0 * camera.scale, size.1 * camera.scale),
                        pickup_orientation(pickup.flags));
                }
            }
        }
    }
    for item in &state.items {
        if let Some(character) = CharacterCoreNet::from_object(item) {
            b.hook(character, camera, game);
        }
    }
    for item in &state.items {
        if let Some(character) = CharacterCoreNet::from_object(item) {
            let words = if item.type_id == CHARACTER_TYPE {
                item.words.as_slice()
            } else { &[] };
            b.tee(character, words, camera, tick, game);
            b.scene.characters += 1;
        }
    }
    *output = b.scene;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::Vec2;
    use crate::snapshot_delta::SnapshotObject;
    fn slots() -> SpriteTextureSlots {
        SpriteTextureSlots {white:1,skin:2,game:3}
    }
    fn tex(width:u32,height:u32)->Texture {
        Texture::from_rgba(width,height,vec![255; (width*height*4) as usize]).unwrap()
    }
    fn snap(weapon:i32,hook:bool)->SnapshotState {
        let mut words=vec![0;22];
        words[1]=50;words[2]=50;words[19]=weapon;words[6]=1;
        if hook {words[9]=5;words[11]=170;words[12]=50;}
        SnapshotState {items: vec![
            SnapshotObject {type_id:CHARACTER_TYPE,id:1,words},
        ]}
    }
    fn camera()->Camera {
        Camera {center:Vec2::new(50.0,50.0),scale:1.0}
    }
    #[test]
    fn native_tee_scene_reuses_memory_and_matches_fresh_build_each_frame() {
        let skin = tex(80, 40);
        let game = tex(320, 160);
        let character = snap(1, true);
        let mut result = GpuTeeScene::default();
        build_gpu_tees_into(&mut result, &character, camera(), 100,
            (400, 200), (Some(&skin), Some(&game)), slots()).unwrap();
        assert!(!result.vertices.is_empty());
        assert!(!result.batches.is_empty());
        let vertex_ptr = result.vertices.as_ptr();
        let batch_ptr = result.batches.as_ptr();
        for (tick, weapon) in [(101u64, 1), (102, 2), (110, 5)] {
            let character = snap(weapon, true);
            build_gpu_tees_into(&mut result, &character, camera(), tick,
                (400, 200), (Some(&skin), Some(&game)), slots()).unwrap();
            let expected = build_gpu_tees(&character, camera(), tick,
                (400, 200), Some(&skin), Some(&game), slots()).unwrap();
            assert_eq!(result, expected);
            assert_eq!(result.vertices.as_ptr(), vertex_ptr);
            assert_eq!(result.batches.as_ptr(), batch_ptr);
        }
        build_gpu_tees_into(&mut result, &character, camera(), 120,
            (400, 200), (None, Some(&game)), slots()).unwrap();
        assert_eq!(result.characters, 0);
        assert!(result.vertices.is_empty() && result.batches.is_empty());
        assert_eq!(result.vertices.as_ptr(), vertex_ptr);
        assert_eq!(result.batches.as_ptr(), batch_ptr);
        // Bad camera values cannot destroy the last valid geometry.
        let expected = build_gpu_tees(&character, camera(), 130,
            (400, 200), Some(&skin), Some(&game), slots()).unwrap();
        build_gpu_tees_into(&mut result, &character, camera(), 130,
            (400, 200), (Some(&skin), Some(&game)), slots()).unwrap();
        assert!(build_gpu_tees_into(&mut result, &character,
            Camera { scale: f32::NAN, ..camera() }, 131,
            (400, 200), (Some(&skin), Some(&game)), slots()).is_err());
        assert_eq!(result, expected);
    }

    #[test]
    fn native_gpu_renders_projectiles_and_laser_segments_behind_characters() {
        let shot = SnapshotObject {
            type_id: crate::netobjects::PROJECTILE_TYPE,
            id: 4, words: vec![50, 50, 100, 0, 1, 100],
        };
        let beam = SnapshotObject {
            type_id: crate::netobjects::LASER_TYPE,
            id: 5, words: vec![110, 70, 40, 70, 100],
        };
        let state = SnapshotState { items: vec![shot, beam] };
        let scene = build_gpu_tees(&state, camera(), 100, (200, 120),
            Some(&tex(80, 40)), Some(&tex(320, 160)), slots()).unwrap();
        assert_eq!(scene.characters, 0);
        assert_eq!(scene.vertices.len(), 11 * 6 + 6);
        assert_eq!(scene.batches[0].texture, slots().white);
        assert_eq!(scene.batches[1].texture, slots().game);
        assert_eq!(scene.vertices[66].uv, [6.0 / 32.0, 4.0 / 16.0]);
        let empty = build_gpu_tees(&state, camera(), 100, (200, 120),
            Some(&tex(80, 40)), None, slots()).unwrap();
        assert_eq!(empty.vertices.len(), 11 * 6);
        assert!(empty.batches.iter().all(|b| b.texture == slots().white));
    }

    #[test]
    fn native_gpu_pickups_use_game_png_and_preserve_render_order() {
        let state = SnapshotState {
            items: vec![
                SnapshotObject {
                    type_id: crate::netobjects::PICKUP_TYPE,
                    id: 1, words: vec![40, 40, 0, 0],
                },
                SnapshotObject {
                    type_id: crate::netobjects::PICKUP_TYPE,
                    id: 2, words: vec![60, 40, 2, 1],
                },
            ],
        };
        let camera = Camera {
            center: Vec2::new(50.0, 40.0), scale: 1.0,
        };
        let visuals = build_gpu_tees(&state, camera, 100,
            (200, 120), Some(&tex(80, 40)), Some(&tex(320, 160)),
            slots()).unwrap();
        assert_eq!(visuals.characters, 0);
        assert_eq!(visuals.vertices.len(), 12);
        assert_eq!(visuals.batches.len(), 1);
        assert_eq!(visuals.batches[0].texture, slots().game);
        assert_eq!(visuals.vertices[0].uv, [10.0 / 32.0, 2.0 / 16.0]);
        assert_eq!(visuals.vertices[6].uv, [2.0 / 32.0, 4.0 / 16.0]);
        let mut no_game = GpuTeeScene::default();
        build_gpu_tees_into(&mut no_game, &state, camera, 100,
            (200, 120), (Some(&tex(80, 40)), None),
            slots()).unwrap();
        assert_eq!(no_game.vertices.len(), 0);
    }

    #[test]
    fn body_feet_eye_and_weapon_are_gpu_geometry_not_cpu_pixels() {
        let skin=tex(80,40);let game=tex(320,160);
        let scene=build_gpu_tees(&snap(1,false),camera(),100,(100,100),
            Some(&skin),Some(&game),slots()).unwrap();
        assert_eq!(scene.characters,1);
        assert_eq!(scene.vertices.len(),30); // 2 feet + body + eyes + gun
        assert_eq!(scene.batches.len(),2); // skin batch, game batch
        assert_eq!(scene.batches[0].texture,2);
        assert_eq!(scene.batches[0].count,24);
        assert_eq!(scene.batches[1].texture,3);
        assert_eq!(scene.batches[1].count,6);
        assert_eq!(scene.vertices[0].color,[255;4]);
    }
    #[test]
    fn hook_chain_draws_before_all_tees_and_fires_muzzle_only_when_recent() {
        let skin=tex(80,40);let game=tex(320,160);
        let mut state=snap(1,true);
        state.items[0].words[21]=100;
        let active=build_gpu_tees(&state,camera(),100,(400,200),
            Some(&skin),Some(&game),slots()).unwrap();
        assert!(active.vertices.len()>36);
        assert_eq!(active.batches[0].texture,3);
        assert_eq!(active.batches[1].texture,2);
        assert_eq!(active.batches.last().unwrap().texture,3);
        let idle=build_gpu_tees(&state,camera(),200,(400,200),
            Some(&skin),Some(&game),slots()).unwrap();
        assert_eq!(active.vertices.len(),idle.vertices.len()+6);
    }
    #[test]
    fn no_skins_causes_fallback_and_offscreen_characters_are_culled() {
        let game=tex(320,160);let skin=tex(80,40);
        let state=snap(4,false);
        let none=build_gpu_tees(&state,camera(),0,(100,100),
            None,Some(&game),slots()).unwrap();
        assert!(none.vertices.is_empty());
        let far=Camera {center:Vec2::new(-3000.0,-3000.0),scale:1.0};
        let scene=build_gpu_tees(&state,far,0,(100,100),
            Some(&skin),Some(&game),slots()).unwrap();
        assert!(scene.vertices.is_empty());
        assert!(build_gpu_tees(&state,Camera {center:far.center,scale:f32::NAN},
            0,(100,100),Some(&skin),Some(&game),slots()).is_err());
    }
    #[test]
    fn all_six_weapons_and_mirror_eyes_have_valid_uvs() {
        let skin=tex(80,40);let game=tex(320,160);
        for weapon in 0..=5 {
            let mut state=snap(weapon,false);
            state.items[0].words[6] = -1;
            state.items[0].words[5]=(std::f32::consts::PI*256.0) as i32;
            let scene=build_gpu_tees(&state,camera(),99,(200,200),
                Some(&skin),Some(&game),slots()).unwrap();
            assert_eq!(scene.vertices.len(),30);
            assert!(scene.vertices.iter().all(|v|
                v.uv.iter().all(|n| (0.0..=1.0).contains(n))
                && v.clip.iter().all(|n| n.is_finite())));
        }
    }
}
