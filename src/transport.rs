//! DDNet/Teeworlds 0.6 connection-oriented UDP transport (initial subset).
//!
//! Ported from upstream/ddnet/src/engine/shared/network_conn.cpp and
//! network.cpp, pinned to DDNet 20.2.1. Implements the CONNECT/CONNECTACCEPT/
//! ACCEPT handshake, optional 0.6 security tokens, vital 10-bit sequences,
//! ACK validation, duplicate filtering and bounded resend requests.
//!
//! Missing: 0.7 tokens, a complete configurable timeout/reconnect policy and
//! full playable-client integration. A successful handshake is NOT a session.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use crate::network::{
    encode_packet_06, parse_chunks_06, parse_packet_06, ChunkHeader, NetworkError, Packet,
    PacketHeader, CHUNK_RESEND, CHUNK_VITAL, FLAG_COMPRESSION, FLAG_CONTROL, FLAG_RESEND,
    MAX_CHUNK_SIZE, MAX_PACKET_SIZE, MAX_SEQUENCE,
};
use crate::reliable::{is_in_backroom, Delivery, Receiver};
use crate::huffman::{Huffman, HuffmanError};

const TOKEN_MAGIC: &[u8; 4] = b"TKEN";
const CTRL_KEEPALIVE: u8 = 0;
const CTRL_CONNECT: u8 = 1;
const CTRL_CONNECTACCEPT: u8 = 2;
const CTRL_ACCEPT: u8 = 3;
const CTRL_CLOSE: u8 = 4;
const HALF_SEQUENCE_SPACE: usize = (MAX_SEQUENCE / 2) as usize;
// DDNet 20.2.1 network_conn.cpp retransmits the oldest vital chunk after 1s.
// Unlike the original configurable timeout, the prototype currently uses 15s.
const VITAL_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const VITAL_ACK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Offline,
    Connecting,
    Online,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityToken {
    Unknown,
    Unsupported,
    Negotiated(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Network(NetworkError),
    InvalidState,
    InvalidAcknowledgement,
    InvalidToken,
    Huffman(HuffmanError),
    InvalidControl,
    OversizedMessage,
    TooManyOutstanding,
    UnacknowledgedTimeout,
}
impl From<NetworkError> for TransportError {
    fn from(value: NetworkError) -> Self {
        Self::Network(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Send this on the connected UDP socket.
    Outgoing(Vec<u8>),
    /// Transport handshake finished, NOT the game protocol handshake.
    Connected,
    /// Received uncompressed game payload, with vital/duplicate checks.
    Message(Vec<u8>),
    /// The peer closed its connection.
    Disconnected(String),
}

#[derive(Debug, Clone)]
struct Unacked {
    sequence: u16,
    payload: Vec<u8>,
    first_sent: Instant,
    last_sent: Instant,
}

#[derive(Debug, Clone)]
pub struct Client06 {
    pub state: State,
    pub token: SecurityToken,
    pub receiver: Receiver,
    pub sequence: u16,
    pub peer_ack: u16,
    outstanding: VecDeque<Unacked>,
    huffman: Huffman,
}

impl Default for Client06 {
    fn default() -> Self {
        Self {
            state: State::Offline,
            token: SecurityToken::Unknown,
            receiver: Receiver::default(),
            sequence: 0,
            peer_ack: 0,
            outstanding: VecDeque::new(),
            huffman: Huffman::new(),
        }
    }
}

impl Client06 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the original DDNet 0.6 connection handshake.
    ///
    /// Initial message: 10 00 00 01 'T' 'K' 'E' 'N' ff ff ff ff
    pub fn begin_connect(&mut self) -> Result<Vec<u8>, TransportError> {
        if self.state != State::Offline && self.state != State::Connecting {
            return Err(TransportError::InvalidState);
        }
        self.state = State::Connecting;
        self.control_packet(CTRL_CONNECT, TOKEN_MAGIC)
    }

    pub fn outstanding_count(&self) -> usize {
        self.outstanding.len()
    }

    fn append_security_token(&self, data: &mut Vec<u8>) {
        match self.token {
            SecurityToken::Unknown => data.extend_from_slice(&u32::MAX.to_be_bytes()),
            SecurityToken::Unsupported => {}
            SecurityToken::Negotiated(value) => data.extend_from_slice(&value.to_be_bytes()),
        }
    }

    fn packet(&self, header: PacketHeader, payload: &[u8]) -> Result<Vec<u8>, TransportError> {
        let mut data = Vec::with_capacity(payload.len() + 4);
        data.extend_from_slice(payload);
        self.append_security_token(&mut data);
        Ok(encode_packet_06(header, &data)?)
    }

    fn control_packet(&self, kind: u8, extra: &[u8]) -> Result<Vec<u8>, TransportError> {
        let mut content = Vec::with_capacity(1 + extra.len());
        content.push(kind);
        content.extend_from_slice(extra);
        self.packet(
            PacketHeader { flags: FLAG_CONTROL, ack: self.receiver.ack, num_chunks: 0 },
            &content,
        )
    }

    fn chunk_packet(
        &self, flags: u8, sequence: Option<u16>, payload: &[u8],
    ) -> Result<Vec<u8>, TransportError> {
        if payload.len() > MAX_CHUNK_SIZE {
            return Err(TransportError::OversizedMessage);
        }
        let header = ChunkHeader { flags, size: payload.len() as u16, sequence };
        let mut chunk = header.encode_06()?;
        chunk.extend_from_slice(payload);
        self.packet(
            PacketHeader { flags: 0, ack: self.receiver.ack, num_chunks: 1 },
            &chunk,
        )
    }

    /// Send one uncompressed chunk. Vital chunks are queued for retransmission.
    pub fn send_message(
        &mut self, payload: &[u8], vital: bool,
    ) -> Result<Vec<u8>, TransportError> {
        if self.state != State::Online {
            return Err(TransportError::InvalidState);
        }
        if payload.len() > MAX_CHUNK_SIZE {
            return Err(TransportError::OversizedMessage);
        }
        if vital && self.outstanding.len() >= HALF_SEQUENCE_SPACE {
            return Err(TransportError::TooManyOutstanding);
        }
        let next = (self.sequence + 1) % MAX_SEQUENCE;
        let sequence = vital.then_some(next);
        let flags = if vital { CHUNK_VITAL } else { 0 };
        let wire = self.chunk_packet(flags, sequence, payload)?;
        if vital {
            self.sequence = next;
            let now = Instant::now();
            self.outstanding.push_back(Unacked {
                sequence: next,
                payload: payload.to_vec(),
                first_sent: now,
                last_sent: now,
            });
        }
        Ok(wire)
    }

    pub fn keepalive(&self) -> Result<Vec<u8>, TransportError> {
        if self.state != State::Online {
            return Err(TransportError::InvalidState);
        }
        self.control_packet(CTRL_KEEPALIVE, &[])
    }

    pub fn disconnect(&mut self) -> Result<Vec<u8>, TransportError> {
        if self.state != State::Online {
            return Err(TransportError::InvalidState);
        }
        let wire = self.control_packet(CTRL_CLOSE, &[])?;
        self.state = State::Closed;
        self.outstanding.clear();
        Ok(wire)
    }

    fn request_resend(&self) -> Result<Vec<u8>, TransportError> {
        self.packet(
            PacketHeader {
                flags: FLAG_RESEND,
                ack: self.receiver.ack,
                num_chunks: 0,
            },
            &[],
        )
    }

    fn retransmit(&mut self) -> Result<Vec<Event>, TransportError> {
        // Peer explicitly requested resends. Do not change vital sequence IDs
        // or enqueue a second copy of any unacknowledged chunk.
        let mut output = Vec::with_capacity(self.outstanding.len());
        for chunk in &self.outstanding {
            output.push(Event::Outgoing(self.chunk_packet(
                CHUNK_VITAL | CHUNK_RESEND,
                Some(chunk.sequence),
                &chunk.payload,
            )?));
        }
        let now = Instant::now();
        for chunk in &mut self.outstanding {
            chunk.last_sent = now;
        }
        Ok(output)
    }

    /// Drive retransmission when no peer RESEND request arrives. Call this
    /// regularly from an online session's main loop. The oldest vital chunk
    /// is retried at most once per second, matching DDNet's baseline timing.
    ///
    /// `now` is supplied by the caller so timeout tests are deterministic.
    /// A bounded 15-second unacknowledged queue closes the prototype session.
    pub fn poll_retransmission(
        &mut self, now: Instant,
    ) -> Result<Option<Vec<u8>>, TransportError> {
        if self.state != State::Online {
            return Err(TransportError::InvalidState);
        }
        let Some(oldest) = self.outstanding.front() else {
            return Ok(None);
        };
        if now.saturating_duration_since(oldest.first_sent) >= VITAL_ACK_TIMEOUT {
            self.state = State::Closed;
            self.outstanding.clear();
            return Err(TransportError::UnacknowledgedTimeout);
        }
        if now.saturating_duration_since(oldest.last_sent) < VITAL_RETRY_INTERVAL {
            return Ok(None);
        }
        let packet = self.chunk_packet(
            CHUNK_VITAL | CHUNK_RESEND,
            Some(oldest.sequence),
            &oldest.payload,
        )?;
        if let Some(chunk) = self.outstanding.front_mut() {
            chunk.last_sent = now;
        }
        Ok(Some(packet))
    }

    fn valid_ack(&self, ack: u16) -> bool {
        if self.sequence >= self.peer_ack {
            ack >= self.peer_ack && ack <= self.sequence
        } else {
            ack >= self.peer_ack || ack <= self.sequence
        }
    }

    fn acknowledge(&mut self, ack: u16) {
        self.peer_ack = ack;
        while self.outstanding.front().is_some_and(|chunk| {
            is_in_backroom(chunk.sequence, ack)
        }) {
            self.outstanding.pop_front();
        }
    }

    fn strip_security<'a>(&self, payload: &'a [u8]) -> Result<&'a [u8], TransportError> {
        match self.token {
            SecurityToken::Unknown => Err(TransportError::InvalidToken),
            SecurityToken::Unsupported => Ok(payload),
            SecurityToken::Negotiated(token) => {
                let Some(end) = payload.len().checked_sub(4) else {
                    return Err(TransportError::InvalidToken);
                };
                if payload[end..] != token.to_be_bytes() {
                    return Err(TransportError::InvalidToken);
                }
                Ok(&payload[..end])
            }
        }
    }

    /// Process one UDP datagram received from the already verified peer.
    /// Reject invalid tokens and ACKs BEFORE mutating any sequence counters.
    pub fn receive(&mut self, bytes: &[u8]) -> Result<Vec<Event>, TransportError> {
        if self.state != State::Connecting && self.state != State::Online {
            return Err(TransportError::InvalidState);
        }
        let packet = parse_packet_06(bytes)?;
        let Packet::Connected { header, payload } = packet else {
            return Err(TransportError::InvalidControl);
        };

        if !self.valid_ack(header.ack) {
            return Err(TransportError::InvalidAcknowledgement);
        }

        if self.state == State::Connecting {
            if header.flags & FLAG_CONTROL == 0 || payload.first() != Some(&CTRL_CONNECTACCEPT) {
                return Ok(Vec::new());
            }
            self.token = if payload.get(1..5) == Some(TOKEN_MAGIC.as_slice())
                && payload.len() >= 9
            {
                let bytes: [u8; 4] = payload[5..9].try_into()
                    .map_err(|_| TransportError::InvalidToken)?;
                SecurityToken::Negotiated(u32::from_be_bytes(bytes))
            } else {
                SecurityToken::Unsupported
            };
            self.state = State::Online;
            return Ok(vec![
                Event::Outgoing(self.control_packet(CTRL_ACCEPT, &[])?),
                Event::Connected,
            ]);
        }

        // In DDNet, Huffman compression covers the entire packet body,
        // INCLUDING the trailing 0.6 security token. Decompress first.
        let decompressed = if header.flags & FLAG_COMPRESSION != 0 {
            Some(self.huffman.decompress(payload, MAX_PACKET_SIZE)
                .map_err(TransportError::Huffman)?)
        } else {
            None
        };
        let uncompressed = decompressed.as_deref().unwrap_or(payload);
        let payload = self.strip_security(uncompressed)?;
        if header.flags & FLAG_CONTROL != 0 {
            let Some(&kind) = payload.first() else {
                return Err(TransportError::InvalidControl);
            };
            self.acknowledge(header.ack);
            if kind == CTRL_CLOSE {
                let reason_bytes = payload[1..]
                    .split(|&b| b == 0)
                    .next()
                    .unwrap_or(&[]);
                let reason = String::from_utf8_lossy(reason_bytes).into_owned();
                self.state = State::Closed;
                self.outstanding.clear();
                return Ok(vec![Event::Disconnected(reason)]);
            }
            return Ok(Vec::new());
        }

        // Verify all chunks before changing state, to avoid accepting
        // a half-parsed malformed packet.
        let chunks = parse_chunks_06(payload, header.num_chunks)?;
        self.acknowledge(header.ack);
        let mut events = Vec::new();
        if header.flags & FLAG_RESEND != 0 {
            events.extend(self.retransmit()?);
        }
        for chunk in chunks {
            if let Some(sequence) = chunk.header.sequence {
                match self.receiver.receive_vital(sequence) {
                    Delivery::Accepted => events.push(Event::Message(chunk.payload.to_vec())),
                    Delivery::Duplicate => {}
                    Delivery::ResendRequested => {
                        events.push(Event::Outgoing(self.request_resend()?));
                    }
                    Delivery::InvalidSequence => return Err(TransportError::InvalidControl),
                }
            } else {
                events.push(Event::Message(chunk.payload.to_vec()));
            }
        }
        Ok(events)
    }
}

