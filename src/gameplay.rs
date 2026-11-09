//! Vanilla Teeworlds/DDNet 0.6 gameplay messages and weapon input.
//! Source: upstream/ddnet/datasrc/network.py and controls.cpp, tag 20.2.1.

use crate::game_protocol::{encode_message, MessageError, MessageId};
use crate::protocol::{write_int, write_string, Reader};
use crate::tuning_wire::{ServerTuning, SV_TUNEPARAMS};

pub const SV_MOTD: u16 = 1;
pub const SV_BROADCAST: u16 = 2;
pub const SV_CHAT: u16 = 3;
pub const CL_SAY: u16 = 17;
pub const CL_SETTEAM: u16 = 18;
pub const CL_KILL: u16 = 22;
pub const MAX_CHAT_BYTES: usize = 512;
const MAX_SERVER_NOTICE_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerGameplayMessage {
    Motd(String),
    Broadcast(String),
    Chat { team: i32, client_id: i32, text: String },
    Tuning(ServerTuning),
}

/// Unknown game messages are not an error. This is essential for servers
/// using additional DDNet 0.6 messages not yet implemented in this client.
pub fn decode_server_message(
    id: u16, payload: &[u8],
) -> Result<Option<ServerGameplayMessage>, MessageError> {
    let mut reader = Reader::new(payload);
    let result = match id {
        SV_MOTD | SV_BROADCAST => {
            let text = reader.read_string()?;
            if text.len() > MAX_SERVER_NOTICE_BYTES {
                return Err(MessageError::InvalidMessage);
            }
            if id == SV_MOTD {
                ServerGameplayMessage::Motd(text.to_owned())
            } else {
                ServerGameplayMessage::Broadcast(text.to_owned())
            }
        }
        SV_TUNEPARAMS => {
            return Ok(Some(ServerGameplayMessage::Tuning(ServerTuning::decode(payload)?)));
        }
        SV_CHAT => {
            let team = reader.read_int()?;
            let client_id = reader.read_int()?;
            let text = reader.read_string()?;
            if !(-2..=3).contains(&team) || !(-1..64).contains(&client_id)
                || text.len() > MAX_CHAT_BYTES * 2
            {
                return Err(MessageError::InvalidMessage);
            }
            ServerGameplayMessage::Chat {
                team, client_id, text: text.to_owned(),
            }
        }
        _ => return Ok(None),
    };
    if reader.remaining() != 0 {
        return Err(MessageError::InvalidMessage);
    }
    Ok(Some(result))
}

fn client_message(id: u16, payload: &[u8]) -> Result<Vec<u8>, MessageError> {
    encode_message(MessageId::Regular { system: false, number: id }, payload)
}

/// DDNet 0.6 Cl_Say: team flag, NUL-terminated UTF-8 message.
/// Chat messages are transmitted as vital chunks by GameSession06.
pub fn client_say(team: bool, text: &str) -> Result<Vec<u8>, MessageError> {
    if text.is_empty() || text.len() > MAX_CHAT_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(MessageError::InvalidMessage);
    }
    let mut payload = Vec::with_capacity(text.len() + 8);
    write_int(i32::from(team), &mut payload);
    write_string(text, &mut payload)?;
    client_message(CL_SAY, &payload)
}

pub fn client_set_team(team: i32) -> Result<Vec<u8>, MessageError> {
    if !(-1..=1).contains(&team) {
        return Err(MessageError::InvalidMessage);
    }
    let mut data = Vec::new();
    write_int(team, &mut data);
    client_message(CL_SETTEAM, &data)
}

pub fn client_kill() -> Vec<u8> {
    // Non-system game message 22: no payload.
    client_message(CL_KILL, &[]).expect("fixed regular message ID")
}

/// DDNet's wanted weapon uses 1=hammer ... 5=laser; zero is automatic.
/// Wheel next/previous use transition counters, not selected slot numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WeaponControls {
    pub wanted_weapon: i32,
    pub next_weapon: i32,
    pub prev_weapon: i32,
}

impl WeaponControls {
    pub fn select(&mut self, slot: i32) -> bool {
        if !(1..=5).contains(&slot) {
            return false;
        }
        self.wanted_weapon = slot;
        true
    }

