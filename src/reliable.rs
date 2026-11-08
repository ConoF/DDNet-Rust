//! DDNet 0.6 reliable-chunk sequence and acknowledgment state.
//! Port of CNetBase::IsSeqInBackroom and CPacketChunkUnpacker from
//! upstream/ddnet/src/engine/shared/network.cpp (tag 20.2.1).
//! This is an offline model, not a complete UDP transport.

use crate::network::MAX_SEQUENCE;
use std::collections::VecDeque;
const HALF: u16 = MAX_SEQUENCE / 2;

/// DDNet's backwards 512-sequence ACK window, including wraparound.
pub fn is_in_backroom(seq: u16, ack: u16) -> bool {
    if seq >= MAX_SEQUENCE || ack >= MAX_SEQUENCE { return false; }
    let bottom = i32::from(ack) - i32::from(HALF);
    if bottom < 0 {
        seq <= ack || i32::from(seq) >= bottom + i32::from(MAX_SEQUENCE)
    } else {
        seq <= ack && i32::from(seq) >= bottom
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery { Accepted, Duplicate, ResendRequested, InvalidSequence }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Receiver {
    pub ack: u16,
    pub unknown_sequence: bool,
}

impl Receiver {
    pub fn receive_vital(&mut self, sequence: u16) -> Delivery {
        if sequence >= MAX_SEQUENCE { return Delivery::InvalidSequence; }
        if sequence == (self.ack + 1) % MAX_SEQUENCE || self.unknown_sequence {
            self.ack = sequence;
            self.unknown_sequence = false;
            Delivery::Accepted
        } else if is_in_backroom(sequence, self.ack) {
            Delivery::Duplicate
        } else {
            Delivery::ResendRequested
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError { InvalidSequence, Full, OutOfOrder }

#[derive(Debug, Default)]
pub struct PendingAcks { sequences: VecDeque<u16> }

impl PendingAcks {
    pub fn len(&self) -> usize { self.sequences.len() }
    pub fn is_empty(&self) -> bool { self.sequences.is_empty() }
    pub fn queue(&mut self, sequence: u16) -> Result<(), QueueError> {
        if sequence >= MAX_SEQUENCE { return Err(QueueError::InvalidSequence); }
        if self.len() >= usize::from(HALF) { return Err(QueueError::Full); }
        if self.sequences.back().is_some_and(|&last| (last + 1) % MAX_SEQUENCE != sequence) {
            return Err(QueueError::OutOfOrder);
        }
        self.sequences.push_back(sequence);
        Ok(())
    }
    /// Remove acknowledged vital chunks from the head only.
    pub fn acknowledge(&mut self, ack: u16) -> usize {
        if ack >= MAX_SEQUENCE { return 0; }
        let mut removed = 0;
        while self.sequences.front().is_some_and(|&seq| is_in_backroom(seq, ack)) {
            self.sequences.pop_front();
            removed += 1;
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraparound_and_half_window() {
        assert!(is_in_backroom(1023, 0));
        assert!(!is_in_backroom(1, 0));
        assert!(is_in_backroom(512, 0));
        assert!(!is_in_backroom(511, 0));
        assert!(!is_in_backroom(1024, 0));
        assert!(is_in_backroom(60, 100));
    }

    #[test]
    fn packet_loss_reordering_and_duplicates() {
        let mut recv = Receiver::default();
        assert_eq!(recv.receive_vital(1), Delivery::Accepted);
        assert_eq!(recv.receive_vital(3), Delivery::ResendRequested);
        assert_eq!(recv.receive_vital(1), Delivery::Duplicate);
        assert_eq!(recv.receive_vital(2), Delivery::Accepted);
        assert_eq!(recv.receive_vital(3), Delivery::Accepted);
        assert_eq!(recv.receive_vital(1024), Delivery::InvalidSequence);
    }

    #[test]
    fn wraparound_and_resync() {
        let mut recv = Receiver { ack: 1022, unknown_sequence: false };
        assert_eq!(recv.receive_vital(1023), Delivery::Accepted);
        assert_eq!(recv.receive_vital(0), Delivery::Accepted);
        assert_eq!(recv.receive_vital(1023), Delivery::Duplicate);
        recv.unknown_sequence = true;
        assert_eq!(recv.receive_vital(800), Delivery::Accepted);
        assert_eq!(recv.receive_vital(801), Delivery::Accepted);
    }

    #[test]
    fn pending_queue_acknowledges_only_oldest() {
        let mut queue = PendingAcks::default();
        for seq in 1..=5 { queue.queue(seq).unwrap(); }
        assert_eq!(queue.queue(7), Err(QueueError::OutOfOrder));
        assert_eq!(queue.acknowledge(3), 3);
        assert_eq!(queue.acknowledge(2), 0);
        assert_eq!(queue.acknowledge(5), 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn looped_loss_simulation_across_wraparound() {
        let mut recv = Receiver::default();
        let mut send = PendingAcks::default();
        for number in 1..=3000u16 {
            let seq = number % MAX_SEQUENCE;
            send.queue(seq).unwrap();
            if number % 3 == 0 {
                assert_eq!(
                    recv.receive_vital((seq + 1) % MAX_SEQUENCE),
                    Delivery::ResendRequested
                );
            }
            assert_eq!(recv.receive_vital(seq), Delivery::Accepted);
            assert_eq!(recv.receive_vital(seq), Delivery::Duplicate);
            assert_eq!(send.acknowledge(recv.ack), 1);
        }
        assert!(send.is_empty());
    }
}
