//! Headless DDNet 0.6 client protocol flow: transport -> INFO -> map download
//! -> READY -> ENTERGAME. Original flow: upstream/ddnet/src/engine/client/client.cpp
//! DDNet 20.2.1. This is still an experimental client, not fully playable.
//!
//! Tests simulate the real upstream wire format end-to-end with a tiny v3 map,
//! including valid ACK sequences and CRC verification.

use crate::datafile::{Datafile, DatafileError};
use crate::game_protocol::{
    client_enter_game, client_info, client_ping_reply, client_ready, decode_message,
    request_map_chunk, InputTiming, MapChange, MapData, MapDownload, MessageError, MessageId, StartInfo,
    NETMSGTYPE_SV_READYTOENTER,
    NETMSG_CON_READY, NETMSG_MAP_CHANGE, NETMSG_MAP_DATA, NETMSG_PING, NETMSG_INPUTTIMING,
    NETMSG_SNAP, NETMSG_SNAPEMPTY, NETMSG_SNAPSINGLE,
};
use crate::map::{GameMap, MapError};
use crate::gameplay::{
    client_kill, client_say, client_set_team, decode_server_message,
    ServerGameplayMessage,
};
use crate::transport::{Client06, Event, TransportError};
use crate::snapshot::{SnapshotAssembler, SnapshotError, SnapshotPacket};
use crate::snapshot_delta::{DeltaDecoder, SnapshotState};
use crate::input::{InputFrame, InputError};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Offline,
    Connecting,
    WaitingMap,
    Downloading,
    WaitingConReady,
    WaitingReadyToEnter,
    EnterGameSent,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameAction {
    /// Caller must transmit this datagram to the connected server.
    Outgoing(Vec<u8>),
    TransportConnected,
    MapAnnounced { name: String, size: usize },
    MapLoaded { name: String, width: usize, height: usize },
    StartInfoSent,
    EnterGameSent,
    SnapshotReceived { tick: i32, packed_bytes: usize },
    SnapshotDecoded { tick: i32, item_count: usize },
    /// A missing reference, corrupt delta or CRC mismatch requires a fresh
    /// complete snapshot. The next NETMSG_INPUT carries ACK=-1.
    SnapshotRecoveryRequested { tick: i32, reason: SnapshotRecoveryReason },
    InputTiming { tick: i32, time_left_ms: i32 },
    ChatReceived { team: i32, client_id: i32, text: String },
    ServerBroadcast(String),
    ServerMotd(String),
    Disconnected(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRecoveryReason {
    MissingBase,
    InvalidDelta,
    CrcMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Transport(TransportError),
    Message(MessageError),
    Datafile(DatafileError),
    Map(MapError),
    Snapshot(SnapshotError),
    Input(InputError),
    UnexpectedStage,
}
impl From<TransportError> for SessionError {
    fn from(value: TransportError) -> Self { Self::Transport(value) }
}
impl From<MessageError> for SessionError {
    fn from(value: MessageError) -> Self { Self::Message(value) }
}
impl From<DatafileError> for SessionError {
    fn from(value: DatafileError) -> Self { Self::Datafile(value) }
}
impl From<MapError> for SessionError {
    fn from(value: MapError) -> Self { Self::Map(value) }
}
impl From<SnapshotError> for SessionError {
    fn from(value: SnapshotError) -> Self { Self::Snapshot(value) }
}
impl From<InputError> for SessionError {
    fn from(value: InputError) -> Self { Self::Input(value) }
}

#[derive(Debug, Clone)]
pub struct GameSession06 {
    pub stage: Stage,
    pub transport: Client06,
    pub loaded_map: Option<GameMap>,
    /// CRC-verified downloaded DATA bytes retained for original map graphics.
    /// Bounded by max_map_bytes and cleared on map change/reconnect.
    pub loaded_map_bytes: Option<Vec<u8>>,
    pub start_info: StartInfo,
    password: String,
    max_map_bytes: usize,
    max_tiles: usize,
    pending_map: Option<MapDownload>,
    map_name: Option<String>,
    snapshots: SnapshotAssembler,
    decoder: DeltaDecoder,
    snapshot_states: BTreeMap<i32, SnapshotState>,
    /// Last verified snapshot ACK to send; -1 requests a full snapshot.
    snapshot_ack_tick: i32,
    snapshot_recovering: bool,
    /// Only match feedback to inputs actually sent on this connection.
    /// Bound the queue to 128 recent ticks to prevent unbounded growth.
    pending_input_ticks: VecDeque<i32>,
    pub last_input_timing: Option<InputTiming>,
}

impl GameSession06 {
    pub fn new(password: String, max_map_bytes: usize, max_tiles: usize) -> Self {
        Self {
            stage: Stage::Offline,
            transport: Client06::new(),
            loaded_map: None,
            loaded_map_bytes: None,
            start_info: StartInfo::default(),
            password,
            max_map_bytes,
            max_tiles,
            pending_map: None,
            map_name: None,
            snapshots: SnapshotAssembler::new(),
            decoder: DeltaDecoder::for_vanilla_06(),
            snapshot_states: BTreeMap::new(),
            snapshot_ack_tick: -1,
            snapshot_recovering: false,
            pending_input_ticks: VecDeque::new(),
            last_input_timing: None,
        }
    }

    pub fn connect(&mut self) -> Result<Vec<u8>, SessionError> {
        if self.stage != Stage::Offline {
            return Err(SessionError::UnexpectedStage);
        }
        let start = self.transport.begin_connect()?;
        self.stage = Stage::Connecting;
        Ok(start)
    }

    /// Start a new 0.6 handshake using the current credentials and limits.
    /// Clears the previous transport's ACK window, unacknowledged packets,
    /// pending map chunks and snapshot history atomically.
    ///
    /// Caller must only use this on an explicitly failed connection or
    /// user-requested reconnect. It does not retry on intentional kicks.
    pub fn restart_connection(&mut self) -> Result<Vec<u8>, SessionError> {
        let mut fresh = Self::new(
            self.password.clone(), self.max_map_bytes, self.max_tiles,
        );
        fresh.start_info = self.start_info.clone();
        let hello = fresh.connect()?;
        *self = fresh;
        Ok(hello)
    }

    /// Parse one server UDP datagram. All output packets are returned as actions.
    pub fn receive(&mut self, data: &[u8]) -> Result<Vec<GameAction>, SessionError> {
        let events = self.transport.receive(data)?;
        let mut actions = Vec::new();
        for event in events {
            match event {
                Event::Outgoing(packet) => actions.push(GameAction::Outgoing(packet)),
                Event::Connected => {
                    if self.stage != Stage::Connecting {
                        return Err(SessionError::UnexpectedStage);
                    }
                    self.stage = Stage::WaitingMap;
                    actions.push(GameAction::TransportConnected);
                    let info = client_info(&self.password)?;
                    let wire = self.transport.send_message(&info, true)?;
                    actions.push(GameAction::Outgoing(wire));
                }
                Event::Message(payload) => {
                    self.receive_message(&payload, &mut actions)?;
                }
                Event::Disconnected(reason) => {
                    self.stage = Stage::Closed;
                    actions.push(GameAction::Disconnected(reason));
                }
            }
        }
        Ok(actions)
    }

    fn send_system(
        &mut self, packet: &[u8], vital: bool, actions: &mut Vec<GameAction>,
    ) -> Result<(), SessionError> {
        let bytes = self.transport.send_message(packet, vital)?;
        actions.push(GameAction::Outgoing(bytes));
        Ok(())
    }

    fn receive_message(
        &mut self, bytes: &[u8], actions: &mut Vec<GameAction>,
    ) -> Result<(), SessionError> {
        let message = decode_message(bytes)?;
        if let MessageId::Regular {
            system: false, number: NETMSGTYPE_SV_READYTOENTER,
        } = message.id {
            if self.stage != Stage::WaitingReadyToEnter {
                return Err(SessionError::UnexpectedStage);
            }
            self.send_system(&client_enter_game(), true, actions)?;
            self.stage = Stage::EnterGameSent;
            actions.push(GameAction::EnterGameSent);
            return Ok(());
        }
        let MessageId::Regular { system, number } = message.id else {
            // Unknown UUID extension: leave protocol extensions to their
            // independent handlers instead of failing the game session.
            return Ok(());
        };
        if !system {
            match decode_server_message(number, message.payload)? {
                Some(ServerGameplayMessage::Chat { team, client_id, text }) =>
                    actions.push(GameAction::ChatReceived { team, client_id, text }),
                Some(ServerGameplayMessage::Broadcast(text)) =>
                    actions.push(GameAction::ServerBroadcast(text)),
                Some(ServerGameplayMessage::Motd(text)) =>
                    actions.push(GameAction::ServerMotd(text)),
                None => {}
            }
            return Ok(());
        }
        match number {
            NETMSG_MAP_CHANGE => {
                if self.stage != Stage::WaitingMap && self.stage != Stage::Downloading {
                    return Err(SessionError::UnexpectedStage);
                }
                let change = MapChange::parse(message.payload)?;
                let pending = MapDownload::new(change.clone(), self.max_map_bytes)?;
                self.pending_map = Some(pending);
                self.map_name = Some(change.name.clone());
                self.loaded_map = None;
                self.loaded_map_bytes = None;
                self.snapshots = SnapshotAssembler::new();
                self.snapshot_states.clear();
                self.snapshot_ack_tick = -1;
                self.snapshot_recovering = false;
                self.pending_input_ticks.clear();
                self.last_input_timing = None;
                self.stage = Stage::Downloading;
                actions.push(GameAction::MapAnnounced {
                    name: change.name, size: change.size,
                });
                self.send_system(&request_map_chunk(0)?, true, actions)?;
            }
            NETMSG_MAP_DATA => {
                if self.stage != Stage::Downloading {
                    return Err(SessionError::UnexpectedStage);
                }
                let chunk = MapData::parse(message.payload)?;
                let progress = self.pending_map.as_mut()
                    .ok_or(SessionError::UnexpectedStage)?
                    .ingest(chunk)?;
                if progress.complete {
                    let map_bytes = self.pending_map.take()
                        .ok_or(SessionError::UnexpectedStage)?
                        .into_map_data()?;
                    let datafile = Datafile::parse(&map_bytes)?;
                    let map = GameMap::from_datafile(&datafile, self.max_tiles)?;
                    let name = self.map_name.clone().unwrap_or_default();
                    actions.push(GameAction::MapLoaded {
                        name, width: map.width, height: map.height,
                    });
                    self.loaded_map = Some(map);
                    self.loaded_map_bytes = Some(map_bytes);
                    self.stage = Stage::WaitingConReady;
                    self.send_system(&client_ready(), true, actions)?;
                } else {
                    self.send_system(&request_map_chunk(progress.next_chunk)?, true, actions)?;
                }
            }
            NETMSG_CON_READY => {
                if self.stage != Stage::WaitingConReady {
                    return Err(SessionError::UnexpectedStage);
                }
                let info = self.start_info.pack()?;
                self.send_system(&info, true, actions)?;
                self.stage = Stage::WaitingReadyToEnter;
                actions.push(GameAction::StartInfoSent);
            }
            NETMSG_SNAP | NETMSG_SNAPSINGLE | NETMSG_SNAPEMPTY => {
                if self.stage != Stage::EnterGameSent {
                    return Err(SessionError::UnexpectedStage);
                }
                let packet = SnapshotPacket::parse(number, message.payload)?;
                let assembled = match self.snapshots.push(packet) {
                    Ok(snapshot) => snapshot,
                    // A delayed UDP snapshot does not invalidate the active
                    // stream, even when a newer multipart tick is underway.
                    Err(SnapshotError::OldTick) => None,
                    Err(error) => return Err(error.into()),
                };
                if let Some(snapshot) = assembled {
                    actions.push(GameAction::SnapshotReceived {
                        tick: snapshot.game_tick,
                        packed_bytes: snapshot.packed_bytes.len(),
                    });
                    self.try_decode_snapshot(&snapshot, actions);
                }
            }
            NETMSG_INPUTTIMING => {
                if self.stage != Stage::EnterGameSent {
                    return Err(SessionError::UnexpectedStage);
                }
                let timing = InputTiming::parse(message.payload)?;
                // Ignore unsolicited, stale, out-of-order or duplicated
                // timing messages, rather than adjusting the clock from
                // unconfirmed server data.
                if let Some(pos) = self.pending_input_ticks.iter()
                    .position(|&tick| tick == timing.intended_tick)
                {
                    for _ in 0..=pos {
                        self.pending_input_ticks.pop_front();
                    }
                    if self.last_input_timing.is_none_or(|prev|
                        timing.intended_tick > prev.intended_tick
                    ) {
                        self.last_input_timing = Some(timing);
                        actions.push(GameAction::InputTiming {
                            tick: timing.intended_tick,
                            time_left_ms: timing.time_left_ms,
                        });
                    }
                }
            }
            NETMSG_PING => {
                self.send_system(&client_ping_reply(), false, actions)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Ask the server for an un-deltaed snapshot through the next input's
    /// ACK=-1. Only emit the diagnostic once per recovery episode. Preserve
    /// the last CRC-verified snapshots for rendering during the gap.
    fn request_snapshot_recovery(
        &mut self, tick: i32, reason: SnapshotRecoveryReason,
        actions: &mut Vec<GameAction>,
    ) {
        self.snapshot_ack_tick = -1;
        if !self.snapshot_recovering {
            self.snapshot_recovering = true;
            actions.push(GameAction::SnapshotRecoveryRequested { tick, reason });
        }
    }

    /// Reconstruct verified snapshots. A damaged delta never advances the
    /// ACK or overwrites state; recovery requests a fresh server baseline.
    fn try_decode_snapshot(
        &mut self,
        snapshot: &crate::snapshot::AssembledSnapshot,
        actions: &mut Vec<GameAction>,
    ) {
        // A repeated accepted frame must not replay effects or move time back.
        if self.snapshot_states.last_key_value()
            .is_some_and(|(&last, _)| snapshot.game_tick <= last)
        {
            return;
        }
        let empty = SnapshotState::default();
        let reference = if snapshot.delta_tick < 0 {
            Some(&empty)
        } else {
            self.snapshot_states.get(&snapshot.delta_tick)
        };
        let Some(reference) = reference else {
            self.request_snapshot_recovery(
                snapshot.game_tick, SnapshotRecoveryReason::MissingBase, actions,
            );
            return;
        };
        let Ok(state) = self.decoder.unpack(&snapshot.packed_bytes, reference) else {
            self.request_snapshot_recovery(
                snapshot.game_tick, SnapshotRecoveryReason::InvalidDelta, actions,
            );
            return;
        };
        if snapshot.crc.is_some_and(|expected| expected != state.checksum()) {
            self.request_snapshot_recovery(
                snapshot.game_tick, SnapshotRecoveryReason::CrcMismatch, actions,
            );
            return;
        }

        self.snapshot_ack_tick = snapshot.game_tick;
        self.snapshot_recovering = false;
        actions.push(GameAction::SnapshotDecoded {
            tick: snapshot.game_tick,
            item_count: state.items.len(),
        });
        self.snapshot_states.insert(snapshot.game_tick, state);
        while self.snapshot_states.len() > 64 {
            if let Some(oldest) = self.snapshot_states.keys().next().copied() {
                self.snapshot_states.remove(&oldest);
            }
        }
    }

    /// ACK used by NETMSG_INPUT. -1 requests a complete snapshot after
    /// invalid or missing delta baselines.
    pub fn snapshot_ack_tick(&self) -> i32 { self.snapshot_ack_tick }
    pub fn is_snapshot_recovering(&self) -> bool { self.snapshot_recovering }

    /// Sends one 0.6 system NETMSG_INPUT as an *unreliable* chunk, matching
    /// upstream CClient::SendInput. The caller owns the 50 Hz scheduling.
    pub fn send_input(&mut self, frame: InputFrame) -> Result<Vec<u8>, SessionError> {
        if self.stage != Stage::EnterGameSent {
            return Err(SessionError::UnexpectedStage);
        }
        let bytes = self.transport.send_message(&frame.pack()?, false)?;
        self.pending_input_ticks.push_back(frame.intended_tick);
        if self.pending_input_ticks.len() > 128 {
            self.pending_input_ticks.pop_front();
        }
        Ok(bytes)
    }

    /// Send a vital 0.6 chat message (team=false for public chat).
    pub fn send_chat(&mut self, team: bool, text: &str) -> Result<Vec<u8>, SessionError> {
        if self.stage != Stage::EnterGameSent {
            return Err(SessionError::UnexpectedStage);
        }
        Ok(self.transport.send_message(&client_say(team, text)?, true)?)
    }

    /// Send a vital in-game Cl_ChangeInfo (name, clan, skin and colors).
    /// Update local handshake metadata only once the packet is encoded.
    pub fn change_name(&mut self, name: &str) -> Result<Vec<u8>, SessionError> {
        if self.stage != Stage::EnterGameSent {
            return Err(SessionError::UnexpectedStage);
        }
        let mut new_info = self.start_info.clone();
        new_info.name = name.to_owned();
        let packet = new_info.pack_change()?;
        let wire = self.transport.send_message(&packet, true)?;
        self.start_info = new_info;
        Ok(wire)
    }

    /// -1=spectators, 0=red/regular team, 1=blue (if server permits).
    pub fn set_team(&mut self, team: i32) -> Result<Vec<u8>, SessionError> {
        if self.stage != Stage::EnterGameSent {
            return Err(SessionError::UnexpectedStage);
        }
        Ok(self.transport.send_message(&client_set_team(team)?, true)?)
    }

    /// Request a normal vanilla kill/respawn (server controls cooldown).
    pub fn request_kill(&mut self) -> Result<Vec<u8>, SessionError> {
        if self.stage != Stage::EnterGameSent {
            return Err(SessionError::UnexpectedStage);
        }
        Ok(self.transport.send_message(&client_kill(), true)?)
    }

    pub fn snapshot_state(&self, tick: i32) -> Option<&SnapshotState> {
        self.snapshot_states.get(&tick)
    }

    pub fn map_name(&self) -> Option<&str> {
        self.map_name.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_protocol::{crc32_update, encode_message};
    use crate::network::{
        encode_packet_06, ChunkHeader, PacketHeader, CHUNK_VITAL, FLAG_CONTROL,
    };
    use crate::protocol::{write_int, write_string};
    use std::time::Duration;
    use std::net::UdpSocket;
    use std::thread;

    const TOKEN: u32 = 0x0102_0304;

    fn server_control(code: u8, extra: &[u8]) -> Vec<u8> {
        let mut data = vec![code];
        data.extend_from_slice(extra);
        encode_packet_06(
            PacketHeader { flags: FLAG_CONTROL, ack: 0, num_chunks: 0 }, &data
        ).unwrap()
    }

    fn server_message(id: u16, payload: &[u8], seq: u16, ack: u16) -> Vec<u8> {
        let msg = encode_message(
            MessageId::Regular { system: true, number: id }, payload
        ).unwrap();
        let mut chunk = ChunkHeader {
            flags: CHUNK_VITAL, size: msg.len() as u16, sequence: Some(seq),
        }.encode_06().unwrap();
        chunk.extend_from_slice(&msg);
        chunk.extend_from_slice(&TOKEN.to_be_bytes());
        encode_packet_06(
            PacketHeader { flags: 0, ack, num_chunks: 1 }, &chunk
        ).unwrap()
    }

    fn server_game_message(id: u16, payload: &[u8], seq: u16, ack: u16) -> Vec<u8> {
        let msg = encode_message(
            MessageId::Regular { system: false, number: id }, payload
        ).unwrap();
        let mut chunk = ChunkHeader {
            flags: CHUNK_VITAL, size: msg.len() as u16, sequence: Some(seq),
        }.encode_06().unwrap();
        chunk.extend_from_slice(&msg);
        chunk.extend_from_slice(&TOKEN.to_be_bytes());
        encode_packet_06(PacketHeader { flags: 0, ack, num_chunks: 1 }, &chunk).unwrap()
    }

    fn push_i32(data: &mut Vec<u8>, value: i32) {
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn small_real_datafile_v3() -> Vec<u8> {
        // Exact DATA v3 layout: one game layer item and one CTile.
        let mut data = Vec::new();
        data.extend_from_slice(b"DATA");
        for value in [3, 0, 0, 1, 1, 1, 68, 4] {
            push_i32(&mut data, value);
        }
        for value in [5, 0, 1, 0, 0, 5 << 16, 60] {
            push_i32(&mut data, value);
        }
        let mut item = [0i32; 15];
        item[1] = 2; // LAYERTYPE_TILES
        item[3] = 2; // Tilemap version 2
        item[4] = 1; // Width
        item[5] = 1; // Height
        item[6] = 1; // Game physics layer flag
        item[14] = 0; // Raw data block index
        for value in item {
            push_i32(&mut data, value);
        }
        data.extend_from_slice(&[1, 0, 0, 0]);
        let size = i32::try_from(data.len() - 16).unwrap();
        data[8..12].copy_from_slice(&size.to_le_bytes());
        data
    }

    fn feed_handshake(session: &mut GameSession06) -> Vec<GameAction> {
        assert_eq!(session.stage, Stage::Offline);
        let hello = session.connect().unwrap();
        assert_eq!(&hello[..4], &[0x10, 0, 0, 1]);
        let mut extra = b"TKEN".to_vec();
        extra.extend_from_slice(&TOKEN.to_be_bytes());
        session.receive(&server_control(2, &extra)).unwrap()
    }

    fn make_map_change(data: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        write_string("mini", &mut wire).unwrap();
        write_int(crc32_update(0, data) as i32, &mut wire);
        write_int(data.len() as i32, &mut wire);
        wire
    }

    fn make_map_data(data: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        for value in [1i32, crc32_update(0, data) as i32, 0, data.len() as i32] {
            write_int(value, &mut wire);
        }
        wire.extend_from_slice(data);
        wire
    }

    #[test]
    fn full_headless_vanilla_06_map_login_simulation() {
        let map = small_real_datafile_v3();
        let mut client = GameSession06::new(String::new(), 1024 * 1024, 1024);
        let hello = feed_handshake(&mut client);
        assert_eq!(client.stage, Stage::WaitingMap);
        assert!(hello.contains(&GameAction::TransportConnected));
        assert_eq!(hello.iter().filter(|action| matches!(action, GameAction::Outgoing(_))).count(), 2);
        assert_eq!(client.transport.sequence, 1); // INFO was vital.

        let announced = client.receive(&server_message(
            NETMSG_MAP_CHANGE, &make_map_change(&map), 1, 1
        )).unwrap();
        assert!(announced.contains(&GameAction::MapAnnounced {
            name: "mini".into(), size: map.len(),
        }));
        assert_eq!(client.stage, Stage::Downloading);
        assert_eq!(client.transport.sequence, 2); // REQUEST_MAP_DATA vital.

        let downloaded = client.receive(&server_message(
            NETMSG_MAP_DATA, &make_map_data(&map), 2, 2
        )).unwrap();
        assert!(downloaded.contains(&GameAction::MapLoaded {
            name: "mini".into(), width: 1, height: 1,
        }));
        assert_eq!(client.stage, Stage::WaitingConReady);
        assert_eq!(client.loaded_map.as_ref().unwrap().tiles[0].index, 1);
        assert_eq!(client.loaded_map_bytes.as_deref(), Some(map.as_slice()));
        assert_eq!(client.transport.sequence, 3); // READY vital.

        let started = client.receive(&server_message(
            NETMSG_CON_READY, &[], 3, 3
        )).unwrap();
        assert!(started.contains(&GameAction::StartInfoSent));
        assert_eq!(client.stage, Stage::WaitingReadyToEnter);
        assert_eq!(client.transport.sequence, 4);
        let outgoing = started.iter().find_map(|event| match event {
            GameAction::Outgoing(packet) => {
                let crate::network::Packet::Connected { payload, .. } =
                    crate::network::parse_packet_06(packet).ok()?
                else { return None };
                let chunks = crate::network::parse_chunks_06(&payload[..payload.len()-4], 1).ok()?;
                Some(chunks[0].payload.to_vec())
            }
            _ => None,
        }).unwrap();
        let start_info = decode_message(&outgoing).unwrap();
        assert_eq!(start_info.id, MessageId::Regular { system: false, number: 20 });
        let joined = client.receive(&server_game_message(
            NETMSGTYPE_SV_READYTOENTER, &[], 4, 4
        )).unwrap();
        assert!(joined.contains(&GameAction::EnterGameSent));
        assert_eq!(client.stage, Stage::EnterGameSent);
        assert_eq!(client.transport.sequence, 5);

        // A first snapshot is not decoded into game objects yet, but its
        // wire envelope is parsed and delivered to the game session.
        let mut snap = Vec::new();
        for value in [100, 100, 0, 0] {
            write_int(value, &mut snap);
        }
        let received = client.receive(&server_message(
            NETMSG_SNAPSINGLE, &snap, 5, 5
        )).unwrap();
        assert!(received.contains(&GameAction::SnapshotReceived {
            tick: 100, packed_bytes: 0,
        }));
    }

    #[test]
    fn bad_map_crc_does_not_mark_ready() {
        let map = small_real_datafile_v3();
        let mut client = GameSession06::new(String::new(), 1024 * 1024, 1024);
        feed_handshake(&mut client);
        client.receive(&server_message(
            NETMSG_MAP_CHANGE, &make_map_change(&map), 1, 1
        )).unwrap();
        let mut bad = make_map_data(&map);
        *bad.last_mut().unwrap() ^= 0x01;
        let error = client.receive(&server_message(
            NETMSG_MAP_DATA, &bad, 2, 2
        )).unwrap_err();
        assert_eq!(error, SessionError::Message(MessageError::MapCrcMismatch));
        assert!(client.loaded_map.is_none());
        assert!(client.loaded_map_bytes.is_none());
        assert_eq!(client.stage, Stage::Downloading);
    }

    #[test]
    fn rejects_out_of_order_ready_or_map_payload() {
        let mut client = GameSession06::new(String::new(), 1024, 1024);
        feed_handshake(&mut client);
        assert_eq!(
            client.receive(&server_message(NETMSG_CON_READY, &[], 1, 1)),
            Err(SessionError::UnexpectedStage),
        );
    }

    #[test]
    fn headless_decodes_vanilla_character_snapshots_and_deltas() {
        let map = small_real_datafile_v3();
        let mut session = GameSession06::new(String::new(), 4096, 1024);
        feed_handshake(&mut session);
        session.receive(&server_message(
            NETMSG_MAP_CHANGE, &make_map_change(&map), 1, 1
        )).unwrap();
        session.receive(&server_message(
            NETMSG_MAP_DATA, &make_map_data(&map), 2, 2
        )).unwrap();
        session.receive(&server_message(NETMSG_CON_READY, &[], 3, 3)).unwrap();
        assert_eq!(session.stage, Stage::WaitingReadyToEnter);
        session.receive(&server_game_message(
            NETMSGTYPE_SV_READYTOENTER, &[], 4, 4
        )).unwrap();
        assert_eq!(session.stage, Stage::EnterGameSent);

        let mut first_data = vec![0, 1, 0, 9, 1];
        let mut state_words = vec![0; 22];
        state_words[1] = 128;
        state_words[2] = 256;
        first_data.extend_from_slice(&state_words);
        let mut packed = Vec::new();
        for &value in &first_data {
            write_int(value, &mut packed);
        }
        let expected_crc = 384i32;
        let mut snapshot = Vec::new();
        for value in [100, 101, expected_crc, packed.len() as i32] {
            write_int(value, &mut snapshot);
        }
        snapshot.extend_from_slice(&packed);
        let events = session.receive(&server_message(
            NETMSG_SNAPSINGLE, &snapshot, 5, 5
        )).unwrap();
        assert!(events.contains(&GameAction::SnapshotDecoded {
            tick: 100, item_count: 1
        }));
        assert_eq!(session.snapshot_state(100).unwrap().find(9, 1).unwrap().words[1], 128);

        let mut delta = vec![0, 1, 0, 9, 1];
        let mut differences = vec![0; 22];
        differences[1] = 5;
        delta.extend_from_slice(&differences);
        let mut packed_next = Vec::new();
        for value in delta {
            write_int(value, &mut packed_next);
        }
        let mut next_snap = Vec::new();
        for value in [101, 1, expected_crc+5, packed_next.len() as i32] {
            write_int(value, &mut next_snap);
        }
        next_snap.extend_from_slice(&packed_next);
        let events = session.receive(&server_message(
            NETMSG_SNAPSINGLE, &next_snap, 6, 5
        )).unwrap();
        assert!(events.contains(&GameAction::SnapshotDecoded {
            tick: 101, item_count: 1
        }));
        assert_eq!(session.snapshot_state(101).unwrap().find(9, 1).unwrap().words[1], 133);
    }

    #[test]
    fn unexpected_ready_to_enter_before_startinfo_is_rejected() {
        let mut client = GameSession06::new(String::new(), 1024, 1024);
        feed_handshake(&mut client);
        assert_eq!(client.receive(&server_game_message(
            NETMSGTYPE_SV_READYTOENTER, &[], 1, 1
        )), Err(SessionError::UnexpectedStage));
        assert_eq!(client.stage, Stage::WaitingMap);
    }

    #[test]
    fn headless_game_input_is_unreliable_and_not_available_during_login() {
        let map = small_real_datafile_v3();
        let mut session = GameSession06::new(String::new(), 4096, 1024);
        let frame = crate::input::InputFrame {
            last_ack_snapshot: -1,
            intended_tick: 101,
            input: crate::input::PlayerInput::default(),
        };
        assert_eq!(session.send_input(frame), Err(SessionError::UnexpectedStage));
        feed_handshake(&mut session);
        session.receive(&server_message(NETMSG_MAP_CHANGE, &make_map_change(&map), 1, 1)).unwrap();
        session.receive(&server_message(NETMSG_MAP_DATA, &make_map_data(&map), 2, 2)).unwrap();
        session.receive(&server_message(NETMSG_CON_READY, &[], 3, 3)).unwrap();
        session.receive(&server_game_message(NETMSGTYPE_SV_READYTOENTER, &[], 4, 4)).unwrap();
        assert_eq!(session.stage, Stage::EnterGameSent);
        let sequence = session.transport.sequence;
        let message = session.send_input(frame).unwrap();
        assert_eq!(session.transport.sequence, sequence);
        let crate::network::Packet::Connected { header, payload } =
            crate::network::parse_packet_06(&message).unwrap()
        else { panic!("input isn't a connected UDP packet"); };
        assert_eq!(header.num_chunks, 1);
        let chunks = crate::network::parse_chunks_06(&payload[..payload.len()-4], 1).unwrap();
        assert_eq!(chunks[0].header.flags, 0); // no vital bit
        let decoded = decode_message(chunks[0].payload).unwrap();
        assert_eq!(decoded.id, MessageId::Regular { system: true, number: 16 });
        assert_eq!(crate::input::InputFrame::unpack_payload(decoded.payload), Ok(frame));
    }

    #[test]
    fn input_timing_only_accepts_recent_sent_ticks_and_deduplicates_feedback() {
        let mut session = GameSession06::new(String::new(), 4096, 1024);
        feed_handshake(&mut session);
        session.stage = Stage::EnterGameSent;
        for tick in 100..=102 {
            session.send_input(InputFrame {
                last_ack_snapshot: 99, intended_tick: tick,
                input: crate::input::PlayerInput::default(),
            }).unwrap();
        }

        let mut payload = Vec::new();
        write_int(500, &mut payload);
        write_int(i32::MIN, &mut payload);
        let forged = session.receive(&server_message(
            NETMSG_INPUTTIMING, &payload, 1, 1,
        )).unwrap();
        assert!(!forged.iter().any(|ev| matches!(ev, GameAction::InputTiming { .. })));
        assert!(session.last_input_timing.is_none());

        payload.clear();
        write_int(101, &mut payload);
        write_int(-40, &mut payload);
        let valid = session.receive(&server_message(
            NETMSG_INPUTTIMING, &payload, 2, 1,
        )).unwrap();
        assert!(valid.contains(&GameAction::InputTiming {
            tick: 101, time_left_ms: -40,
        }));
        assert_eq!(session.pending_input_ticks.iter().copied().collect::<Vec<_>>(), vec![102]);
        assert_eq!(session.last_input_timing.unwrap().intended_tick, 101);

        // A duplicate vital datagram and a late reply cannot reapply timing.
        let replay = session.receive(&server_message(
            NETMSG_INPUTTIMING, &payload, 3, 1,
        )).unwrap();
        assert!(!replay.iter().any(|ev| matches!(ev, GameAction::InputTiming { .. })));
        assert_eq!(session.last_input_timing.unwrap().time_left_ms, -40);

        payload.clear();
        write_int(102, &mut payload);
        write_int(25, &mut payload);
        let newer = session.receive(&server_message(
            NETMSG_INPUTTIMING, &payload, 4, 1,
        )).unwrap();
        assert!(newer.contains(&GameAction::InputTiming {
            tick: 102, time_left_ms: 25,
        }));

        assert!(session.pending_input_ticks.is_empty());
        assert!(session.restart_connection().is_ok());
        assert!(session.last_input_timing.is_none());
        assert!(session.pending_input_ticks.is_empty());
    }

    #[test]
    fn input_timing_rejects_malformed_wire_payload() {
        let mut session = GameSession06::new(String::new(), 4096, 1024);
        feed_handshake(&mut session);
        session.stage = Stage::EnterGameSent;
        let invalid = session.receive(&server_message(
            NETMSG_INPUTTIMING, &[0x80], 1, 1,
        ));
        assert!(matches!(invalid, Err(SessionError::Message(_))));
    }

    #[test]
    fn loss_and_crc_mismatch_recover_with_full_snapshots() {
        use crate::snapshot::AssembledSnapshot;
        fn snapshot(tick: i32, base: i32, x: i32, crc: u32) -> AssembledSnapshot {
            let mut words = vec![0, 1, 0, 9, 0];
            let mut character = vec![0; 22];
            character[1] = x;
            words.extend(character);
            let mut packed = Vec::new();
            for value in words { write_int(value, &mut packed); }
            AssembledSnapshot {
                game_tick: tick, delta_tick: base,
                crc: Some(crc), packed_bytes: packed,
            }
        }
        let mut session = GameSession06::new(String::new(), 4096, 1024);
        let mut actions = Vec::new();
        assert_eq!(session.snapshot_ack_tick(), -1);
        session.try_decode_snapshot(&snapshot(100, -1, 100, 100), &mut actions);
        assert_eq!(session.snapshot_ack_tick(), 100);
        assert_eq!(session.snapshot_state(100).unwrap().find(9, 0).unwrap().words[1], 100);
        actions.clear();

        session.try_decode_snapshot(&snapshot(101, 99, 5, 105), &mut actions);
        assert_eq!(actions, vec![GameAction::SnapshotRecoveryRequested {
            tick: 101, reason: SnapshotRecoveryReason::MissingBase,
        }]);
        assert_eq!(session.snapshot_ack_tick(), -1);
        assert!(session.is_snapshot_recovering());
        // Subsequent bad frames cannot spam recovery messages.
        actions.clear();
        session.try_decode_snapshot(&snapshot(102, 99, 5, 105), &mut actions);
        assert!(actions.is_empty());
        assert!(session.snapshot_state(100).is_some());

        session.try_decode_snapshot(&snapshot(103, -1, 200, 200), &mut actions);
        assert_eq!(session.snapshot_ack_tick(), 103);
        assert!(!session.is_snapshot_recovering());
        assert!(actions.iter().any(|a| matches!(a,
            GameAction::SnapshotDecoded { tick: 103, .. }
        )));
        actions.clear();
        session.try_decode_snapshot(&snapshot(104, -1, 900, 1), &mut actions);
        assert_eq!(actions, vec![GameAction::SnapshotRecoveryRequested {
            tick: 104, reason: SnapshotRecoveryReason::CrcMismatch,
        }]);
        assert_eq!(session.snapshot_ack_tick(), -1);
        assert!(session.snapshot_state(104).is_none());
        assert!(session.snapshot_state(103).is_some());

        actions.clear();
        session.try_decode_snapshot(&snapshot(105, -1, 250, 250), &mut actions);
        assert_eq!(session.snapshot_ack_tick(), 105);
        assert!(!session.is_snapshot_recovering());
        actions.clear();
        let mut malformed = snapshot(106, 105, 0, 0);
        malformed.packed_bytes = vec![0x80];
        session.try_decode_snapshot(&malformed, &mut actions);
        assert_eq!(actions, vec![GameAction::SnapshotRecoveryRequested {
            tick: 106, reason: SnapshotRecoveryReason::InvalidDelta,
        }]);
        assert_eq!(session.snapshot_ack_tick(), -1);
        actions.clear();
        session.try_decode_snapshot(&snapshot(107, -1, 300, 300), &mut actions);
        assert_eq!(session.snapshot_ack_tick(), 107);
        assert!(!session.is_snapshot_recovering());
    }

    #[test]
    fn game_chat_and_team_commands_use_vital_udp_chunks() {
        let mut client = GameSession06::new(String::new(), 1024, 1024);
        assert_eq!(client.send_chat(false, "hello"), Err(SessionError::UnexpectedStage));
        assert_eq!(client.request_kill(), Err(SessionError::UnexpectedStage));
        feed_handshake(&mut client);
        client.stage = Stage::EnterGameSent;
        for packet in [
            client.send_chat(true, "привет").unwrap(),
            client.set_team(-1).unwrap(),
            client.request_kill().unwrap(),
        ] {
            let crate::network::Packet::Connected { header, payload } =
                crate::network::parse_packet_06(&packet).unwrap()
            else { panic!("not connected packet"); };
            assert_eq!(header.num_chunks, 1);
            let chunks = crate::network::parse_chunks_06(
                &payload[..payload.len() - 4], 1,
            ).unwrap();
            assert_eq!(chunks[0].header.flags & CHUNK_VITAL, CHUNK_VITAL);
            assert!(!chunks[0].payload.is_empty());
        }
        assert!(client.transport.outstanding_count() >= 4);
    }

    #[test]
    fn server_chat_broadcast_and_motd_are_dispatched_to_game() {
        let mut session = GameSession06::new(String::new(), 1024, 1024);
        feed_handshake(&mut session);
        session.stage = Stage::EnterGameSent;
        let mut chat = Vec::new();
        write_int(0, &mut chat);
        write_int(3, &mut chat);
        write_string("Hi player", &mut chat).unwrap();
        let events = session.receive(&server_game_message(3, &chat, 1, 1)).unwrap();
        assert!(events.contains(&GameAction::ChatReceived {
            team: 0, client_id: 3, text: "Hi player".into(),
        }));
        let events = session.receive(&server_game_message(2, b"New record!\0", 2, 1)).unwrap();
        assert!(events.contains(&GameAction::ServerBroadcast("New record!".into())));
        let events = session.receive(&server_game_message(1, b"Welcome\0", 3, 1)).unwrap();
        assert!(events.contains(&GameAction::ServerMotd("Welcome".into())));
    }

    #[test]
    fn reconnect_resets_network_ack_map_and_snapshot_state() {
        let map = small_real_datafile_v3();
        let mut session = GameSession06::new("private".into(), 4096, 1024);
        session.start_info.name = "ReconnectTee".into();
        feed_handshake(&mut session);
        session.receive(&server_message(
            NETMSG_MAP_CHANGE, &make_map_change(&map), 1, 1,
        )).unwrap();
        assert_eq!(session.stage, Stage::Downloading);
        assert_eq!(session.map_name(), Some("mini"));
        assert!(session.transport.outstanding_count() > 0);
        assert_eq!(session.transport.receiver.ack, 1);
        assert_ne!(session.transport.token, crate::transport::SecurityToken::Unknown);

        let hello = session.restart_connection().unwrap();
        assert_eq!(hello, [vec![0x10, 0, 0, 1], b"TKEN".to_vec(), vec![0xff; 4]].concat());
        assert_eq!(session.stage, Stage::Connecting);
        assert_eq!(session.map_name(), None);
        assert!(session.pending_map.is_none());
        assert!(session.loaded_map.is_none());
        assert!(session.loaded_map_bytes.is_none());
        assert!(session.snapshot_states.is_empty());
        assert_eq!(session.snapshot_ack_tick(), -1);
        assert!(!session.is_snapshot_recovering());
        assert!(session.pending_input_ticks.is_empty());
        assert_eq!(session.last_input_timing, None);
        assert_eq!(session.snapshots.last_tick(), None);
        assert_eq!(session.transport.sequence, 0);
        assert_eq!(session.transport.peer_ack, 0);
        assert_eq!(session.transport.receiver.ack, 0);
        assert_eq!(session.transport.outstanding_count(), 0);
        assert_eq!(session.transport.token, crate::transport::SecurityToken::Unknown);
        assert_eq!(session.start_info.name, "ReconnectTee");
        assert_eq!(session.password, "private");
        assert_eq!(session.max_map_bytes, 4096);
        assert_eq!(session.max_tiles, 1024);

        let mut extra = b"TKEN".to_vec();
        extra.extend_from_slice(&TOKEN.to_be_bytes());
        let events = session.receive(&server_control(2, &extra)).unwrap();
        assert!(events.contains(&GameAction::TransportConnected));
        assert_eq!(session.stage, Stage::WaitingMap);
        assert_eq!(session.transport.sequence, 1);
    }

    #[test]
    fn actual_loopback_udp_transport_accepts_info_message() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let target = server.local_addr().unwrap();
        let join = thread::spawn(move || {
            let mut buffer = [0u8; 1400];
            let (_, peer) = server.recv_from(&mut buffer).unwrap();
            let mut extra = b"TKEN".to_vec();
            extra.extend_from_slice(&TOKEN.to_be_bytes());
            server.send_to(&server_control(2, &extra), peer).unwrap();

            let (n, _) = server.recv_from(&mut buffer).unwrap(); // ACCEPT
            assert_eq!(&buffer[..n], &[0x10, 0, 0, 3, 1, 2, 3, 4]);

            let (n, _) = server.recv_from(&mut buffer).unwrap(); // INFO vital
            let data = &buffer[..n];
            let crate::network::Packet::Connected { payload, header } =
                crate::network::parse_packet_06(data).unwrap()
            else { panic!("wrong packet kind"); };
            assert_eq!(header.num_chunks, 1);
            let chunks = crate::network::parse_chunks_06(
                &payload[..payload.len() - 4], 1
            ).unwrap();
            let decoded = decode_message(chunks[0].payload).unwrap();
            assert_eq!(decoded.id,
                MessageId::Regular { system: true, number: 1 });
        });
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(target).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut client = GameSession06::new(String::new(), 1024, 1024);
        socket.send(&client.connect().unwrap()).unwrap();
        let mut buffer = [0u8; 1400];
        let n = socket.recv(&mut buffer).unwrap();
        let actions = client.receive(&buffer[..n]).unwrap();
        for action in actions {
            if let GameAction::Outgoing(bytes) = action {
                socket.send(&bytes).unwrap();
            }
        }
        join.join().unwrap();
    }
}
