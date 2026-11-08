//! Bounded, display-only local movement prediction against verified snapshots.
//!
//! The server owns gameplay state. This module replays locally submitted input
//! starting from the last authoritative 0.6 Character netobject. It deliberately
//! does not predict hooks, other players, switches, teleports or tune zones.
//! These cases fall back to the original server snapshot, not guessed physics.

use std::collections::VecDeque;

use crate::input::{InputFrame, PlayerInput};
use crate::map::GameMap;
use crate::netobjects::{CharacterCoreNet, PlayerInfoNet, CHARACTER_TYPE};
use crate::physics::Vec2;
use crate::simulator::{SimInput, TeeSimulator};
use crate::snapshot_delta::SnapshotState;

pub const MAX_INPUT_HISTORY: usize = 256;
pub const MAX_REPLAY_TICKS: i32 = 64;
const NEARBY_PLAYER_RADIUS: f32 = 64.0;
const MAX_PREDICTION_OFFSET: f32 = 512.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimedInput {
    tick: i32,
    input: PlayerInput,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PredictedLocal {
    pub client_id: u16,
    pub tick: i32,
    pub position: Vec2,
    pub velocity: Vec2,
    pub replayed_ticks: i32,
}

/// This history is independent of the UDP ACK. Only *successfully sent* inputs
/// are recorded; a snapshot for a later tick is always the reconciliation base.
#[derive(Debug, Clone, Default)]
pub struct LocalPredictor {
    inputs: VecDeque<TimedInput>,
}

pub fn local_character_id(snapshot: &SnapshotState) -> Option<u16> {
    let info = snapshot.items.iter().filter_map(PlayerInfoNet::from_object)
        .find(|player| player.local)?;
    let id = u16::try_from(info.client_id).ok()?;
    // Require a true character, not another object that happens to share ID.
    snapshot.find(CHARACTER_TYPE, id)?;
    Some(id)
}

impl LocalPredictor {
    pub fn new() -> Self { Self::default() }

    pub fn reset(&mut self) { self.inputs.clear(); }

    pub fn recorded_count(&self) -> usize { self.inputs.len() }

    pub fn record_sent_input(&mut self, frame: InputFrame) {
        if frame.validate().is_err() {
            return;
        }
        if let Some(last) = self.inputs.back() {
            if frame.intended_tick <= last.tick {
                // Reconnect, rewound clocks or duplicate ticks must never
                // mix different input epochs in a prediction.
                self.reset();
            }
        }
        self.inputs.push_back(TimedInput {
            tick: frame.intended_tick, input: frame.input,
        });
        if self.inputs.len() > MAX_INPUT_HISTORY {
            self.inputs.pop_front();
        }
    }

    /// Returns None if replay would rely on unsupported mechanics, missing
    /// input history, implausible network data or more than 64 prediction ticks.
    /// It never modifies the verified network snapshot.
    pub fn predict(
        &self,
        authoritative_tick: i32,
        authoritative: &SnapshotState,
        map: &GameMap,
    ) -> Option<PredictedLocal> {
        let id = local_character_id(authoritative)?;
        let object = authoritative.find(CHARACTER_TYPE, id)?;
        let core = CharacterCoreNet::from_object(object)?;
        let last_tick = self.inputs.back()?.tick;
        let steps = last_tick.checked_sub(authoritative_tick)?;
        if !(1..=MAX_REPLAY_TICKS).contains(&steps) { return None; }

        // Hook and player interactions cannot be reproduced by the isolated
        // simulator; rendering their movement from unverified predictions
        // would be misleading.
        if !matches!(core.hook_state, -1 | 0) || !(0..=3).contains(&core.jumped) {
            return None;
        }
        let server_pos = Vec2::new(core.x as f32, core.y as f32);
        if authoritative.items.iter().any(|other| {
            if other.type_id != CHARACTER_TYPE || other.id == id { return false; }
            CharacterCoreNet::from_object(other).is_some_and(|player| {
                (player.x as f32 - server_pos.x).abs() <= NEARBY_PLAYER_RADIUS
                && (player.y as f32 - server_pos.y).abs() <= NEARBY_PLAYER_RADIUS
            })
        }) {
            return None;
        }

        let mut sim = TeeSimulator::new(server_pos);
        sim.tick = u64::try_from(authoritative_tick).ok()?;
        sim.character.velocity = Vec2::new(
            core.velocity_x as f32 / 256.0,
            core.velocity_y as f32 / 256.0,
        );
        sim.character.jumped = core.jumped as u8;
        sim.character.direction = core.direction.clamp(-1, 1);

        // Entries can be ahead of or behind the snapshot tick. Carry forward
        // the most recent submitted input across any unsent intermediate ticks,
        // but do not guess the first missing input.
        let mut inputs = self.inputs.iter().peekable();
        let mut held: Option<PlayerInput> = None;
        for tick in (authoritative_tick + 1)..=last_tick {
            while inputs.peek().is_some_and(|entry| entry.tick <= tick) {
                held = inputs.next().map(|entry| entry.input);
            }
            let input = held?;
            if input.hook != 0 { return None; }
            let step = sim.step(map, SimInput {
                direction: input.direction,
                jump: input.jump != 0,
                hook: false,
                aim: Vec2::new(input.target_x as f32, input.target_y as f32),
            }).ok()?;
            if !step.position.x.is_finite() || !step.position.y.is_finite() {
                return None;
            }
        }
        if (sim.position.x - server_pos.x).abs() > MAX_PREDICTION_OFFSET
            || (sim.position.y - server_pos.y).abs() > MAX_PREDICTION_OFFSET
        {
            return None;
        }
        Some(PredictedLocal {
            client_id: id,
            tick: last_tick,
            position: sim.position,
            velocity: sim.character.velocity,
            replayed_ticks: steps,
        })
    }
}

/// Modify only the *display clone*, leaving the server's original snapshot,
/// ACK, CRC and all other players untouched.
pub fn apply_visual_prediction(
    displayed: &mut SnapshotState, prediction: PredictedLocal,
) -> bool {
    let Some(local) = displayed.items.iter_mut()
        .find(|item| item.type_id == CHARACTER_TYPE && item.id == prediction.client_id)
    else { return false; };
    if local.words.len() < 15 { return false; }
    if !prediction.position.x.is_finite() || !prediction.position.y.is_finite() {
        return false;
    }
    local.words[1] = prediction.position.x.round() as i32;
    local.words[2] = prediction.position.y.round() as i32;
    local.words[3] = (prediction.velocity.x * 256.0).round() as i32;
    local.words[4] = (prediction.velocity.y * 256.0).round() as i32;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_delta::SnapshotObject;

    fn empty_map() -> GameMap {
        GameMap::from_tile_bytes(32, 20, &vec![0; 32 * 20 * 4]).unwrap()
    }

    fn snapshot(tick: i32, x: i32, y: i32) -> SnapshotState {
        let mut words = vec![0i32; 22];
        words[0] = tick;
        words[1] = x;
        words[2] = y;
        words[8] = -1;
        SnapshotState { items: vec![
            SnapshotObject { type_id: CHARACTER_TYPE, id: 3, words },
            SnapshotObject { type_id: 10, id: 3, words: vec![1, 3, 0, 0, 0] },
        ] }
    }

    fn input(tick: i32, direction: i32) -> InputFrame {
        InputFrame {
            intended_tick: tick, last_ack_snapshot: -1,
            input: PlayerInput { direction, ..PlayerInput::default() },
        }
    }

    #[test]
    fn replay_is_seeded_from_server_and_corrected_by_next_snapshot() {
        let map = empty_map();
        let mut predictor = LocalPredictor::new();
        for tick in 101..=105 { predictor.record_sent_input(input(tick, 1)); }
        let baseline = snapshot(100, 100, 100);
        let five = predictor.predict(100, &baseline, &map).unwrap();
        assert_eq!(five.client_id, 3);
        assert_eq!(five.tick, 105);
        assert_eq!(five.replayed_ticks, 5);
        assert!(five.position.x > 100.0);
        assert_eq!(baseline.find(CHARACTER_TYPE, 3).unwrap().words[1], 100);

        // Independently advance the same offline physics to tick 102, then
        // reconcile against this newer authoritative snapshot and replay.
        let mut sim = TeeSimulator::new(Vec2::new(100.0, 100.0));
        for _ in 0..2 {
            sim.step(&map, SimInput { direction: 1, ..SimInput::default() }).unwrap();
        }
        let mut next = snapshot(102, sim.position.x.round() as i32,
            sim.position.y.round() as i32);
        let player = next.items.iter_mut().find(|obj| obj.type_id == CHARACTER_TYPE).unwrap();
        player.words[3] = (sim.character.velocity.x * 256.0).round() as i32;
        player.words[4] = (sim.character.velocity.y * 256.0).round() as i32;
        let update = predictor.predict(102, &next, &map).unwrap();
        assert_eq!(update.replayed_ticks, 3);
        assert_eq!(update.tick, 105);
        assert!(update.position.x >= next.items[0].words[1] as f32);

        let mut display = next.clone();
        assert!(apply_visual_prediction(&mut display, update));
        assert_eq!(display.find(CHARACTER_TYPE, 3).unwrap().words[1],
            update.position.x.round() as i32);
        assert_eq!(next.items[0].words[1], sim.position.x.round() as i32);
        assert!(predictor.predict(105, &next, &map).is_none());
    }

    #[test]
    fn prediction_requires_identified_local_character_and_valid_input() {
        let map = empty_map();
        let mut predictor = LocalPredictor::new();
        predictor.record_sent_input(input(101, 1));
        let baseline = snapshot(100, 100, 100);
        assert!(predictor.predict(100, &baseline, &map).is_some());
        let mut unknown = baseline.clone();
        unknown.items.retain(|obj| obj.type_id != 10);
        assert!(predictor.predict(100, &unknown, &map).is_none());
        unknown.items.push(SnapshotObject {
            type_id: 10, id: 3, words: vec![1, 999, 0, 0, 0],
        });
        assert_eq!(local_character_id(&unknown), None);
        assert!(predictor.predict(100, &unknown, &map).is_none());
        predictor.reset();
        assert_eq!(predictor.recorded_count(), 0);
        assert!(predictor.predict(100, &baseline, &map).is_none());
    }

    #[test]
    fn hooking_or_player_contact_falls_back_to_server_snapshot() {
        let map = empty_map();
        let mut predictor = LocalPredictor::new();
        let baseline = snapshot(100, 100, 100);
        let mut hooking = input(101, 1);
        hooking.input.hook = 1;
        predictor.record_sent_input(hooking);
        assert!(predictor.predict(100, &baseline, &map).is_none());
        predictor.reset();
        predictor.record_sent_input(input(101, 1));

        let mut hooked = baseline.clone();
        hooked.items[0].words[9] = 5;
        assert!(predictor.predict(100, &hooked, &map).is_none());
        let mut crowded = baseline.clone();
        let mut peer = vec![0; 22];
        peer[1] = 124;
        peer[2] = 110;
        crowded.items.push(SnapshotObject { type_id: CHARACTER_TYPE, id: 4, words: peer });
        assert!(predictor.predict(100, &crowded, &map).is_none());
    }

    #[test]
    fn bounded_history_discards_old_epochs_and_limits_replay() {
        let map = empty_map();
        let baseline = snapshot(290, 100, 100);
        let mut predictor = LocalPredictor::new();
        for tick in 1..=300 { predictor.record_sent_input(input(tick, 1)); }
        assert_eq!(predictor.recorded_count(), MAX_INPUT_HISTORY);
        assert!(predictor.predict(290, &baseline, &map).is_some());
        assert!(predictor.predict(100, &snapshot(100, 100, 100), &map).is_none());
        predictor.record_sent_input(input(42, 0));
        assert_eq!(predictor.recorded_count(), 1);
        assert!(predictor.predict(100, &snapshot(100, 100, 100), &map).is_none());
    }

    #[test]
    fn missing_history_and_invalid_physics_do_not_extrapolate() {
        let map = empty_map();
        let baseline = snapshot(100, 100, 100);
        let mut predictor = LocalPredictor::new();
        predictor.record_sent_input(input(103, 1));
        assert!(predictor.predict(100, &baseline, &map).is_none());

        predictor.reset();
        predictor.record_sent_input(input(101, 1));
        let mut extreme = baseline.clone();
        extreme.items[0].words[3] = i32::MAX;
        assert!(predictor.predict(100, &extreme, &map).is_none());
        // Rendering without the tee in a frame must not insert an object.
        let mut missing = SnapshotState::default();
        let prediction = predictor.predict(100, &baseline, &map).unwrap();
        assert!(!apply_visual_prediction(&mut missing, prediction));
        assert!(missing.items.is_empty());
    }
}
