//! Vanilla DDNet 0.6 player input and NETMSG_INPUT wire format.
//!
//! Source: upstream/ddnet/datasrc/network.py PlayerInput and
//! src/engine/client/client.cpp SendInput, tag 20.2.1.
//!
//! The server expects a non-vital system message with last ACKed snapshot,
//! predicted tick, byte size, and exactly ten signed 32-bit input fields.
//! Keyboard/controller sampling and client-side prediction clocks are not
//! implemented here.

use crate::game_protocol::{encode_message, MessageError, MessageId, NETMSG_INPUT};
use crate::protocol::{write_int, Reader, CodecError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    InvalidTick,
    InvalidSnapshotAck,
    InvalidDirection,
    InvalidPlayerFlags,
    Codec(CodecError),
    Message(MessageError),
    InvalidDataLength,
}

impl From<CodecError> for InputError {
    fn from(value: CodecError) -> Self { Self::Codec(value) }
}
impl From<MessageError> for InputError {
    fn from(value: MessageError) -> Self { Self::Message(value) }
}

/// Exactly the 10 i32 fields in CNetObj_PlayerInput.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlayerInput {
    pub direction: i32,
    pub target_x: i32,
    pub target_y: i32,
    pub jump: i32,
    pub fire: i32,
    pub hook: i32,
    pub player_flags: i32,
    pub wanted_weapon: i32,
    pub next_weapon: i32,
    pub prev_weapon: i32,
}

impl PlayerInput {
    pub const fn as_words(self) -> [i32; 10] {
        [
            self.direction, self.target_x, self.target_y, self.jump,
            self.fire, self.hook, self.player_flags, self.wanted_weapon,
            self.next_weapon, self.prev_weapon,
        ]
    }

    pub fn validate(&self) -> Result<(), InputError> {
        if !(-1..=1).contains(&self.direction) {
            return Err(InputError::InvalidDirection);
        }
        if !(0..=256).contains(&self.player_flags) {
            return Err(InputError::InvalidPlayerFlags);
        }
        Ok(())
    }
}

/// Last acknowledged snapshot must be -1 until a snapshot is decoded.
/// Packet size is always 40, even though the wire uses variable integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputFrame {
    pub last_ack_snapshot: i32,
    pub intended_tick: i32,
    pub input: PlayerInput,
}

impl InputFrame {
    pub fn validate(&self) -> Result<(), InputError> {
        if !(0..=0x6fff_ffff).contains(&self.intended_tick) {
            return Err(InputError::InvalidTick);
        }
        if self.last_ack_snapshot < -1
            || self.last_ack_snapshot > self.intended_tick
        {
            return Err(InputError::InvalidSnapshotAck);
        }
        self.input.validate()
    }

    pub fn pack(&self) -> Result<Vec<u8>, InputError> {
        self.validate()?;
        let mut data = Vec::with_capacity(64);
        write_int(self.last_ack_snapshot, &mut data);
        write_int(self.intended_tick, &mut data);
        write_int(40, &mut data);
        for value in self.input.as_words() {
            write_int(value, &mut data);
        }
        Ok(encode_message(
            MessageId::Regular { system: true, number: NETMSG_INPUT },
            &data,
        )?)
    }

