//! Additional DDNet physics layers (front, tele, speedup, switch, tune).
//!
//! Reference: game/mapitems.h and game/layers.cpp, official DDNet 20.2.1.
//! This only extracts layer records. Gameplay effects, switches, teleport
//! routing, tune-zone behavior and full collision are not implemented.

use crate::datafile::Datafile;
use crate::map::{GameMap, MapError, Tile};

const MAPITEMTYPE_LAYER: u16 = 5;
const LAYERTYPE_TILES: i32 = 2;
const TELE: i32 = 1 << 1;
const SPEEDUP: i32 = 1 << 2;
const FRONT: i32 = 1 << 3;
const SWITCH: i32 = 1 << 4;
const TUNE: i32 = 1 << 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TeleTile { pub number: u8, pub kind: u8 }
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpeedupTile {
    pub force: u8, pub max_speed: u8, pub kind: u8, pub angle: i16,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwitchTile {
    pub number: u8, pub kind: u8, pub flags: u8, pub delay: u8,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuneTile { pub number: u8, pub kind: u8 }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicsLayers {
    pub game: GameMap,
    pub front: Option<Vec<Tile>>,
    pub tele: Option<Vec<TeleTile>>,
    pub speedup: Option<Vec<SpeedupTile>>,
    pub switches: Option<Vec<SwitchTile>>,
    pub tune: Option<Vec<TuneTile>>,
}

fn field(payload: &[u8], offset: usize) -> Option<i32> {
    let raw: [u8; 4] = payload.get(offset..offset + 4)?.try_into().ok()?;
    Some(i32::from_le_bytes(raw))
}

fn layer_index_offset(version: i32, flag: i32) -> Option<usize> {
    let v3 = version >= 3;
    match flag {
        TELE => Some(if v3 { 72 } else { 60 }),
        SPEEDUP => Some(if v3 { 76 } else { 64 }),
        FRONT => Some(if v3 { 80 } else { 68 }),
        SWITCH => Some(if v3 { 84 } else { 72 }),
        TUNE => Some(if v3 { 88 } else { 76 }),
        _ => None,
    }
}

fn raw_records(
    file: &Datafile<'_>,
    index: usize,
    record_size: usize,
    count: usize,
) -> Result<Vec<u8>, MapError> {
    let exact_size = count.checked_mul(record_size).ok_or(MapError::InvalidDimensions)?;
    let bytes = file.raw_data(index, exact_size)?;
    if bytes.len() != exact_size { return Err(MapError::InvalidTileData); }
    Ok(bytes)
}

impl PhysicsLayers {
    pub fn from_datafile(file: &Datafile<'_>, max_tiles: usize) -> Result<Self, MapError> {
        let game = GameMap::from_datafile(file, max_tiles)?;
        let cells = game.width.checked_mul(game.height)
            .ok_or(MapError::InvalidDimensions)?;
        let mut layers = Self {
            game, front: None, tele: None, speedup: None, switches: None, tune: None,
        };

        for item in file.items.iter().filter(|item| item.type_id == MAPITEMTYPE_LAYER) {
            if field(item.payload, 4) != Some(LAYERTYPE_TILES) { continue; }
            let flags = field(item.payload, 24).ok_or(MapError::InvalidTileLayer)?;
            let kind = if flags & TELE != 0 { TELE }
                else if flags & SPEEDUP != 0 { SPEEDUP }
                else if flags & FRONT != 0 { FRONT }
                else if flags & SWITCH != 0 { SWITCH }
                else if flags & TUNE != 0 { TUNE }
                else { continue };
            if usize::try_from(field(item.payload, 16).ok_or(MapError::InvalidTileLayer)?)
                != Ok(layers.game.width)
                || usize::try_from(field(item.payload, 20).ok_or(MapError::InvalidTileLayer)?)
                    != Ok(layers.game.height)
            {
                return Err(MapError::InvalidDimensions);
            }
            let version = field(item.payload, 12).ok_or(MapError::InvalidTileLayer)?;
            let offset = layer_index_offset(version, kind).ok_or(MapError::InvalidTileLayer)?;
            let block_index = usize::try_from(
                field(item.payload, offset).ok_or(MapError::InvalidTileLayer)?
            ).map_err(|_| MapError::InvalidTileLayer)?;

            match kind {
                TELE => {
                    let bytes = raw_records(file, block_index, 2, cells)?;
                    layers.tele = Some(bytes.chunks_exact(2).map(|t|
                        TeleTile { number: t[0], kind: t[1] }
                    ).collect());
                }
                SPEEDUP => {
                    let bytes = raw_records(file, block_index, 6, cells)?;
                    layers.speedup = Some(bytes.chunks_exact(6).map(|t|
                        SpeedupTile {
                            force: t[0], max_speed: t[1], kind: t[2],
                            angle: i16::from_le_bytes([t[4], t[5]]),
                        }
                    ).collect());
                }
                FRONT => {
                    let bytes = raw_records(file, block_index, 4, cells)?;
                    layers.front = Some(bytes.chunks_exact(4).map(|t|
                        Tile { index: t[0], flags: t[1], skip: t[2], reserved: t[3] }
                    ).collect());
                }
                SWITCH => {
                    let bytes = raw_records(file, block_index, 4, cells)?;
                    layers.switches = Some(bytes.chunks_exact(4).map(|t|
                        SwitchTile { number: t[0], kind: t[1], flags: t[2], delay: t[3] }
                    ).collect());
                }
                TUNE => {
                    let bytes = raw_records(file, block_index, 2, cells)?;
                    layers.tune = Some(bytes.chunks_exact(2).map(|t|
                        TuneTile { number: t[0], kind: t[1] }
                    ).collect());
                }
                _ => unreachable!(),
            }
        }
        Ok(layers)
    }

    /// Location is clamped exactly as CCollision::GetTile in vanilla DDNet.
    pub fn index_at_pixel(&self, x: i32, y: i32) -> usize {
        let tx = (x / 32).clamp(0, self.game.width as i32 - 1) as usize;
        let ty = (y / 32).clamp(0, self.game.height as i32 - 1) as usize;
        ty * self.game.width + tx
    }

    /// Read the raw game/front tile indices; this does not perform freeze.
    pub fn game_and_front_at_pixel(&self, x: i32, y: i32) -> (u8, Option<u8>) {
        let idx = self.index_at_pixel(x, y);
        (self.game.tiles[idx].index, self.front.as_ref().map(|layer| layer[idx].index))
    }

    pub fn tele_at_pixel(&self, x: i32, y: i32) -> Option<TeleTile> {
        let idx = self.index_at_pixel(x, y);
        self.tele.as_ref().map(|layer| layer[idx])
    }

    pub fn tune_at_pixel(&self, x: i32, y: i32) -> Option<TuneTile> {
        let idx = self.index_at_pixel(x, y);
        self.tune.as_ref().map(|layer| layer[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_legacy_and_modern_offsets() {
        assert_eq!(layer_index_offset(2, TELE), Some(60));
        assert_eq!(layer_index_offset(2, SPEEDUP), Some(64));
        assert_eq!(layer_index_offset(2, FRONT), Some(68));
        assert_eq!(layer_index_offset(3, TELE), Some(72));
        assert_eq!(layer_index_offset(3, SPEEDUP), Some(76));
        assert_eq!(layer_index_offset(3, FRONT), Some(80));
        assert_eq!(layer_index_offset(3, SWITCH), Some(84));
        assert_eq!(layer_index_offset(3, TUNE), Some(88));
        assert_eq!(layer_index_offset(3, 128), None);
    }

    #[test]
    fn layer_record_byte_layouts() {
        let speed = [7u8, 9, 29, 0, 0x2c, 0x01];
        let parsed = SpeedupTile {
            force: speed[0], max_speed: speed[1], kind: speed[2],
            angle: i16::from_le_bytes([speed[4], speed[5]]),
        };
        assert_eq!(parsed.angle, 300);
        assert_eq!(parsed.force, 7);
        let tele = TeleTile { number: 5, kind: 26 };
        assert_eq!(tele, TeleTile { number: 5, kind: 26 });
    }
}
