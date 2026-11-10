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
/// Standard 0.6 CNetObj_Pickup: x, y, powerup type, subtype.
pub const PICKUP_TYPE: u16 = 4;
/// 0.6 CNetObj_Projectile and CNetObj_Laser (datasrc/network.py).
pub const PROJECTILE_TYPE: u16 = 2;
pub const LASER_TYPE: u16 = 3;
pub const PLAYERINFO_TYPE: u16 = 10;
pub const CLIENTINFO_TYPE: u16 = 11;

/// CalculateUuid("switch-state@netobj.ddnet.tw") from DDNet's
/// e05ddaaa-c4e6-4cfb-b642-5d48e80c0029 namespace, packed as four
/// big-endian signed int32s by UuidToInts (uuid_manager.cpp).
pub const SWITCH_STATE_UUID_WORDS: [i32; 4] =
    [-334109079, -837733529, -1366378226, 1529330133];
/// uuid3(TEEWORLDS_NAMESPACE, "pickup@netobj.ddnet.tw"), Uint32 BE.
pub const DDNET_PICKUP_UUID_WORDS: [i32; 4] =
    [-362919343, 1492858500, -1763385134, 1744095845];
const OFFSET_UUID_TYPE: u16 = 0x4000;

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

/// Vanilla 0.6 CNetObj_Character appends these values after its 15-word
/// character core. These fields can be hidden for other players, so only
/// the verified local character is used for the gameplay HUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterHudStatus {
    pub health: u8,
    pub armor: u8,
    pub ammo: i8,
    pub weapon: i8,
}

impl CharacterHudStatus {
    pub fn from_object(object: &SnapshotObject) -> Option<Self> {
        if object.type_id != CHARACTER_TYPE || object.words.len() != 22 {
            return None;
        }
        let data = &object.words;
        Some(Self {
            health: u8::try_from(data[16]).ok().filter(|&n| n <= 10)?,
            armor: u8::try_from(data[17]).ok().filter(|&n| n <= 10)?,
            ammo: i8::try_from(data[18]).ok().filter(|&n| (-1..=10).contains(&n))?,
            weapon: i8::try_from(data[19]).ok().filter(|&n| (-1..=5).contains(&n))?,
        })
    }

    pub fn local_from_snapshot(snapshot: &SnapshotState) -> Option<Self> {
        let id = snapshot.items.iter()
            .filter_map(PlayerInfoNet::from_object)
            .find(|player| player.local)?
            .client_id;
        let id = u16::try_from(id).ok()?;
        Self::from_object(snapshot.find(CHARACTER_TYPE, id)?)
    }
}

/// Plain Teeworlds 0.6 snapshot projectile. Unlike DDNetProjectiles,
/// vanilla position is unscaled and velocities are fixed point /100.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectileNet {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub weapon: u8,
    pub start_tick: i32,
}

impl ProjectileNet {
    pub fn from_object(object: &SnapshotObject) -> Option<Self> {
        if object.type_id != PROJECTILE_TYPE || object.words.len() != 6 {
            return None;
        }
        let w = &object.words;
        // DDNet 20.2.1 uses the high bits of positive VelY for older
        // DDRace extra info. Never interpret those as plain velocity.
        if w[3] >= 0 && (w[3] & (1 << 9)) != 0 { return None; }
        let weapon = u8::try_from(w[4]).ok().filter(|&id| id <= 5)?;
        let value = Self {
            x: w[0] as f32, y: w[1] as f32,
            vx: w[2] as f32 / 100.0,
            vy: w[3] as f32 / 100.0,
            weapon, start_tick: w[5],
        };
        value.validated()
    }

    fn validated(self) -> Option<Self> {
        let values = [self.x, self.y, self.vx, self.vy];
        if self.start_tick < 0 || self.start_tick > i32::MAX - 1000
            || values.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.0)
            || self.vx.hypot(self.vy) > 100.0
        { return None; }
        Some(self)
    }
}

/// Vanilla CNetObj_Laser is five int32s, with no type or team metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaserNet {
    pub to_x: i32,
    pub to_y: i32,
    pub from_x: i32,
    pub from_y: i32,
    pub start_tick: i32,
    pub kind: i32,
}

impl LaserNet {
    pub fn from_object(object: &SnapshotObject) -> Option<Self> {
        if object.type_id != LASER_TYPE || object.words.len() != 5 {
            return None;
        }
        let w = &object.words;
        let laser = Self {
            to_x: w[0], to_y: w[1], from_x: w[2], from_y: w[3],
            start_tick: w[4], kind: 0,
        };
        // Guard against malformed or nonsensically long beams.
        let dx = i64::from(laser.to_x) - i64::from(laser.from_x);
        let dy = i64::from(laser.to_y) - i64::from(laser.from_y);
        (laser.start_tick >= 0
            && dx.abs() <= 8192 && dy.abs() <= 8192
            && [laser.to_x,laser.to_y,laser.from_x,laser.from_y]
                .iter().all(|v| v.abs() < 1_000_000)
        ).then_some(laser)
    }
}