    /// Decode an individual NETMSG_INPUT payload, not its system-message ID.
    pub fn unpack_payload(data: &[u8]) -> Result<Self, InputError> {
        let mut reader = Reader::new(data);
        let last_ack_snapshot = reader.read_int()?;
        let intended_tick = reader.read_int()?;
        if reader.read_int()? != 40 {
            return Err(InputError::InvalidDataLength);
        }
        let mut fields = [0i32; 10];
        for field in &mut fields {
            *field = reader.read_int()?;
        }
        if reader.remaining() != 0 {
            return Err(InputError::InvalidDataLength);
        }
        let result = Self {
            last_ack_snapshot,
            intended_tick,
            input: PlayerInput {
                direction: fields[0], target_x: fields[1],
                target_y: fields[2], jump: fields[3], fire: fields[4],
                hook: fields[5], player_flags: fields[6],
                wanted_weapon: fields[7], next_weapon: fields[8],
                prev_weapon: fields[9],
            }
        };
        result.validate()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_protocol::{decode_message, NETMSG_INPUT};

    #[test]
    fn neutral_input_matches_ddnet_cpp_wire_structure() {
        let frame = InputFrame {
            last_ack_snapshot: -1,
            intended_tick: 99,
            input: PlayerInput::default(),
        };
        let bytes = frame.pack().unwrap();
        let message = decode_message(&bytes).unwrap();
        assert_eq!(message.id, MessageId::Regular { system: true, number: NETMSG_INPUT });
        let mut reader = Reader::new(message.payload);
        assert_eq!(reader.read_int(), Ok(-1));
        assert_eq!(reader.read_int(), Ok(99));
        assert_eq!(reader.read_int(), Ok(40));
        for _ in 0..10 {
            assert_eq!(reader.read_int(), Ok(0));
        }
        assert_eq!(reader.remaining(), 0);
        assert_eq!(InputFrame::unpack_payload(message.payload), Ok(frame));
    }

    #[test]
    fn client_controls_roundtrip_and_packet_limits() {
        let frame = InputFrame {
            last_ack_snapshot: 198,
            intended_tick: 201,
            input: PlayerInput {
                direction: -1,
                target_x: 750,
                target_y: -500,
                jump: 1, fire: 10, hook: 1, player_flags: 2,
                wanted_weapon: 3, next_weapon: 0, prev_weapon: 1,
            }
        };
        let packet = frame.pack().unwrap();
        let decoded = decode_message(&packet).unwrap();
        assert_eq!(InputFrame::unpack_payload(decoded.payload), Ok(frame));
        assert!(packet.len() < 1023);
    }

    #[test]
    fn malformed_frames_are_rejected() {
        let mut frame = InputFrame {
            last_ack_snapshot: -1,
            intended_tick: 1,
            input: PlayerInput::default(),
        };
        frame.intended_tick = -1;
        assert_eq!(frame.pack(), Err(InputError::InvalidTick));
        frame.intended_tick = 1;
        frame.last_ack_snapshot = 2;
        assert_eq!(frame.pack(), Err(InputError::InvalidSnapshotAck));
        frame.last_ack_snapshot = -1;
        frame.input.direction = 5;
        assert_eq!(frame.pack(), Err(InputError::InvalidDirection));
        frame.input.direction = 0;
        frame.input.player_flags = -1;
        assert_eq!(frame.pack(), Err(InputError::InvalidPlayerFlags));
        frame.input.player_flags = 0;
        let bytes = frame.pack().unwrap();
        let packet = decode_message(&bytes).unwrap();
        for n in 0..packet.payload.len() {
            assert!(InputFrame::unpack_payload(&packet.payload[..n]).is_err());
        }
        let mut extra = packet.payload.to_vec();
        extra.push(0);
        assert_eq!(InputFrame::unpack_payload(&extra), Err(InputError::InvalidDataLength));
    }

    #[test]
    fn randomized_input_frames_roundtrip() {
        let mut random = 0x6139_5124u32;
        for tick in 0..10000i32 {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            let input = PlayerInput {
                direction: ((random % 3) as i32) - 1,
                target_x: random as i32,
                target_y: (!random) as i32,
                jump: tick % 2, fire: tick / 2, hook: tick % 3,
                player_flags: (random % 257) as i32,
                wanted_weapon: (random % 6) as i32,
                next_weapon: 0, prev_weapon: 0,
            };
            let frame = InputFrame {
                last_ack_snapshot: tick - 1, intended_tick: tick,
                input,
            };
            let packed = frame.pack().unwrap();
            let decoded = decode_message(&packed).unwrap();
            assert_eq!(InputFrame::unpack_payload(decoded.payload), Ok(frame));
        }
    }
}
