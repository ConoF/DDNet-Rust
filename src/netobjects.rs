//! DDNet 20.2.1 vanilla 0.6 snapshot object metadata.
//!
//! Sizes are generated from pinned upstream datasrc/network.py by counting
//! 32-bit scalar/array fields and inherited fields, as generated C++ does.
//! Object IDs follow datasrc/compile.py: NETOBJTYPE_EX=0, then non-UUID
//! objects and events in source order. Extended UUID objects use dynamic
//! snapshot-size handling and are deliberately not included here.

use crate::snapshot_delta::{SnapshotObject, SnapshotState};

pub const VANILLA_OBJECT_WORDS: [u16; 21] = [
    0, // EX (UUID mapping)
    10, // 1 PlayerInput
    6, // 2 Projectile
    5, // 3 Laser
    4, // 4 Pickup
    3, // 5 Flag
    8, // 6 GameInfo
    4, // 7 GameData
    15, // 8 CharacterCore
    22, // 9 Character:CharacterCore
    5, // 10 PlayerInfo
    17, // 11 ClientInfo
    3, // 12 SpectatorInfo
    2, // 13 Common
    2, // 14 Explosion:Common
    2, // 15 Spawn:Common
    2, // 16 HammerHit:Common
    3, // 17 Death:Common
    3, // 18 SoundGlobal:Common
    3, // 19 SoundWorld:Common
    3, // 20 DamageInd:Common
];

pub const VANILLA_OBJECT_NAMES: [&str; 21] = [
    "EX",
    "PlayerInput",
    "Projectile",
    "Laser",
    "Pickup",
    "Flag",
    "GameInfo",
    "GameData",
    "CharacterCore",
    "Character",
    "PlayerInfo",
    "ClientInfo",
    "SpectatorInfo",
    "Common",
    "Explosion",
    "Spawn",
    "HammerHit",
    "Death",
    "SoundGlobal",
    "SoundWorld",
    "DamageInd",
];

pub const CHARACTERCORE_TYPE: u16 = 8;
pub const CHARACTER_TYPE: u16 = 9;
pub const PLAYERINFO_TYPE: u16 = 10;
pub const CLIENTINFO_TYPE: u16 = 11;

/// DDNet 0.6 stores tee names as signed characters with an offset of 128,
/// four bytes per network-order i32. The last byte is reserved for a terminator.
pub fn decode_tw_int_string(words: &[i32]) -> Option<String> {
    if words.is_empty() || words.len() > 16 { return None; }
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for &word in words {
        for shift in [24, 16, 8, 0] {
            bytes.push(((word >> shift) as u8).wrapping_sub(128));
        }
    }
    *bytes.last_mut()? = 0;
    let end = bytes.iter().position(|&byte| byte == 0)?;
    Some(std::str::from_utf8(&bytes[..end]).ok()?.to_owned())
}

pub fn player_name(snapshot: &SnapshotState, client_id: u16) -> Option<String> {
    let object = snapshot.find(CLIENTINFO_TYPE, client_id)?;
    if object.words.len() != usize::from(VANILLA_OBJECT_WORDS[CLIENTINFO_TYPE as usize]) {
        return None;
    }
    decode_tw_int_string(&object.words[..4])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterCoreNet {
    pub tick: i32,
    pub x: i32,
    pub y: i32,
    pub velocity_x: i32,
    pub velocity_y: i32,
    pub angle: i32,
    pub direction: i32,
    pub jumped: i32,
    pub hooked_player: i32,
    pub hook_state: i32,
    pub hook_tick: i32,
    pub hook_x: i32,
    pub hook_y: i32,
    pub hook_dx: i32,
    pub hook_dy: i32,
}

impl CharacterCoreNet {
    pub fn from_object(item: &SnapshotObject) -> Option<Self> {
        if !matches!(item.type_id, CHARACTERCORE_TYPE | CHARACTER_TYPE) ||
            item.words.len() < 15 {
            return None;
        }
        let w = &item.words;
        Some(Self {
            tick: w[0], x: w[1], y: w[2], velocity_x: w[3],
            velocity_y: w[4], angle: w[5], direction: w[6],
            jumped: w[7], hooked_player: w[8], hook_state: w[9],
            hook_tick: w[10], hook_x: w[11], hook_y: w[12],
            hook_dx: w[13], hook_dy: w[14],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerInfoNet {
    pub local: bool,
    pub client_id: i32,
    pub team: i32,
    pub score: i32,
    pub latency: i32,
}

impl PlayerInfoNet {
    pub fn from_object(item: &SnapshotObject) -> Option<Self> {
        if item.type_id != PLAYERINFO_TYPE || item.words.len() != 5 {
            return None;
        }
        let w = &item.words;
        Some(Self {
            local: w[0] != 0,
            client_id: w[1],
            team: w[2],
            score: w[3],
            latency: w[4],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_sizes_match_ddnet_2021_06_schema() {
        assert_eq!(VANILLA_OBJECT_WORDS[1], 10);
        assert_eq!(VANILLA_OBJECT_WORDS[8], 15);
        assert_eq!(VANILLA_OBJECT_WORDS[9], 22);
        assert_eq!(VANILLA_OBJECT_WORDS[11], 17);
        assert_eq!(VANILLA_OBJECT_WORDS[13], 2);
        assert_eq!(VANILLA_OBJECT_WORDS[14], 2);
        assert_eq!(VANILLA_OBJECT_WORDS[17], 3);
        assert_eq!(VANILLA_OBJECT_NAMES[9], "Character");
    }

    #[test]
    fn decode_real_06_clientinfo_name_layout() {
        fn pack_name(name: &str) -> [i32; 4] {
            let mut bytes = [0u8; 16];
            bytes[..name.len()].copy_from_slice(name.as_bytes());
            let mut words = [0i32; 4];
            for (index, word) in words.iter_mut().enumerate() {
                let mut value = 0u32;
                for byte_index in 0..4 {
                    value |= u32::from(bytes[index * 4 + byte_index].wrapping_add(128))
                        << (24 - byte_index * 8);
                }
                *word = value as i32;
            }
            words[3] &= !0xff;
            words
        }
        let mut items = Vec::new();
        for (id, name) in [(3, "ConoF"), (9, "Ти")] {
            let mut words = vec![0; 17];
            words[..4].copy_from_slice(&pack_name(name));
            items.push(SnapshotObject { type_id: CLIENTINFO_TYPE, id, words });
        }
        let state = SnapshotState { items };
        assert_eq!(player_name(&state, 3).as_deref(), Some("ConoF"));
        assert_eq!(player_name(&state, 9).as_deref(), Some("Ти"));
        assert_eq!(player_name(&state, 0), None);
        assert!(decode_tw_int_string(&[i32::MAX; 4]).is_none());
    }

    #[test]
    fn typed_character_and_player_info() {
        let mut values = vec![0; 22];
        values[0] = 100;
        values[1] = 128;
        values[2] = 256;
        values[9] = 5;
        let entity = SnapshotObject { type_id: 9, id: 1, words: values };
        let char_core = CharacterCoreNet::from_object(&entity).unwrap();
        assert_eq!(char_core.x, 128);
        assert_eq!(char_core.y, 256);
        assert_eq!(char_core.hook_state, 5);
        let player = SnapshotObject {
            type_id: 10, id: 1, words: vec![1, 1, 0, 50, 32],
        };
        assert_eq!(PlayerInfoNet::from_object(&player).unwrap().client_id, 1);
        assert!(PlayerInfoNet::from_object(&player).unwrap().local);
        assert!(PlayerInfoNet::from_object(&entity).is_none());
    }
}
