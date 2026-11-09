//! Vanilla DDNet game-layer extraction and elementary collision queries.
//!
//! Source of truth: game/mapitems.h, game/layers.cpp and game/collision.cpp in
//! upstream DDNet 20.2.1. This is not the full DDNet collision system:
//! Door activation, teleport destinations, front/switch effects and complete
//! movement restrictions still need implementation. Special DDRace layers
//! are decoded before collision prediction to avoid false extrapolation.

use crate::datafile::{Datafile, DatafileError};
use crate::physics::{Tuning, Vec2};

const MAPITEMTYPE_LAYER: u16 = 5;
const LAYERTYPE_TILES: i32 = 2;
const TILESLAYERFLAG_GAME: i32 = 1;
const TILESLAYERFLAG_TELE: i32 = 1 << 1;
const TILESLAYERFLAG_SPEEDUP: i32 = 1 << 2;
const TILESLAYERFLAG_FRONT: i32 = 1 << 3;
const TILESLAYERFLAG_SWITCH: i32 = 1 << 4;
const TILESLAYERFLAG_TUNE: i32 = 1 << 5;
pub const TILE_SPEED_BOOST_OLD: u8 = 28;
pub const TILE_SPEED_BOOST: u8 = 29;
pub const TILE_TELE_IN: u8 = 26;
pub const TILE_TELE_OUT: u8 = 27;
pub const TILE_TELE_CHECK_OUT: u8 = 30;
pub const TILE_TELE_CHECK_IN: u8 = 31;
pub const TILE_TELE_EVIL_IN: u8 = 10;
pub const TILE_TELE_CHECK_EVIL_IN: u8 = 63;
const TILE_SOLID: u8 = 1;
const TILE_NOHOOK: u8 = 3;
const TILE_NOLASER: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    MissingGameLayer,
    InvalidTileLayer,
    InvalidDimensions,
    InvalidTileData,
    InvalidTuneLayer,
    InvalidTuneData,
    InvalidSpecialLayer,
    InvalidSpecialData,
    InvalidMotion,
    MotionTooLarge,
    Datafile(DatafileError),
}

impl From<DatafileError> for MapError {
    fn from(value: DatafileError) -> Self {
        Self::Datafile(value)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tile {
    pub index: u8,
    pub flags: u8,
    pub skip: u8,
    pub reserved: u8,
}

/// CTuneTile, exactly two bytes: m_Number followed by m_Type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuneTile {
    pub number: u8,
    pub tile_type: u8,
}

/// CTeleTile in game/mapitems.h: two unsigned bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TeleTile {
    pub number: u8,
    pub tile_type: u8,
}

/// CSpeedupTile: three u8 fields, reserved byte and little-endian i16 angle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpeedupTile {
    pub force: u8,
    pub max_speed: u8,
    pub tile_type: u8,
    pub reserved: u8,
    pub angle: i16,
}

/// CSwitchTile uses four unsigned bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwitchTile {
    pub number: u8,
    pub tile_type: u8,
    pub flags: u8,
    pub delay: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameMap {
    pub width: usize,
    pub height: usize,
    pub tiles: Vec<Tile>,
    /// Server-defined tune parameters are not encoded in this layer.
    tune_tiles: Option<Vec<TuneTile>>,
    speedup_tiles: Option<Vec<SpeedupTile>>,
    tele_tiles: Option<Vec<TeleTile>>,
    /// Indexed by the map's visible teleporter number (1..=255).
    /// The server randomly selects an exit when there are several.
    tele_outs: Vec<Vec<usize>>,
    tele_check_outs: Vec<Vec<usize>>,
    front_tiles: Option<Vec<Tile>>,
    switch_tiles: Option<Vec<SwitchTile>>,
}

fn layer_int(payload: &[u8], offset: usize) -> Option<i32> {
    let bytes: [u8; 4] = payload.get(offset..offset + 4)?.try_into().ok()?;
    Some(i32::from_le_bytes(bytes))
}

/// Load only a genuinely declared DDRace collision layer. Legacy v2
/// m_Tele/m_Speedup/m_Front/m_Switch/m_Tune offsets start at 60/64/68/72/76;
/// for v3+ the 12-byte name moves them to 72/76/80/84/88.
fn load_special_layer(
    file: &Datafile<'_>, width: usize, height: usize, count: usize,
    flag: i32, old_offset: usize, record_size: usize,
) -> Result<Option<Vec<u8>>, MapError> {
    let Some(item) = file.items.iter().find(|item| {
        item.type_id == MAPITEMTYPE_LAYER
            && layer_int(item.payload, 4) == Some(LAYERTYPE_TILES)
            && layer_int(item.payload, 24).is_some_and(|flags| {
                flags & TILESLAYERFLAG_GAME == 0 && flags & flag != 0
            })
    }) else { return Ok(None); };

    let version = layer_int(item.payload, 12).ok_or(MapError::InvalidSpecialLayer)?;
    let offset = if version >= 3 { old_offset + 12 } else { old_offset };
    if version < 2 || item.payload.len() < offset + 4
        || layer_int(item.payload, 16) != Some(width as i32)
        || layer_int(item.payload, 20) != Some(height as i32)
    {
        return Err(MapError::InvalidSpecialLayer);
    }
    let index = usize::try_from(
        layer_int(item.payload, offset).ok_or(MapError::InvalidSpecialLayer)?,
    ).map_err(|_| MapError::InvalidSpecialLayer)?;
    let bytes = count.checked_mul(record_size).ok_or(MapError::InvalidDimensions)?;
    let raw = file.raw_data(index, bytes)?;
    if raw.len() != bytes { return Err(MapError::InvalidSpecialData); }
    Ok(Some(raw))
}

