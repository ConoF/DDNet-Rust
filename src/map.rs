//! Vanilla DDNet game-layer extraction and elementary collision queries.
//!
//! Source of truth: game/mapitems.h, game/layers.cpp and game/collision.cpp in
//! upstream DDNet 20.2.1. This is not the full DDNet collision system:
//! front tiles, doors, zone-specific tuning values, teleporters, movement
//! restrictions and map entities need separate implementations. Tune layer IDs
//! are decoded to prevent incorrect prediction with default tuning.

use crate::datafile::{Datafile, DatafileError};
use crate::physics::Vec2;

const MAPITEMTYPE_LAYER: u16 = 5;
const LAYERTYPE_TILES: i32 = 2;
const TILESLAYERFLAG_GAME: i32 = 1;
const TILESLAYERFLAG_TUNE: i32 = 1 << 5;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameMap {
    pub width: usize,
    pub height: usize,
    pub tiles: Vec<Tile>,
    /// Server-defined tune parameters are not encoded in this layer.
    tune_tiles: Option<Vec<TuneTile>>,
}

fn layer_int(payload: &[u8], offset: usize) -> Option<i32> {
    let bytes: [u8; 4] = payload.get(offset..offset + 4)?.try_into().ok()?;
    Some(i32::from_le_bytes(bytes))
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
        // CMapItemLayerTilemap_v2Legacy::m_Tune is at 76, while the
        // v3/DDRace tilemap stores it at 88 after the layer name.
        if let Some(tune) = file.items.iter().find(|item| {
            item.type_id == MAPITEMTYPE_LAYER
                && layer_int(item.payload, 4) == Some(LAYERTYPE_TILES)
                && layer_int(item.payload, 24).is_some_and(|flags| {
                    flags & TILESLAYERFLAG_GAME == 0
                        && flags & TILESLAYERFLAG_TUNE != 0
                })
        }) {
            let payload = tune.payload;
            let version = layer_int(payload, 12).ok_or(MapError::InvalidTuneLayer)?;
            let offset = if version >= 3 { 88 } else { 76 };
            if version < 2 || payload.len() < offset + 4
                || layer_int(payload, 16) != Some(width as i32)
                || layer_int(payload, 20) != Some(height as i32)
            {
                return Err(MapError::InvalidTuneLayer);
            }
            let tune_index = usize::try_from(
                layer_int(payload, offset).ok_or(MapError::InvalidTuneLayer)?,
            ).map_err(|_| MapError::InvalidTuneLayer)?;
            let tune_bytes = tile_count.checked_mul(2)
                .ok_or(MapError::InvalidDimensions)?;
            let raw = file.raw_data(tune_index, tune_bytes)?;
            if raw.len() != tune_bytes { return Err(MapError::InvalidTuneData); }
            map.tune_tiles = Some(raw.chunks_exact(2)
                .map(|pair| TuneTile { number: pair[0], tile_type: pair[1] })
                .collect());
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
        Ok(Self { width, height, tiles, tune_tiles: None })
    }

    pub fn tile(&self, x: usize, y: usize) -> Option<Tile> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.tiles.get(y * self.width + x).copied()
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