    pub fn scroll(&mut self, wheel_steps: i32) {
        // One press/release pair advances the official 6-bit input counter by
        // two. Clamp an abnormal wheel delta to avoid huge jumps in one frame.
        let steps = wheel_steps.clamp(-8, 8);
        if steps == 0 { return; }
        self.wanted_weapon = 0;
        if steps > 0 {
            self.prev_weapon = (self.prev_weapon + steps * 2) & 63;
        } else {
            self.next_weapon = (self.next_weapon + -steps * 2) & 63;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_protocol::decode_message;

    #[test]
    fn vanilla_gameplay_message_ids_and_field_order() {
        let say_bytes = client_say(false, "hello").unwrap();
        let say = decode_message(&say_bytes).unwrap();
        assert_eq!(say.id, MessageId::Regular { system: false, number: 17 });
        let mut read = Reader::new(say.payload);
        assert_eq!(read.read_int(), Ok(0));
        assert_eq!(read.read_string(), Ok("hello"));
        assert_eq!(read.remaining(), 0);

        let say_bytes = client_say(true, "Привет").unwrap();
        let say = decode_message(&say_bytes).unwrap();
        let mut read = Reader::new(say.payload);
        assert_eq!(read.read_int(), Ok(1));
        assert_eq!(read.read_string(), Ok("Привет"));

        let spec_bytes = client_set_team(-1).unwrap();
        let spec = decode_message(&spec_bytes).unwrap();
        assert_eq!(spec.id, MessageId::Regular { system: false, number: 18 });
        assert_eq!(Reader::new(spec.payload).read_int(), Ok(-1));
        assert_eq!(decode_message(&client_kill()).unwrap().id,
            MessageId::Regular { system: false, number: 22 });
    }

    #[test]
    fn chat_rejects_controls_oversize_and_invalid_team() {
        for invalid in ["", "hello\nworld", "x\u{0000}", "a\r", &"z".repeat(513)] {
            assert_eq!(client_say(false, invalid), Err(MessageError::InvalidMessage));
        }
        for team in [-2, 2, 99] {
            assert_eq!(client_set_team(team), Err(MessageError::InvalidMessage));
        }
    }

    #[test]
    fn decode_chat_broadcast_motd_and_ignore_unknown_extensions() {
        let mut bytes = Vec::new();
        write_int(0, &mut bytes);
        write_int(12, &mut bytes);
        write_string("hello", &mut bytes).unwrap();
        assert_eq!(decode_server_message(SV_CHAT, &bytes), Ok(Some(
            ServerGameplayMessage::Chat {
                team: 0, client_id: 12, text: "hello".into(),
            }
        )));
        assert_eq!(decode_server_message(SV_MOTD, b"welcome\0"), Ok(Some(
            ServerGameplayMessage::Motd("welcome".into())
        )));
        assert_eq!(decode_server_message(SV_BROADCAST, b"finish!\0"), Ok(Some(
            ServerGameplayMessage::Broadcast("finish!".into())
        )));
        assert_eq!(decode_server_message(900, &bytes), Ok(None));
        bytes.push(0);
        assert_eq!(decode_server_message(SV_CHAT, &bytes), Err(MessageError::InvalidMessage));
        assert!(decode_server_message(SV_MOTD, b"no terminator").is_err());
        let mut invalid = Vec::new();
        write_int(9, &mut invalid);
        write_int(1, &mut invalid);
        write_string("bad", &mut invalid).unwrap();
        assert_eq!(decode_server_message(SV_CHAT, &invalid), Err(MessageError::InvalidMessage));
    }

    #[test]
    fn tune_params_is_regular_game_message_six() {
        let mut payload = Vec::new();
        for _ in 0..47 { write_int(100, &mut payload); }
        let wire = crate::game_protocol::encode_message(
            MessageId::Regular { system: false, number: 6 }, &payload,
        ).unwrap();
        let decoded = decode_message(&wire).unwrap();
        match decode_server_message(6, decoded.payload).unwrap() {
            Some(ServerGameplayMessage::Tuning(t)) => assert_eq!(t.values, [100; 47]),
            other => panic!("unexpected message {other:?}"),
        }
    }

    #[test]
    fn weapon_selection_and_wheel_use_ddnet_input_counters() {
        let mut weapons = WeaponControls::default();
        assert_eq!(weapons.wanted_weapon, 0);
        assert!(!weapons.select(6));
        assert!(weapons.select(5));
        assert_eq!(weapons.wanted_weapon, 5);
        weapons.scroll(-1);
        assert_eq!(weapons.wanted_weapon, 0);
        assert_eq!(weapons.next_weapon, 2);
        weapons.scroll(2);
        assert_eq!(weapons.prev_weapon, 4);
        weapons.scroll(0);
        assert_eq!(weapons.prev_weapon, 4);
        weapons.scroll(100);
        assert_eq!(weapons.prev_weapon, 20);
        for _ in 0..30 { weapons.scroll(-1); }
        assert_eq!(weapons.next_weapon, 62);
    }
}