impl GameMap {
    /// Extract the official physics game tile layer.
    /// `max_tiles` prevents untrusted map files from allocating huge grids.
    pub fn from_datafile(file: &Datafile<'_>, max_tiles: usize) -> Result<Self, MapError> {
        let item = file
            .items
            .iter()
            .find(|item| {
                item.type_id == MAPITEMTYPE_LAYER
                    && layer_int(item.payload, 4) == Some(LAYERTYPE_TILES)
                    && layer_int(item.payload, 24)
                        .is_some_and(|flags| flags & TILESLAYERFLAG_GAME != 0)
            })
            .ok_or(MapError::MissingGameLayer)?;

        if item.payload.len() < 60 {
            return Err(MapError::InvalidTileLayer);
        }
        let width = usize::try_from(
            layer_int(item.payload, 16).ok_or(MapError::InvalidTileLayer)?,
        )
        .map_err(|_| MapError::InvalidDimensions)?;
        let height = usize::try_from(
            layer_int(item.payload, 20).ok_or(MapError::InvalidTileLayer)?,
        )
        .map_err(|_| MapError::InvalidDimensions)?;
        let tile_count = width.checked_mul(height).ok_or(MapError::InvalidDimensions)?;
        if width == 0 || height == 0 || tile_count > max_tiles {
            return Err(MapError::InvalidDimensions);
        }

        // The first 60 bytes use CMapItemLayerTilemap_v2 layout even for v3+
        // and legacy DDRace extensions. m_Data is at byte offset 56.
        let data_index = usize::try_from(
            layer_int(item.payload, 56).ok_or(MapError::InvalidTileLayer)?,
        )
        .map_err(|_| MapError::InvalidTileLayer)?;
        let tile_bytes = tile_count.checked_mul(4).ok_or(MapError::InvalidDimensions)?;
        let data = file.raw_data(data_index, tile_bytes)?;
        let mut map = Self::from_tile_bytes(width, height, &data)?;
        if let Some(raw) = load_special_layer(
            file, width, height, tile_count, TILESLAYERFLAG_TUNE, 76, 2,
        ).map_err(|error| match error {
            MapError::InvalidSpecialLayer => MapError::InvalidTuneLayer,
            MapError::InvalidSpecialData => MapError::InvalidTuneData,
            other => other,
        })? {
            map.tune_tiles = Some(raw.chunks_exact(2)
                .map(|v| TuneTile { number: v[0], tile_type: v[1] }).collect());
        }
        if let Some(raw) = load_special_layer(
            file, width, height, tile_count, TILESLAYERFLAG_SPEEDUP, 64, 6,
        )? {
            map.speedup_tiles = Some(raw.chunks_exact(6).map(|v| SpeedupTile {
                force: v[0], max_speed: v[1], tile_type: v[2],
                reserved: v[3], angle: i16::from_le_bytes([v[4], v[5]]),
            }).collect());
        }
        if let Some(raw) = load_special_layer(
            file, width, height, tile_count, TILESLAYERFLAG_TELE, 60, 2,
        )? {
            let tiles: Vec<TeleTile> = raw.chunks_exact(2)
                .map(|v| TeleTile { number: v[0], tile_type: v[1] }).collect();
            // Same grouping as CCollision::Init: number N uses bucket N-1,
            // and CTeleTile at (x,y) represents its pixel center (32x+16,32y+16).
            // Store grid indices to keep GameMap Eq and avoid allocating Vec2
            // objects for every teleport destination.
            let mut outs = vec![Vec::new(); 256];
            let mut check_outs = vec![Vec::new(); 256];
            for (index, tile) in tiles.iter().enumerate() {
                if tile.number == 0 { continue; }
                match tile.tile_type {
                    TILE_TELE_OUT => outs[usize::from(tile.number)].push(index),
                    TILE_TELE_CHECK_OUT =>
                        check_outs[usize::from(tile.number)].push(index),
                    _ => {}
                }
            }
            map.tele_outs = outs;
            map.tele_check_outs = check_outs;
            map.tele_tiles = Some(tiles);
        }
        if let Some(raw) = load_special_layer(
            file, width, height, tile_count, TILESLAYERFLAG_FRONT, 68, 4,
        )? {
            map.front_tiles = Some(raw.chunks_exact(4).map(|v| Tile {
                index: v[0], flags: v[1], skip: v[2], reserved: v[3],
            }).collect());
        }
        if let Some(raw) = load_special_layer(
            file, width, height, tile_count, TILESLAYERFLAG_SWITCH, 72, 4,
        )? {
            map.switch_tiles = Some(raw.chunks_exact(4).map(|v| SwitchTile {
                number: v[0], tile_type: v[1], flags: v[2], delay: v[3],
            }).collect());
        }
        Ok(map)
    }

    /// Parse CTile records (index, flags, skip, reserved) with explicit
    /// bounds; the raw game layer has exactly width * height * 4 bytes.
    pub fn from_tile_bytes(
        width: usize,
        height: usize,
        bytes: &[u8],
    ) -> Result<Self, MapError> {
        let count = width.checked_mul(height).ok_or(MapError::InvalidDimensions)?;
        if width == 0 || height == 0
            || count.checked_mul(4).ok_or(MapError::InvalidDimensions)? != bytes.len()
        {
            return Err(MapError::InvalidTileData);
        }

        let tiles = bytes
            .chunks_exact(4)
            .map(|tile| Tile {
                index: tile[0],
                flags: tile[1],
                skip: tile[2],
                reserved: tile[3],
            })
            .collect();
        Ok(Self {
            width, height, tiles, tune_tiles: None, speedup_tiles: None,
            tele_tiles: None, tele_outs: Vec::new(),
            tele_check_outs: Vec::new(), front_tiles: None, switch_tiles: None,
        })
    }