/// Unmodified 0.6 CNetObj_Pickup from datasrc/network.py.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickupNet {
    pub x: i32,
    pub y: i32,
    pub kind: u8,
    pub subtype: u8,
    /// PICKUPFLAG_XFLIP, YFLIP, ROTATE, NO_PREDICT (DDNet only).
    pub flags: u8,
    pub switch_number: i32,
}

impl PickupNet {
    pub fn from_object(object: &SnapshotObject) -> Option<Self> {
        if object.type_id != PICKUP_TYPE || object.words.len() != 4 {
            return None;
        }
        let w = &object.words;
        let kind = u8::try_from(w[2]).ok().filter(|&kind| kind <= 8)?;
        let subtype = u8::try_from(w[3]).ok()?;
        if kind == 2 && subtype > 5 { return None; }
        Some(Self {
            x: w[0], y: w[1], kind, subtype, flags: 0, switch_number: 0,
        })
    }

    /// NETOBJTYPE_EX remaps the DDNet-specific pickup to a dynamically
    /// assigned snapshot type. Trust it only after the 128-bit name UUID
    /// has been independently verified from a mapping object.
    pub fn extended_type(snapshot: &SnapshotState) -> Option<u16> {
        let mut types = snapshot.items.iter().filter(|item| {
            item.type_id == 0 && item.id >= OFFSET_UUID_TYPE
                && item.words.as_slice() == DDNET_PICKUP_UUID_WORDS
        });
        let mapped = types.next()?.id;
        if types.next().is_some() { return None; }
        Some(mapped)
    }

    pub fn from_extended_object(item: &SnapshotObject, mapped: u16) -> Option<Self> {
        if mapped < OFFSET_UUID_TYPE || item.type_id != mapped
            || !(5..=6).contains(&item.words.len())
        {
            return None;
        }
        let w = &item.words;
        let kind = u8::try_from(w[2]).ok().filter(|&kind| kind <= 8)?;
        let subtype = u8::try_from(w[3]).ok()?;
        if kind == 2 && subtype > 5 { return None; }
        let flags = u8::try_from(*w.get(5).unwrap_or(&0)).ok()?;
        if flags & !0x0f != 0 { return None; }
        Some(Self {
            x: w[0], y: w[1], kind, subtype,
            switch_number: w[4], flags,
        })
    }
}

/// The server sends an extended CNetObj_SwitchState for the observed
/// DDRace team. m_aStatus contains 256 bits (8 signed int32 words).
/// The last eight words are optional on older servers: 4 switch numbers
/// and 4 timed end ticks. Absence of a timer is not proof of permanence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchStateNet {
    pub team: u16,
    pub highest_number: u8,
    statuses: [u32; 8],
    timed: Option<[(u8, i32); 4]>,
}

impl SwitchStateNet {
    /// Recover the *internal* snapshot type from the NETOBJTYPE_EX UUID.
    /// The server can assign different high type IDs on different snapshots.
    /// Ambiguous/malformed objects never become trusted switch data.
    pub fn from_snapshot(snapshot: &SnapshotState) -> Option<Self> {
        let mut mappings = snapshot.items.iter().filter(|item| {
            item.type_id == 0
                && item.id >= OFFSET_UUID_TYPE
                && item.words.as_slice() == SWITCH_STATE_UUID_WORDS
        });
        let mapped_id = mappings.next()?.id;
        if mappings.next().is_some() { return None; }
        let mut states = snapshot.items.iter().filter(|item| item.type_id == mapped_id);
        let item = states.next()?;
        if states.next().is_some() { return None; }
        Self::from_object(item)
    }

    /// Decode the raw extended payload; type-to-UUID verification belongs
    /// to from_snapshot, so this function is also useful for testing.
    fn from_object(item: &SnapshotObject) -> Option<Self> {
        let words = item.words.as_slice();
        if words.len() != 9 && words.len() != 17 { return None; }
        let highest_number = u8::try_from(*words.first()?).ok()?;
        let mut statuses = [0u32; 8];
        for (index, status) in statuses.iter_mut().enumerate() {
            *status = words[index + 1] as u32;
        }
        let timed = if words.len() == 17 {
            let mut timers = [(0u8, 0i32); 4];
            for (i, timer) in timers.iter_mut().enumerate() {
                let switch = u8::try_from(words[9 + i]).ok()?;
                let end = words[13 + i];
                if end < 0 { return None; }
                *timer = (switch, end);
            }
            Some(timers)
        } else {
            None
        };
        Some(Self { team: item.id, highest_number, statuses, timed })
    }