/// Probe a single DDNet 0.6 UDP server's transport handshake.
/// Does not send gameplay data or read maps. Uses a connected UDP socket
/// so packets from unrelated remote addresses cannot spoof a response.
pub fn probe_server<A: ToSocketAddrs>(
    destination: A, attempts: usize,
) -> io::Result<SocketAddr> {
    let mut addrs = destination.to_socket_addrs()?;
    let peer = addrs.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "no resolved server addresses")
    })?;
    let socket = UdpSocket::bind(if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })?;
    socket.connect(peer)?;
    socket.set_read_timeout(Some(Duration::from_millis(400)))?;
    let mut session = Client06::new();
    let hello = session.begin_connect().map_err(|e| io::Error::other(format!("{e:?}")))?;
    let mut incoming = [0u8; MAX_PACKET_SIZE];
    for _ in 0..attempts {
        socket.send(&hello)?;
        match socket.recv(&mut incoming) {
            Ok(size) => {
                let events = match session.receive(&incoming[..size]) {
                    Ok(events) => events,
                    Err(_) => continue,
                };
                let mut connected = false;
                for event in events {
                    match event {
                        Event::Outgoing(data) => { socket.send(&data)?; }
                        Event::Connected => connected = true,
                        Event::Disconnected(_) | Event::Message(_) => {}
                    }
                }
                if connected {
                    return Ok(peer);
                }
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no accepted DDNet 0.6 handshake"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn packet(flags: u8, ack: u16, num_chunks: u8, payload: &[u8]) -> Vec<u8> {
        encode_packet_06(PacketHeader { flags, ack, num_chunks }, payload).unwrap()
    }

    fn connected(token: Option<u32>) -> Client06 {
        let mut client = Client06::new();
        assert_eq!(client.begin_connect().unwrap(),
            [vec![0x10, 0, 0, 1], b"TKEN".to_vec(), vec![0xff; 4]].concat());
        let mut reply = vec![CTRL_CONNECTACCEPT];
        if let Some(value) = token {
            reply.extend_from_slice(TOKEN_MAGIC);
            reply.extend_from_slice(&value.to_be_bytes());
        }
        let events = client.receive(&packet(FLAG_CONTROL, 0, 0, &reply)).unwrap();
        assert!(events.iter().any(|ev| matches!(ev, Event::Connected)));
        assert_eq!(client.state, State::Online);
        client
    }

    #[test]
    fn token_negotiation_and_accept_bytes() {
        let mut client = Client06::new();
        assert!(matches!(client.send_message(b"test", true), Err(TransportError::InvalidState)));
        assert_eq!(client.begin_connect().unwrap(),
            [vec![0x10, 0, 0, 1], b"TKEN".to_vec(), vec![0xff; 4]].concat());
        let mut accept = vec![2];
        accept.extend_from_slice(TOKEN_MAGIC);
        accept.extend_from_slice(&0x1234_5678u32.to_be_bytes());
        let result = client.receive(&packet(FLAG_CONTROL, 0, 0, &accept)).unwrap();
        assert_eq!(result, vec![
            Event::Outgoing(vec![0x10, 0, 0, 3, 0x12, 0x34, 0x56, 0x78]),
            Event::Connected,
        ]);
        assert_eq!(client.token, SecurityToken::Negotiated(0x1234_5678));
        assert_eq!(client.keepalive().unwrap(),
            vec![0x10, 0, 0, 0, 0x12, 0x34, 0x56, 0x78]);
        assert!(client.begin_connect().is_err());
    }

    #[test]
    fn backwards_compatible_server_without_token() {
        let mut c = connected(None);
        assert_eq!(c.token, SecurityToken::Unsupported);
        assert_eq!(c.keepalive().unwrap(), [0x10, 0, 0, 0]);
        assert_eq!(c.receive(&packet(FLAG_CONTROL, 0, 0, &[0])), Ok(vec![]));
    }

    #[test]
    fn rejection_does_not_mutate_state() {
        let mut c = connected(Some(0x0102_0304));
        let message = c.send_message(b"ping", true).unwrap();
        assert_eq!(c.outstanding_count(), 1);
        assert!(matches!(
            c.receive(&packet(FLAG_CONTROL, 1, 0, &[0, 0, 0, 0, 0])),
            Err(TransportError::InvalidToken),
        ));
        assert_eq!(c.peer_ack, 0);
        assert_eq!(c.outstanding_count(), 1);
        assert_eq!(c.sequence, 1);
        assert!(!message.is_empty());
        // Valid token but out-of-window acknowledgment.
        assert_eq!(
            c.receive(&packet(FLAG_CONTROL, 2, 0, &[0, 1, 2, 3, 4])),
            Err(TransportError::InvalidAcknowledgement),
        );
    }

    #[test]
    fn vital_ack_and_resend_after_loss_simulation() {
        let token = 0x3322_1100u32;
        let mut c = connected(Some(token));
        let sent = c.send_message(b"input", true).unwrap();
        assert_eq!(c.outstanding_count(), 1);
        assert_eq!(sent[2], 1);
        assert_eq!(c.sequence, 1);
        // Peer ACKs our first vital and sends its own vital sequence #1.
        let mut chunk = ChunkHeader {
            flags: CHUNK_VITAL, size: 3, sequence: Some(1)
        }.encode_06().unwrap();
        chunk.extend_from_slice(b"map");
        chunk.extend_from_slice(&token.to_be_bytes());
        assert_eq!(
            c.receive(&packet(0, 1, 1, &chunk)).unwrap(),
            vec![Event::Message(b"map".to_vec())],
        );
        assert_eq!(c.outstanding_count(), 0);
        // Duplicate sequence #1 is dropped.
        assert_eq!(c.receive(&packet(0, 1, 1, &chunk)).unwrap(), vec![]);
        // Gap #3 triggers resend (ack field is now 1).
        let mut missing = ChunkHeader {
            flags: CHUNK_VITAL, size: 1, sequence: Some(3)
        }.encode_06().unwrap();
        missing.extend_from_slice(b"x");
        missing.extend_from_slice(&token.to_be_bytes());
        let events = c.receive(&packet(0, 1, 1, &missing)).unwrap();
        assert_eq!(events, vec![Event::Outgoing(packet(
            FLAG_RESEND, 1, 0, &token.to_be_bytes()
        ))]);
        // A resend-request from the peer causes our unacked packet to be retransmitted.
        c.send_message(b"test", true).unwrap();
        let events = c.receive(&packet(FLAG_RESEND, 1, 0, &token.to_be_bytes())).unwrap();
        assert_eq!(events.len(), 1);
        let Event::Outgoing(ref retransmit) = events[0] else {
            panic!("expected retransmission");
        };
        match parse_packet_06(retransmit).unwrap() {
            Packet::Connected { payload, .. } => {
                let chunks = parse_chunks_06(&payload[..payload.len()-4], 1).unwrap();
                assert_eq!(chunks[0].header.flags, CHUNK_VITAL | CHUNK_RESEND);
                assert_eq!(chunks[0].payload, b"test");
            }
            _ => panic!("unexpected connectionless response"),
        }
    }

    #[test]
    fn clock_driven_retransmission_recovers_when_no_resend_request_arrives() {
        let token = 0x0102_0304u32;
        let mut client = connected(Some(token));
        let first = client.send_message(b"lost vital", true).unwrap();
        assert_eq!(client.outstanding_count(), 1);
        let initial = client.outstanding.front().unwrap().last_sent;
        assert_eq!(
            client.poll_retransmission(initial + Duration::from_millis(999)),
            Ok(None),
        );
        let resent_at = initial + Duration::from_secs(1);
        let retry = client.poll_retransmission(resent_at).unwrap().unwrap();
        assert_ne!(retry, first); // RETRANSMISSION has CHUNK_RESEND.
        let Packet::Connected { header, payload } = parse_packet_06(&retry).unwrap() else {
            panic!("expected connected packet");
        };
        assert_eq!(header.ack, 0);
        let chunks = parse_chunks_06(&payload[..payload.len() - 4], 1).unwrap();
        assert_eq!(chunks[0].header.flags, CHUNK_VITAL | CHUNK_RESEND);
        assert_eq!(chunks[0].header.sequence, Some(1));
        assert_eq!(chunks[0].payload, b"lost vital");
        assert_eq!(client.outstanding_count(), 1); // never double-queue
        assert_eq!(
            client.poll_retransmission(resent_at + Duration::from_millis(800)),
            Ok(None),
        );

        // ACK arrives after retransmission, so no further sends are due.
        let ack = packet(FLAG_CONTROL, 1, 0, &[CTRL_KEEPALIVE, 1, 2, 3, 4]);
        assert_eq!(client.receive(&ack), Ok(Vec::new()));
        assert_eq!(client.outstanding_count(), 0);
        assert_eq!(
            client.poll_retransmission(resent_at + Duration::from_secs(2)),
            Ok(None),
        );
    }

    #[test]
    fn clock_driven_ack_timeout_closes_unrecoverable_connection() {
        let mut client = connected(Some(0x0102_0304));
        assert_eq!(client.poll_retransmission(Instant::now()), Ok(None));
        client.send_message(b"never acknowledged", true).unwrap();
        let oldest = client.outstanding.front().unwrap().first_sent;
        assert_eq!(
            client.poll_retransmission(oldest + VITAL_ACK_TIMEOUT),
            Err(TransportError::UnacknowledgedTimeout),
        );
        assert_eq!(client.state, State::Closed);
        assert_eq!(client.outstanding_count(), 0);
        assert_eq!(
            client.poll_retransmission(oldest + VITAL_ACK_TIMEOUT + Duration::from_secs(1)),
            Err(TransportError::InvalidState),
        );
    }

    #[test]
    fn timer_retries_oldest_only_across_sequence_wraparound() {
        let mut client = connected(None);
        client.sequence = MAX_SEQUENCE - 2;
        client.peer_ack = MAX_SEQUENCE - 2;
        client.send_message(b"first", true).unwrap();
        client.send_message(b"second", true).unwrap();
        assert_eq!(client.sequence, 0);
        let first_at = client.outstanding.front().unwrap().first_sent;
        let retry = client.poll_retransmission(
            first_at + VITAL_RETRY_INTERVAL,
        ).unwrap().unwrap();
        let Packet::Connected { payload, .. } = parse_packet_06(&retry).unwrap() else {
            panic!("expected connected retry");
        };
        let chunk = parse_chunks_06(payload, 1).unwrap();
        assert_eq!(chunk[0].header.sequence, Some(MAX_SEQUENCE - 1));
        assert_eq!(chunk[0].payload, b"first");

        // Wrapped ACK 0 confirms both original vital packets.
        let keepalive = packet(FLAG_CONTROL, 0, 0, &[CTRL_KEEPALIVE]);
        client.receive(&keepalive).unwrap();
        assert_eq!(client.outstanding_count(), 0);
    }

    #[test]
    fn rejects_wrong_security_token_even_in_compressed_packet() {
        let mut c = connected(Some(0x01020304));
        assert_eq!(
            c.receive(&packet(0, 0, 1, &[0, 0])),
            Err(TransportError::InvalidToken),
        );
        let compressed_wrong_token = c.huffman.compress(
            &[0u8, 0, 0, 2, 3, 4], MAX_PACKET_SIZE
        ).unwrap();
        assert_eq!(
            c.receive(&packet(FLAG_COMPRESSION, 0, 1, &compressed_wrong_token)),
            Err(TransportError::InvalidToken),
        );
        assert_eq!(c.state, State::Online);
        assert_eq!(c.disconnect().unwrap(),
            vec![0x10, 0, 0, CTRL_CLOSE, 1, 2, 3, 4]);
        assert_eq!(c.state, State::Closed);
    }

    #[test]
    fn loopback_udp_handshake_with_mock_ddnet06_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let address = server.local_addr().unwrap();
        let join = thread::spawn(move || {
            let mut buffer = [0u8; MAX_PACKET_SIZE];
            let (len, from) = server.recv_from(&mut buffer).unwrap();
            assert_eq!(&buffer[..len],
                &[vec![0x10, 0, 0, 1], b"TKEN".to_vec(), vec![0xff; 4]].concat());
            let token = 0x1020_3040u32;
            let response = packet(FLAG_CONTROL, 0, 0, &[
                &[CTRL_CONNECTACCEPT][..], TOKEN_MAGIC, &token.to_be_bytes()
            ].concat());
            server.send_to(&response, from).unwrap();
            let (len, next) = server.recv_from(&mut buffer).unwrap();
            assert_eq!(from, next);
            assert_eq!(&buffer[..len],
                &[0x10, 0, 0, CTRL_ACCEPT, 0x10, 0x20, 0x30, 0x40]);
        });
        assert_eq!(probe_server(address, 4).unwrap(), address);
        join.join().unwrap();
    }

    #[test]
    fn huffman_compressed_data_includes_security_token() {
        let token = 0x0102_0304u32;
        let mut client = connected(Some(token));
        let mut payload = ChunkHeader {
            flags: 0, size: 5, sequence: None,
        }.encode_06().unwrap();
        payload.extend_from_slice(b"hello");
        payload.extend_from_slice(&token.to_be_bytes());
        let compressed = client.huffman.compress(&payload, MAX_PACKET_SIZE).unwrap();
        let received = packet(FLAG_COMPRESSION, 0, 1, &compressed);
        assert_eq!(client.receive(&received).unwrap(),
            vec![Event::Message(b"hello".to_vec())]);
    }

    #[test]
    fn malformed_datagram_fuzz_does_not_panic() {
        let mut client = connected(Some(0x1213_1415));
        let mut random = 0x1234_5678u32;
        for len in 0..250usize {
            let mut packet = vec![0u8; len];
            for byte in &mut packet {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = (random >> 24) as u8;
            }
            let _ = client.receive(&packet);
            if client.state == State::Closed {
                break;
            }
        }
    }
}