    pub fn tile(&self, x: usize, y: usize) -> Option<Tile> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.tiles.get(y * self.width + x).copied()
    }

    fn tile_index_at(&self, pos: Vec2) -> Option<usize> {
        if !pos.x.is_finite() || !pos.y.is_finite() { return None; }
        let x = (pos.x as i32 / 32).clamp(0, self.width as i32 - 1) as usize;
        let y = (pos.y as i32 / 32).clamp(0, self.height as i32 - 1) as usize;
        Some(y * self.width + x)
    }

    /// A CTeleTile with zero number/type has no active teleport behavior.
    pub fn tele_tile_at(&self, position: Vec2) -> Option<TeleTile> {
        let tile = *self.tele_tiles.as_ref()?.get(self.tile_index_at(position)?)?;
        (tile.number != 0 && tile.tile_type != 0).then_some(tile)
    }

    /// An unambiguous normal exit is the only destination the client may
    /// know without the server's random choice. No exit or >1 exits -> None.
    pub fn unique_tele_exit(&self, number: u8) -> Option<Vec2> {
        let &[index] = self.tele_outs.get(usize::from(number))?.as_slice() else {
            return None;
        };
        Some(self.tile_center(index))
    }

    /// Count actual normal teleporter destinations, for diagnostics.
    pub fn tele_exit_count(&self, number: u8) -> usize {
        self.tele_outs.get(usize::from(number)).map_or(0, Vec::len)
    }

    /// Server checkpoint teleport logic searches checkpoint N down to 1
    /// and falls back to spawn when none exists. A multi-exit checkpoint
    /// cannot be predicted because the server chooses randomly.
    pub fn unique_checkpoint_exit(&self, last_checkpoint: u8) -> Option<Vec2> {
        for number in (1..=last_checkpoint).rev() {
            match self.tele_check_outs.get(usize::from(number))?.as_slice() {
                [] => continue,
                &[index] => return Some(self.tile_center(index)),
                _ => return None,
            }
        }
        None
    }

    fn tile_center(&self, index: usize) -> Vec2 {
        Vec2::new(
            (index % self.width) as f32 * 32.0 + 16.0,
            (index / self.width) as f32 * 32.0 + 16.0,
        )
    }

    /// Only tee-entry teleports can cause a coordinate discontinuity.
    /// Weapon/hook portal types and exit markers do not teleport tees.
    fn is_tee_tele_entry_at(&self, position: Vec2) -> bool {
        self.tele_tile_at(position).is_some_and(|tile| matches!(
            tile.tile_type, TILE_TELE_IN | TILE_TELE_EVIL_IN
                | TILE_TELE_CHECK_IN | TILE_TELE_CHECK_EVIL_IN
        ))
    }

    /// Detect teleports traversed within one server snapshot interval, even
    /// when the tee jumps across a single 32px portal between endpoints.
    pub fn crosses_tee_teleport(&self, start: Vec2, end: Vec2) -> bool {
        if self.tele_tiles.is_none() { return false; }
        let delta_x = end.x - start.x;
        let delta_y = end.y - start.y;
        let distance = delta_x.hypot(delta_y);
        if !distance.is_finite() || distance > 8192.0 { return true; }
        let steps = (distance / 16.0).ceil().max(1.0) as usize;
        (0..=steps).any(|i| {
            let t = i as f32 / steps as f32;
            self.is_tee_tele_entry_at(Vec2::new(
                start.x + delta_x * t, start.y + delta_y * t,
            ))
        })
    }

    /// Return only active speedup tiles; upstream IsSpeedup checks Force > 0.
    pub fn speedup_at(&self, pos: Vec2) -> Option<SpeedupTile> {
        let tile = *self.speedup_tiles.as_ref()?.get(self.tile_index_at(pos)?)?;
        (tile.force > 0).then_some(tile)
    }

    /// Apply TILE_SPEED_BOOST (29), which uses a capped directional
    /// acceleration. This is CCharacter::HandleSkippableTiles, *after* Move,
    /// not CCharacterCore::Tick. Return None for unsupported older types
    /// rather than inventing a velocity. Actual movement restrictions are
    /// checked separately by the caller before this function.
    pub fn apply_speedup(
        &self, position: Vec2, velocity: Vec2, tuning: &Tuning,
    ) -> Option<Vec2> {
        let Some(tile) = self.speedup_at(position) else { return Some(velocity); };
        if tile.tile_type != TILE_SPEED_BOOST { return None; }
        let radians = f32::from(tile.angle).to_radians();
        let direction = Vec2::new(radians.cos(), radians.sin());
        let max_speed = if tile.max_speed == 0 {
            let ramp = tuning.velramp_range /
                (50.0 * tuning.velramp_curvature.max(1.01).ln());
            let estimate = ramp.max(tuning.velramp_start / 50.0) * 5.0;
            if !estimate.is_finite() || !(1.0..=i32::MAX as f32).contains(&estimate) {
                return None;
            }
            (estimate as i32) as f32
        } else {
            f32::from(tile.max_speed)
        };
        let projected = direction.x * velocity.x + direction.y * velocity.y;
        let acceleration = (max_speed / 5.0 - projected).min(f32::from(tile.force));
        let output = Vec2::new(
            velocity.x + direction.x * acceleration,
            velocity.y + direction.y * acceleration,
        );
        (output.x.is_finite() && output.y.is_finite()).then_some(output)
    }

    /// Server teleport destinations and DDRace restrictions are not yet
    /// modeled. A special tile within 32px of the tee can alter a tick.
    /// Fail closed in local prediction instead of displaying fake physics.
    pub fn has_unsupported_physics_near(&self, pos: Vec2) -> bool {
        let Some(center) = self.tile_index_at(pos) else { return true; };
        let cx = center % self.width;
        let cy = center / self.width;
        let xmin = cx.saturating_sub(1);
        let ymin = cy.saturating_sub(1);
        let xmax = (cx + 1).min(self.width - 1);
        let ymax = (cy + 1).min(self.height - 1);
        for y in ymin..=ymax {
            for x in xmin..=xmax {
                let index = y * self.width + x;
                let game_index = self.tiles[index].index;
                // Freeze, death, stoppers and gameplay powerups alter movement
                // or control. Pure solid/nohook/nolaser tile states are safe.
                if !matches!(game_index, 0 | TILE_SOLID | TILE_NOHOOK | TILE_NOLASER) {
                    return true;
                }
                if self.front_tiles.as_ref().is_some_and(|layer| layer[index].index != 0)
                    // Exit markers do not teleport a tee and must not
                    // unnecessarily suppress movement prediction nearby.
                    || self.tele_tiles.as_ref().is_some_and(|layer| {
                        !matches!(layer[index].tile_type,
                            0 | TILE_TELE_OUT | TILE_TELE_CHECK_OUT)
                    })
                    || self.switch_tiles.as_ref().is_some_and(|layer| layer[index].tile_type != 0)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Track movement through physics tiles, not only at endpoints. A tee
    /// can cross a thin teleport or freeze tile in one tick at high speed.
    pub fn crosses_unsupported_physics(&self, start: Vec2, end: Vec2) -> bool {
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let distance = dx.hypot(dy);
        if !distance.is_finite() || distance > 8192.0 { return true; }
        let steps = (distance / 16.0).ceil().max(1.0) as usize;
        (0..=steps).any(|i| {
            let f = i as f32 / steps as f32;
            self.has_unsupported_physics_near(Vec2::new(
                start.x + dx * f, start.y + dy * f,
            ))
        })
    }

    /// CCollision::IsTune(GetMapIndex(pos)): nonzero type activates a zone.
    /// C++ GetMapIndex uses truncated pixel coordinates, then clamps.
    pub fn tune_zone_at(&self, pos: Vec2) -> u8 {
        let Some(tiles) = self.tune_tiles.as_ref() else { return 0; };
        if !pos.x.is_finite() || !pos.y.is_finite() { return 0; }
        let x = (pos.x as i32 / 32).clamp(0, self.width as i32 - 1) as usize;
        let y = (pos.y as i32 / 32).clamp(0, self.height as i32 - 1) as usize;
        match tiles.get(y * self.width + x) {
            Some(tile) if tile.tile_type != 0 => tile.number,
            _ => 0,
        }
    }

    /// Sample at most every half tile while moving between snapshot ticks:
    /// checking only endpoints can miss a narrow tune zone at high velocity.
    /// Refuse nonfinite/excessive motion rather than assuming vanilla tuning.
    pub fn crosses_tune_zone(&self, start: Vec2, end: Vec2) -> bool {
        if self.tune_tiles.is_none() { return false; }
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let distance = dx.hypot(dy);
        if !distance.is_finite() || distance > 8192.0 { return true; }
        let steps = (distance / 16.0).ceil().max(1.0) as usize;
        (0..=steps).any(|i| {
            let t = i as f32 / steps as f32;
            self.tune_zone_at(Vec2::new(start.x + dx * t, start.y + dy * t)) != 0
        })
    }

    /// A zone transition invalidates already received tuning during prediction.
    /// Sample the full segment so a narrow zone is not skipped at high speed.
    pub fn crosses_tune_zone_change(&self, start: Vec2, end: Vec2) -> bool {
        if self.tune_tiles.is_none() { return false; }
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let distance = dx.hypot(dy);
        if !distance.is_finite() || distance > 8192.0 { return true; }
        let origin_zone = self.tune_zone_at(start);
        let steps = (distance / 16.0).ceil().max(1.0) as usize;
        (0..=steps).any(|i| {
            let fraction = i as f32 / steps as f32;
            self.tune_zone_at(Vec2::new(
                start.x + dx * fraction, start.y + dy * fraction,
            )) != origin_zone
        })
    }

    /// Mirrors CCollision::GetTile(int x, int y): integer division by 32,
    /// clamped to the game layer bounds; only 1..4 are returned.
    pub fn collision_tile_at_pixel(&self, x: i32, y: i32) -> u8 {
        let tx = (x / 32).clamp(0, self.width as i32 - 1) as usize;
        let ty = (y / 32).clamp(0, self.height as i32 - 1) as usize;
        let index = self.tiles[ty * self.width + tx].index;
        if (TILE_SOLID..=TILE_NOLASER).contains(&index) {
            index
        } else {
            0
        }
    }

    /// DDNet CCollision::IsSolid only includes TILE_SOLID and TILE_NOHOOK.
    pub fn is_solid_pixel(&self, x: i32, y: i32) -> bool {
        matches!(self.collision_tile_at_pixel(x, y), TILE_SOLID | TILE_NOHOOK)
    }

    /// CCollision::CheckPoint uses round_to_int before IsSolid.
    pub fn check_point(&self, x: f32, y: f32) -> bool {
        self.is_solid_pixel(x.round() as i32, y.round() as i32)
    }

    /// Mirrors CCollision::TestBox: collision at the four AABB corners.
    pub fn test_box(&self, center: Vec2, size: Vec2) -> bool {
        let half_x = size.x * 0.5;
        let half_y = size.y * 0.5;
        self.check_point(center.x - half_x, center.y - half_y)
            || self.check_point(center.x + half_x, center.y - half_y)
            || self.check_point(center.x - half_x, center.y + half_y)
            || self.check_point(center.x + half_x, center.y + half_y)
    }

    /// Vanilla CCollision::MoveBox sub-step collision, with a safety cap
    /// on motion length to prevent untrusted values from freezing the client.
    /// Position/velocity are updated only on success.
    pub fn move_box(
        &self,
        position: &mut Vec2,
        velocity: &mut Vec2,
        size: Vec2,
        elasticity: Vec2,
        grounded: &mut bool,
    ) -> Result<(), MapError> {
        if ![
            position.x, position.y, velocity.x, velocity.y,
            size.x, size.y, elasticity.x, elasticity.y,
        ].iter().all(|x| x.is_finite()) || size.x < 0.0 || size.y < 0.0 {
            return Err(MapError::InvalidMotion);
        }

        let mut pos = *position;
        let mut vel = *velocity;
        let distance = vel.x.hypot(vel.y);
        if distance > 8192.0 {
            return Err(MapError::MotionTooLarge);
        }
        if distance > 0.00001 {
            let steps = distance as u32;
            let fraction = 1.0 / (steps as f32 + 1.0);
            let elastic_x = elasticity.x.clamp(-1.0, 1.0);
            let elastic_y = elasticity.y.clamp(-1.0, 1.0);

            for _ in 0..=steps {
                if vel == Vec2::default() {
                    break;
                }
                let mut next = Vec2::new(
                    pos.x + vel.x * fraction,
                    pos.y + vel.y * fraction,
                );
                if next == pos {
                    break;
                }
                if self.test_box(next, size) {
                    let mut hits = 0;
                    if self.test_box(Vec2::new(pos.x, next.y), size) {
                        if elastic_y > 0.0 && vel.y > 0.0 {
                            *grounded = true;
                        }
                        next.y = pos.y;
                        vel.y *= -elastic_y;
                        hits += 1;
                    }
                    if self.test_box(Vec2::new(next.x, pos.y), size) {
                        next.x = pos.x;
                        vel.x *= -elastic_x;
                        hits += 1;
                    }
                    if hits == 0 {
                        if elastic_y > 0.0 && vel.y > 0.0 {
                            *grounded = true;
                        }
                        next.y = pos.y;
                        vel.y *= -elastic_y;
                        next.x = pos.x;
                        vel.x *= -elastic_x;
                    }
                }
                pos = next;
            }
        }
        *position = pos;
        *velocity = vel;
        Ok(())
    }

    /// Port of CCollision::MovePoint. Returns the number of axis bounces;
    /// a diagonal corner collision without axis hits reflects both.
    pub fn move_point(
        &self,
        position: &mut Vec2,
        velocity: &mut Vec2,
        elasticity: f32,
    ) -> Result<u8, MapError> {
        if ![position.x, position.y, velocity.x, velocity.y, elasticity]
            .iter().all(|value| value.is_finite()) {
            return Err(MapError::InvalidMotion);
        }
        let next = Vec2::new(position.x + velocity.x, position.y + velocity.y);
        if !self.check_point(next.x, next.y) {
            *position = next;
            return Ok(0);
        }

        let mut hits = 0;
        if self.check_point(position.x + velocity.x, position.y) {
            velocity.x *= -elasticity;
            hits += 1;
        }
        if self.check_point(position.x, position.y + velocity.y) {
            velocity.y *= -elasticity;
            hits += 1;
        }
        if hits == 0 {
            velocity.x *= -elasticity;
            velocity.y *= -elasticity;
        }
        Ok(hits)
    }

    /// DDNet collision.cpp::IsOnGround. Position is the tee center.
    pub fn is_on_ground(&self, center: Vec2, physical_size: f32) -> bool {
        let y = center.y + physical_size / 2.0 + 5.0;
        self.check_point(center.x + physical_size / 2.0, y)
            || self.check_point(center.x - physical_size / 2.0, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{Character, Input, JumpEvent, Tuning};

    fn grid() -> GameMap {
        let indices: [u8; 9] = [
            0, 0, 0,
            0, 9, 0, // freeze tile is not solid
            1, 3, 2, // solid, nohook, death
        ];
        let mut data = Vec::new();
        for index in indices {
            data.extend_from_slice(&[index, 0, 0, 0]);
        }
        GameMap::from_tile_bytes(3, 3, &data).unwrap()
    }

    #[test]
    fn collision_matches_upstream_basic_tile_semantics() {
        let map = grid();
        assert_eq!(map.tile(1, 1).unwrap().index, 9);
        assert_eq!(map.collision_tile_at_pixel(48, 48), 0);
        assert_eq!(map.collision_tile_at_pixel(16, 80), 1);
        assert_eq!(map.collision_tile_at_pixel(48, 80), 3);
        assert_eq!(map.collision_tile_at_pixel(80, 80), 2);
        assert!(map.is_solid_pixel(16, 80));
        assert!(map.is_solid_pixel(48, 80));
        assert!(!map.is_solid_pixel(80, 80));
        assert!(!map.is_solid_pixel(48, 48));
    }

    #[test]
    fn x_y_clamp_and_truncate_match_ddnet() {
        let map = grid();
        assert_eq!(map.collision_tile_at_pixel(-500, 80), 1);
        assert_eq!(map.collision_tile_at_pixel(500, 80), 2);
        assert_eq!(map.collision_tile_at_pixel(48, -100), 0);
        assert_eq!(map.collision_tile_at_pixel(48, 500), 3);
        assert_eq!(map.tile(3, 1), None);
    }

    #[test]
    fn grounded_tick_uses_collision_grid() {
        let map = grid();
        let pos = Vec2::new(48.0, 48.0);
        assert!(map.is_on_ground(pos, 28.0));
        let mut c = Character::default();
        let event = c.tick_without_collision(
            Input { direction: 1, jump: true },
            map.is_on_ground(pos, 28.0),
            &Tuning::default(),
        );
        assert_eq!(event, JumpEvent::Ground);
        assert_eq!(c.velocity.y, -13.2);
        assert_eq!(c.velocity.x, 2.0);
    }

    #[test]
    fn moving_box_stops_at_vertical_wall() {
        let mut bytes = vec![0u8; 3 * 2 * 4];
        for y in 0..2 {
            bytes[(y * 3 + 2) * 4] = 1;
        }
        let map = GameMap::from_tile_bytes(3, 2, &bytes).unwrap();
        let mut pos = Vec2::new(48.0, 16.0);
        let mut vel = Vec2::new(20.0, 0.0);
        let mut grounded = false;
        map.move_box(
            &mut pos, &mut vel, Vec2::new(28.0, 28.0),
            Vec2::default(), &mut grounded
        ).unwrap();
        assert!(pos.x > 48.0 && pos.x < 50.0, "pos={pos:?}");
        assert_eq!(vel.x, 0.0);
        assert!(!grounded);
        assert!(!map.test_box(pos, Vec2::new(28.0, 28.0)));
    }

    #[test]
    fn moving_box_ground_contact_and_bounce() {
        let map = grid();
        let mut pos = Vec2::new(48.0, 48.0);
        let mut vel = Vec2::new(0.0, 10.0);
        let mut grounded = false;
        map.move_box(
            &mut pos, &mut vel, Vec2::new(28.0, 28.0),
            Vec2::new(0.0, 1.0), &mut grounded,
        ).unwrap();
        assert!(grounded);
        assert_eq!(vel.y, -10.0);
        assert!(!map.test_box(pos, Vec2::new(28.0, 28.0)));
    }

    #[test]
    fn move_point_bounces_and_is_bounded() {
        let map = grid();
        let mut pos = Vec2::new(16.0, 48.0);
        let mut vel = Vec2::new(0.0, 32.0);
        assert_eq!(map.move_point(&mut pos, &mut vel, 1.0), Ok(1));
        assert_eq!(pos.y, 48.0);
        assert_eq!(vel.y, -32.0);

        let mut pos = Vec2::new(16.0, 16.0);
        let mut vel = Vec2::new(0.0, 9000.0);
        assert_eq!(
            map.move_box(&mut pos, &mut vel, Vec2::new(28.0, 28.0),
                Vec2::default(), &mut false),
            Err(MapError::MotionTooLarge),
        );
        assert_eq!(pos, Vec2::new(16.0, 16.0));
        assert_eq!(vel.y, 9000.0);
    }

    #[test]
    fn stationary_and_unobstructed_motion_with_float_tolerance() {
        let map = grid();
        let mut pos = Vec2::new(16.0, 16.0);
        let mut vel = Vec2::new(0.0, 0.0);
        let mut grounded = false;
        map.move_box(&mut pos, &mut vel, Vec2::new(28.0, 28.0),
            Vec2::default(), &mut grounded).unwrap();
        assert_eq!(pos, Vec2::new(16.0, 16.0));
        vel = Vec2::new(5.0, 0.0);
        map.move_box(&mut pos, &mut vel, Vec2::new(28.0, 28.0),
            Vec2::default(), &mut grounded).unwrap();
        assert!((pos.x - 21.0).abs() <= 0.0001, "pos={pos:?}");
        assert_eq!(pos.y, 16.0);
        assert_eq!(vel, Vec2::new(5.0, 0.0));
    }

    #[test]
    fn broken_grids_rejected() {
        assert_eq!(GameMap::from_tile_bytes(0, 1, &[]), Err(MapError::InvalidTileData));
        assert_eq!(GameMap::from_tile_bytes(1, 1, &[0]), Err(MapError::InvalidTileData));
        assert_eq!(
            GameMap::from_tile_bytes(usize::MAX, 2, &[]),
            Err(MapError::InvalidDimensions)
        );
    }

    #[test]
    fn special_ddrace_layers_decode_in_legacy_and_modern_tilemaps() {
        use crate::datafile::{Item, RawBlock};
        const W: usize = 12;
        const H: usize = 6;
        for version in [2i32, 3i32] {
            let mut source = vec![vec![0u8; W * H * 4],
                vec![0u8; W * H * 6],
                vec![0u8; W * H * 2],
                vec![0u8; W * H * 4],
                vec![0u8; W * H * 4]];
            let speedup = (W * 2 + 2) * 6;
            source[1][speedup..speedup + 6]
                .copy_from_slice(&[10, 100, TILE_SPEED_BOOST, 0, 0, 0]);
            let tele = (W * 2 + 6) * 2;
            source[2][tele..tele + 2].copy_from_slice(&[3, 26]);
            let front = (W * 2 + 8) * 4;
            source[3][front..front + 4].copy_from_slice(&[9, 0, 0, 0]);
            let switch = (W * 2 + 10) * 4;
            source[4][switch..switch + 4].copy_from_slice(&[1, 22, 0, 0]);

            let mut layers = Vec::new();
            for (data_index, flag, offset) in [
                (0usize, TILESLAYERFLAG_GAME, 56usize),
                (1, TILESLAYERFLAG_SPEEDUP, 64),
                (2, TILESLAYERFLAG_TELE, 60),
                (3, TILESLAYERFLAG_FRONT, 68),
                (4, TILESLAYERFLAG_SWITCH, 72),
            ] {
                let off = if data_index == 0 || version == 2 {
                    offset
                } else {
                    offset + 12
                };
                let mut payload = vec![0u8; (off + 4).max(60)];
                payload[4..8].copy_from_slice(&LAYERTYPE_TILES.to_le_bytes());
                payload[12..16].copy_from_slice(&version.to_le_bytes());
                payload[16..20].copy_from_slice(&(W as i32).to_le_bytes());
                payload[20..24].copy_from_slice(&(H as i32).to_le_bytes());
                payload[24..28].copy_from_slice(&flag.to_le_bytes());
                payload[off..off + 4].copy_from_slice(&(data_index as i32).to_le_bytes());
                layers.push(payload);
            }
            let datafile = Datafile {
                version: 3, item_types: vec![],
                items: layers.iter().enumerate().map(|(id, payload)| Item {
                    type_id: MAPITEMTYPE_LAYER, id: id as u16, payload,
                }).collect(),
                raw_blocks: source.iter().map(|bytes| RawBlock {
                    encoded: bytes, uncompressed_size: None,
                }).collect(),
            };
            let map = GameMap::from_datafile(&datafile, W * H).unwrap();
            assert_eq!(map.speedup_at(Vec2::new(80.0, 80.0)).unwrap(),
                SpeedupTile { force: 10, max_speed: 100,
                    tile_type: TILE_SPEED_BOOST, reserved: 0, angle: 0 });
            assert_eq!(map.speedup_at(Vec2::new(16.0, 80.0)), None);
            assert!(!map.has_unsupported_physics_near(Vec2::new(80.0, 80.0)));
            for x in [208.0, 272.0, 336.0] {
                assert!(map.has_unsupported_physics_near(Vec2::new(x, 80.0)));
            }
            assert_eq!(map.apply_speedup(Vec2::new(80.0, 80.0),
                Vec2::default(), &Tuning::default()), Some(Vec2::new(10.0, 0.0)));
            assert_eq!(map.apply_speedup(Vec2::new(80.0, 80.0),
                Vec2::new(18.0, 0.0), &Tuning::default()),
                Some(Vec2::new(20.0, 0.0)));
            assert_eq!(map.apply_speedup(Vec2::new(80.0, 80.0),
                Vec2::new(22.0, 0.0), &Tuning::default()),
                Some(Vec2::new(20.0, 0.0)));
            assert_eq!(map.apply_speedup(Vec2::new(16.0, 80.0),
                Vec2::new(4.0, 0.0), &Tuning::default()),
                Some(Vec2::new(4.0, 0.0)));

            let mut broken = source.clone();
            broken[1].truncate(3);
            let bad_file = Datafile {
                version: 3, item_types: vec![],
                items: datafile.items.clone(),
                raw_blocks: broken.iter().map(|bytes| RawBlock {
                    encoded: bytes, uncompressed_size: None,
                }).collect(),
            };
            assert!(GameMap::from_datafile(&bad_file, W * H).is_err());
        }
    }

    #[test]
    fn teleporter_exits_and_checkpoint_fallback_follow_ddnet_numbering() {
        use crate::datafile::{Item, RawBlock};
        const W: usize = 12;
        const H: usize = 4;
        for version in [2i32, 3i32] {
            let mut game = vec![0u8; 60];
            game[4..8].copy_from_slice(&LAYERTYPE_TILES.to_le_bytes());
            game[16..20].copy_from_slice(&(W as i32).to_le_bytes());
            game[20..24].copy_from_slice(&(H as i32).to_le_bytes());
            game[24..28].copy_from_slice(&TILESLAYERFLAG_GAME.to_le_bytes());
            let tele_offset = if version == 2 { 60 } else { 72 };
            let mut tele = vec![0u8; tele_offset + 4];
            tele[4..8].copy_from_slice(&LAYERTYPE_TILES.to_le_bytes());
            tele[12..16].copy_from_slice(&version.to_le_bytes());
            tele[16..20].copy_from_slice(&(W as i32).to_le_bytes());
            tele[20..24].copy_from_slice(&(H as i32).to_le_bytes());
            tele[24..28].copy_from_slice(&TILESLAYERFLAG_TELE.to_le_bytes());
            tele[tele_offset..tele_offset+4].copy_from_slice(&1i32.to_le_bytes());
            let game_data = vec![0u8; W * H * 4];
            let mut tele_data = vec![0u8; W * H * 2];
            let mut place = |x: usize, y: usize, number: u8, kind: u8| {
                let index = (y * W + x) * 2;
                tele_data[index..index+2].copy_from_slice(&[number, kind]);
            };
            place(1, 1, 4, TILE_TELE_IN);
            place(8, 1, 4, TILE_TELE_OUT);
            place(3, 2, 7, TILE_TELE_OUT);
            place(6, 2, 7, TILE_TELE_OUT);
            place(9, 2, 2, TILE_TELE_CHECK_OUT);
            place(10, 2, 5, TILE_TELE_CHECK_OUT);
            place(11, 2, 5, TILE_TELE_CHECK_OUT);
            let file = Datafile {
                version: 3, item_types: vec![],
                items: vec![
                    Item { type_id: MAPITEMTYPE_LAYER, id: 0, payload: &game },
                    Item { type_id: MAPITEMTYPE_LAYER, id: 1, payload: &tele },
                ],
                raw_blocks: vec![
                    RawBlock { encoded: &game_data, uncompressed_size: None },
                    RawBlock { encoded: &tele_data, uncompressed_size: None },
                ],
            };
            let map = GameMap::from_datafile(&file, W * H).unwrap();
            assert!(!map.has_unsupported_physics_near(
                Vec2::new(272.0, 48.0),
            ), "teleport exit markers are not gameplay inputs");
            assert_eq!(map.tele_exit_count(4), 1);
            assert_eq!(map.unique_tele_exit(4), Some(Vec2::new(272.0, 48.0)));
            assert_eq!(map.tele_exit_count(7), 2);
            assert_eq!(map.unique_tele_exit(7), None);
            assert_eq!(map.tele_exit_count(0), 0);
            assert_eq!(map.unique_tele_exit(0), None);
            assert_eq!(map.unique_checkpoint_exit(4),
                Some(Vec2::new(304.0, 80.0)));
            assert_eq!(map.unique_checkpoint_exit(8), None,
                "server randomly chooses between checkpoint 5 exits");
            assert_eq!(map.unique_checkpoint_exit(1), None);
            assert!(map.crosses_tee_teleport(
                Vec2::new(16.0, 48.0), Vec2::new(100.0, 48.0),
            ));
            assert!(!map.crosses_tee_teleport(
                Vec2::new(16.0, 16.0), Vec2::new(100.0, 16.0),
            ));
            assert!(!map.crosses_tee_teleport(
                Vec2::new(245.0, 48.0), Vec2::new(290.0, 48.0),
            ), "a teleport exit is not a tee teleport input");
        }
    }

    #[test]
    fn legacy_and_modern_tune_layers_decode_zone_numbers() {
        use crate::datafile::{Item, RawBlock};
        for version in [2i32, 3i32] {
            let (width, height) = (4usize, 3usize);
            let game_bytes = vec![0u8; width * height * 4];
            let mut tune_bytes = vec![0u8; width * height * 2];
            tune_bytes[(width + 1) * 2..(width + 1) * 2 + 2]
                .copy_from_slice(&[7, 68]);
            tune_bytes[(width + 2) * 2..(width + 2) * 2 + 2]
                .copy_from_slice(&[9, 0]);
            let mut game = vec![0u8; 60];
            game[4..8].copy_from_slice(&LAYERTYPE_TILES.to_le_bytes());
            game[16..20].copy_from_slice(&(width as i32).to_le_bytes());
            game[20..24].copy_from_slice(&(height as i32).to_le_bytes());
            game[24..28].copy_from_slice(&TILESLAYERFLAG_GAME.to_le_bytes());
            let offset = if version == 2 { 76 } else { 88 };
            let mut tune = vec![0u8; offset + 4];
            tune[4..8].copy_from_slice(&LAYERTYPE_TILES.to_le_bytes());
            tune[12..16].copy_from_slice(&version.to_le_bytes());
            tune[16..20].copy_from_slice(&(width as i32).to_le_bytes());
            tune[20..24].copy_from_slice(&(height as i32).to_le_bytes());
            tune[24..28].copy_from_slice(&TILESLAYERFLAG_TUNE.to_le_bytes());
            tune[offset..offset+4].copy_from_slice(&1i32.to_le_bytes());
            let file = Datafile {
                version: 3, item_types: vec![],
                items: vec![
                    Item { type_id: MAPITEMTYPE_LAYER, id: 0, payload: &game },
                    Item { type_id: MAPITEMTYPE_LAYER, id: 1, payload: &tune },
                ],
                raw_blocks: vec![
                    RawBlock { encoded: &game_bytes, uncompressed_size: None },
                    RawBlock { encoded: &tune_bytes, uncompressed_size: None },
                ],
            };
            let parsed = GameMap::from_datafile(&file, width * height).unwrap();
            assert_eq!(parsed.tune_zone_at(Vec2::new(48.0, 48.0)), 7);
            assert_eq!(parsed.tune_zone_at(Vec2::new(80.0, 48.0)), 0);
            assert_eq!(parsed.tune_zone_at(Vec2::new(-300.0, 48.0)), 0);
            assert_eq!(parsed.tune_zone_at(Vec2::new(999.0, 48.0)), 0);
            assert!(parsed.crosses_tune_zone(
                Vec2::new(16.0, 48.0), Vec2::new(108.0, 48.0),
            ));
            assert!(!parsed.crosses_tune_zone(
                Vec2::new(16.0, 16.0), Vec2::new(108.0, 16.0),
            ));
            let vanilla = GameMap::from_tile_bytes(
                width, height, &game_bytes,
            ).unwrap();
            assert_eq!(vanilla.tune_zone_at(Vec2::new(48.0, 48.0)), 0);
            assert!(!vanilla.crosses_tune_zone(
                Vec2::new(16.0, 48.0), Vec2::new(108.0, 48.0),
            ));
            let mut mismatched_tune = tune.clone();
            mismatched_tune[20..24].copy_from_slice(&4i32.to_le_bytes());
            let malformed = Datafile {
                version: 3, item_types: vec![],
                items: vec![
                    Item { type_id: MAPITEMTYPE_LAYER, id: 0, payload: &game },
                    Item { type_id: MAPITEMTYPE_LAYER, id: 1,
                        payload: &mismatched_tune },
                ],
                raw_blocks: file.raw_blocks.clone(),
            };
            assert_eq!(GameMap::from_datafile(&malformed, width * height),
                Err(MapError::InvalidTuneLayer));
        }
    }

    #[test]
    fn map_layer_offset_contract() {
        let mut payload = vec![0u8; 92];
        payload[4..8].copy_from_slice(&LAYERTYPE_TILES.to_le_bytes());
        payload[16..20].copy_from_slice(&3i32.to_le_bytes());
        payload[20..24].copy_from_slice(&2i32.to_le_bytes());
        payload[24..28].copy_from_slice(&TILESLAYERFLAG_GAME.to_le_bytes());
        payload[56..60].copy_from_slice(&7i32.to_le_bytes());
        assert_eq!(layer_int(&payload, 4), Some(2));
        assert_eq!(layer_int(&payload, 16), Some(3));
        assert_eq!(layer_int(&payload, 20), Some(2));
        assert_eq!(layer_int(&payload, 24), Some(1));
        assert_eq!(layer_int(&payload, 56), Some(7));
        assert_eq!(layer_int(&payload, 92), None);
    }
}