    /// None means this switch is outside the server-confirmed mask.
    pub fn is_active(&self, number: u8) -> Option<bool> {
        (number <= self.highest_number).then(|| {
            (self.statuses[usize::from(number / 32)] >> (number % 32)) & 1 != 0
        })
    }

    /// If the server supplied a timed toggle that can expire during local
    /// prediction, its resulting status must be obtained from a new snapshot.
    /// Legacy 9-word states cannot prove timed stability.
    pub fn stable_through(&self, number: u8, snapshot_tick: i32, target_tick: i32) -> bool {
        if self.is_active(number).is_none() || target_tick < snapshot_tick {
            return false;
        }
        let Some(timers) = self.timed else { return false; };
        !timers.iter().any(|&(which, end_tick)| {
            which == number && end_tick > snapshot_tick && end_tick <= target_tick
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_state_uses_verified_uuid_mapping_and_team_specific_bitset() {
        let mut words = vec![0; 17];
        words[0] = 255;
        words[1] = (1 << 3) | (1 << 8);
        words[8] = i32::MIN; // switch 255
        words[9] = 8; // first timed switch
        words[13] = 108; // expires after tick 108
        let state = SnapshotState { items: vec![
            SnapshotObject {
                type_id: 0, id: 32767, words: SWITCH_STATE_UUID_WORDS.to_vec(),
            },
            SnapshotObject { type_id: 32767, id: 3, words },
        ] };
        let switch = SwitchStateNet::from_snapshot(&state).unwrap();
        assert_eq!(switch.team, 3);
        assert_eq!(switch.highest_number, 255);
        assert_eq!(switch.is_active(3), Some(true));
        assert_eq!(switch.is_active(8), Some(true));
        assert_eq!(switch.is_active(9), Some(false));
        assert_eq!(switch.is_active(255), Some(true));
        assert!(switch.stable_through(8, 100, 107));
        assert!(!switch.stable_through(8, 100, 108));
        assert!(switch.stable_through(3, 100, 108));
        assert_eq!(state.items[1].words[0], 255);
    }

    #[test]
    fn legacy_switch_state_and_ambiguous_extensions_fail_closed() {
        let mut state = SnapshotState { items: vec![
            SnapshotObject {
                type_id: 0, id: 32766, words: SWITCH_STATE_UUID_WORDS.to_vec(),
            },
            SnapshotObject {
                type_id: 32766, id: 1, words: vec![7, 8, 0, 0, 0, 0, 0, 0, 0],
            },
        ] };
        let switch = SwitchStateNet::from_snapshot(&state).unwrap();
        assert_eq!(switch.is_active(3), Some(true));
        assert_eq!(switch.is_active(8), None);
        assert!(!switch.stable_through(3, 100, 101));
        state.items[0].words[0] ^= 1;
        assert!(SwitchStateNet::from_snapshot(&state).is_none());
        state.items[0].words = SWITCH_STATE_UUID_WORDS.to_vec();
        state.items[1].words.truncate(8);
        assert!(SwitchStateNet::from_snapshot(&state).is_none());
        state.items[1].words = vec![256; 9];
        assert!(SwitchStateNet::from_snapshot(&state).is_none());
        state.items[1].words = vec![7; 10];
        assert!(SwitchStateNet::from_snapshot(&state).is_none());
        state.items[1].words = vec![7; 17];
        state.items.push(state.items[1].clone());
        assert!(SwitchStateNet::from_snapshot(&state).is_none());
    }

    #[test]
    fn ddnet_pickup_uuid_mapping_and_extended_flags_are_verified() {
        let mut state = SnapshotState { items: vec![
            SnapshotObject {
                type_id: 0, id: 0x4001, words: DDNET_PICKUP_UUID_WORDS.to_vec(),
            },
            SnapshotObject {
                type_id: 0x4001, id: 7, words: vec![112, 89, 2, 3, 12, 0x07],
            },
            SnapshotObject {
                type_id: 0x4001, id: 8, words: vec![200, 89, 0, 0, 0],
            },
        ] };
        assert_eq!(PickupNet::extended_type(&state), Some(0x4001));
        let pickup = PickupNet::from_extended_object(
            &state.items[1], PickupNet::extended_type(&state).unwrap(),
        ).unwrap();
        assert_eq!(pickup, PickupNet {
            x: 112, y: 89, kind: 2, subtype: 3,
            flags: 7, switch_number: 12,
        });
        assert_eq!(PickupNet::from_extended_object(&state.items[2], 0x4001)
            .unwrap().flags, 0);
        state.items[0].words[0] ^= 1;
        assert_eq!(PickupNet::extended_type(&state), None);
        state.items[0].words = DDNET_PICKUP_UUID_WORDS.to_vec();
        state.items.push(state.items[0].clone());
        assert_eq!(PickupNet::extended_type(&state), None);
        state.items.pop();
        state.items[1].words[5] = 16;
        assert!(PickupNet::from_extended_object(&state.items[1], 0x4001).is_none());
        state.items[1].words[5] = 7;
        assert!(PickupNet::from_extended_object(&state.items[1], 4).is_none());
        state.items[1].words.push(1);
        assert!(PickupNet::from_extended_object(&state.items[1], 0x4001).is_none());
    }

    #[test]
    fn vanilla_projectile_and_laser_snapshots_are_strict_and_safe() {
        let make = |type_id, words| SnapshotObject { type_id, id: 7, words };
        let gun = make(PROJECTILE_TYPE, vec![64, 128, 100, 0, 1, 500]);
        assert_eq!(ProjectileNet::from_object(&gun), Some(ProjectileNet {
            x: 64.0, y: 128.0, vx: 1.0, vy: 0.0,
            weapon: 1, start_tick: 500,
        }));
        assert!(ProjectileNet::from_object(&make(
            PROJECTILE_TYPE, vec![0,0,0,0,8,500])).is_none());
        assert!(ProjectileNet::from_object(&make(
            PROJECTILE_TYPE, vec![0,0,0,0,1])).is_none());
        assert!(ProjectileNet::from_object(&make(
            PROJECTILE_TYPE, vec![0,0,0,0,1,-1])).is_none());
        let beam = make(LASER_TYPE, vec![100, 120, 40, 120, 700]);
        assert_eq!(LaserNet::from_object(&beam), Some(LaserNet {
            to_x: 100, to_y: 120, from_x: 40, from_y: 120,
            start_tick: 700, kind: 0,
        }));
        assert!(LaserNet::from_object(&make(
            LASER_TYPE, vec![10000,0,0,0,1])).is_none());
        assert!(LaserNet::from_object(&make(
            LASER_TYPE, vec![0,0,0,0,-1])).is_none());
        assert!(LaserNet::from_object(&make(
            PICKUP_TYPE, vec![0,0,0,0,1])).is_none());
    }

    #[test]
    fn vanilla_pickups_match_upstream_netobject_schema_and_reject_invalid() {
        let p = |words: Vec<i32>| SnapshotObject {
            type_id: PICKUP_TYPE, id: 9, words,
        };
        for kind in 0..=8 {
            let mut data = p(vec![120, -64, kind, 0]);
            assert_eq!(PickupNet::from_object(&data), Some(PickupNet {
                x: 120, y: -64, kind: kind as u8, subtype: 0,
                flags: 0, switch_number: 0,
            }));
            if kind == 2 {
                data.words[3] = 5;
                assert!(PickupNet::from_object(&data).is_some());
                data.words[3] = 6;
                assert!(PickupNet::from_object(&data).is_none());
            }
        }
        assert!(PickupNet::from_object(&p(vec![0, 0, 9, 0])).is_none());
        assert!(PickupNet::from_object(&p(vec![0, 0, -1, 0])).is_none());
        assert!(PickupNet::from_object(&p(vec![0, 0, 0, -1])).is_none());
        assert!(PickupNet::from_object(&p(vec![0, 0, 0])).is_none());
        let mut wrong = p(vec![0, 0, 0, 0]);
        wrong.type_id = CHARACTER_TYPE;
        assert!(PickupNet::from_object(&wrong).is_none());
    }

    #[test]
    fn local_hud_status_reads_vanilla_character_fields_and_rejects_other_players() {
        let mut words = vec![0; 22];
        words[16] = 7; words[17] = 4; words[18] = -1; words[19] = 3;
        let snapshot = SnapshotState { items: vec![
            SnapshotObject { type_id: PLAYERINFO_TYPE, id: 1,
                words: vec![1, 1, 0, 10, 40] },
            SnapshotObject { type_id: PLAYERINFO_TYPE, id: 2,
                words: vec![0, 2, 0, 10, 40] },
            SnapshotObject { type_id: CHARACTER_TYPE, id: 1,
                words: words.clone() },
            SnapshotObject { type_id: CHARACTER_TYPE, id: 2,
                words: vec![0; 22] },
        ] };
        let status = CharacterHudStatus::local_from_snapshot(&snapshot).unwrap();
        assert_eq!(status, CharacterHudStatus {
            health: 7, armor: 4, ammo: -1, weapon: 3,
        });
        let mut invalid = snapshot.clone();
        invalid.items[2].words[16] = 11;
        assert!(CharacterHudStatus::local_from_snapshot(&invalid).is_none());
        invalid.items[2].words[16] = 7;
        invalid.items[0].words[0] = 0;
        assert!(CharacterHudStatus::local_from_snapshot(&invalid).is_none());
    }

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
