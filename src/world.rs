//! Small, deterministic multi-character DDNet physics sandbox (up to 64).
//!
//! Tick all characters on a shared 50 Hz clock, apply pairwise
//! CCharacterCore::TickDeferred-style pushes and hooked-player drag,
//! then integrate world movement. The scope is deliberately partial:
//! no TeamsCore, switch restrictions, tee teleporting or network prediction.

use crate::hook::{HookError, HookPeer, HookState};
use crate::map::GameMap;
use crate::physics::Vec2;
use crate::simulator::{SimError, SimInput, SimStep, TeeSimulator};

pub const MAX_WORLD_PLAYERS: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayerRules {
    pub team: u8,
    pub solo: bool,
    pub super_mode: bool,
    pub collision_disabled: bool,
    pub hook_disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldPlayer {
    pub id: u8,
    pub tee: TeeSimulator,
    pub rules: PlayerRules,
}

#[derive(Debug, Clone, PartialEq)]
pub struct World {
    pub tick: u64,
    /// Sorted by client ID, reflecting the slot order of CWorldCore.
    pub players: Vec<WorldPlayer>,
    pub player_collision: bool,
}

impl Default for World {
    fn default() -> Self {
        Self { tick: 0, players: Vec::new(), player_collision: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldError {
    InvalidPlayer,
    DuplicatePlayer,
    MissingPlayer,
    TooManyPlayers,
    InvalidInput,
    Sim(SimError),
    Hook(HookError),
}
impl From<SimError> for WorldError {
    fn from(error: SimError) -> Self { Self::Sim(error) }
}
impl From<HookError> for WorldError {
    fn from(error: HookError) -> Self { Self::Hook(error) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorldStep {
    pub tick: u64,
    pub steps: Vec<(u8, SimStep)>,
}

fn eligible(a: PlayerRules, b: PlayerRules) -> bool {
    a.super_mode || b.super_mode
        || (!a.solo && !b.solo && a.team == b.team)
}

fn can_collide(a: PlayerRules, b: PlayerRules, enabled: bool) -> bool {
    eligible(a, b) && (a.super_mode || b.super_mode
        || (enabled && !a.collision_disabled && !b.collision_disabled))
}

fn can_hook(a: PlayerRules, b: PlayerRules) -> bool {
    eligible(a, b) && !a.hook_disabled
}

/// CCharacterCore::TickDeferred's player-vs-player nudge.
/// This is a directional push: each participant applies it on its own tick.
fn collide_velocity(actor: &mut TeeSimulator, other: &TeeSimulator) {
    let delta = Vec2::new(
        actor.position.x - other.position.x,
        actor.position.y - other.position.y,
    );
    let dist = delta.x.hypot(delta.y);
    let physical = actor.hook_tuning.physical_size;
    if dist <= 0.0 || dist >= physical * 1.25 { return; }

    let direction = Vec2::new(delta.x / dist, delta.y / dist);
    let speed = actor.character.velocity.x.hypot(actor.character.velocity.y);
    let strength = if speed > 0.0001 {
        let dot = (actor.character.velocity.x * direction.x
            + actor.character.velocity.y * direction.y) / speed;
        1.0 - (dot + 1.0) / 2.0
    } else {
        0.5
    };
    let amount = (physical * 1.45 - dist) * (strength * 0.75);
    actor.character.velocity.x =
        (actor.character.velocity.x + direction.x * amount) * 0.85;
    actor.character.velocity.y =
        (actor.character.velocity.y + direction.y * amount) * 0.85;
}

/// CCharacterCore::Move player collision is a swept test rather than a
/// final-only circle overlap. Revert to the last non-overlapping sample.
fn stop_at_players(
    start: Vec2, end: Vec2, radius: f32,
    targets: impl Iterator<Item = Vec2>,
) -> Vec2 {
    let targets: Vec<_> = targets.collect();
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let distance = dx.hypot(dy);
    if distance <= 0.0 { return end; }
    let steps = (distance + 1.0) as u32;
    let mut last = start;
    for i in 0..steps {
        let alpha = i as f32 / distance;
        let pos = Vec2::new(start.x + dx * alpha, start.y + dy * alpha);
        for target in &targets {
            let separation = (pos.x - target.x).hypot(pos.y - target.y);
            if separation < radius {
                if alpha > 0.0 {
                    return last;
                }
                // If already inside another tee, let the character
                // move away, as in upstream CCharacterCore::Move.
                if (end.x - target.x).hypot(end.y - target.y) > separation {
                    return end;
                }
                return start;
            }
        }
        last = pos;
    }
    end
}

impl World {
    pub fn insert(&mut self, player: WorldPlayer) -> Result<(), WorldError> {
        if usize::from(player.id) >= MAX_WORLD_PLAYERS
            || ![
                player.tee.position.x, player.tee.position.y,
                player.tee.character.velocity.x, player.tee.character.velocity.y,
            ].iter().all(|x| x.is_finite())
        {
            return Err(WorldError::InvalidPlayer);
        }
        if self.players.len() >= MAX_WORLD_PLAYERS {
            return Err(WorldError::TooManyPlayers);
        }
        match self.players.binary_search_by_key(&player.id, |p| p.id) {
            Ok(_) => Err(WorldError::DuplicatePlayer),
            Err(index) => {
                self.players.insert(index, player);
                Ok(())
            }
        }
    }

    pub fn remove(&mut self, id: u8) -> Result<WorldPlayer, WorldError> {
        let index = self.players.binary_search_by_key(&id, |p| p.id)
            .map_err(|_| WorldError::MissingPlayer)?;
        Ok(self.players.remove(index))
    }

    pub fn get(&self, id: u8) -> Option<&WorldPlayer> {
        self.players.binary_search_by_key(&id, |p| p.id)
            .ok().map(|index| &self.players[index])
    }

    /// One transactional 50 Hz world tick.
    /// Inputs are keyed by client IDs, not Vec order.
    pub fn step(
        &mut self,
        map: &GameMap,
        inputs: &[(u8, SimInput)],
    ) -> Result<WorldStep, WorldError> {
        let mut seen = [false; MAX_WORLD_PLAYERS];
        for &(id, _) in inputs {
            let slot = usize::from(id);
            if slot >= MAX_WORLD_PLAYERS || seen[slot] || self.get(id).is_none() {
                return Err(WorldError::InvalidInput);
            }
            seen[slot] = true;
        }
        let mut next = self.clone();
        let n = next.players.len();
        let mut events = Vec::with_capacity(n);

        // All tee positions are unchanged during the Tick stage.
        // This ensures hook acquisition uses one coherent world frame.
        for i in 0..n {
            let actor = next.players[i];
            let peers: Vec<_> = next.players.iter().filter(|p| p.id != actor.id)
                .map(|p| HookPeer {
                    id: p.id, position: p.tee.position,
                    velocity: p.tee.character.velocity,
                    can_hook: can_hook(actor.rules, p.rules),
                    can_keep_hook: can_hook(actor.rules, p.rules),
                }).collect();
            let input = inputs.iter().find(|(id, _)| *id == actor.id)
                .map(|(_, input)| *input).unwrap_or_default();
            let (jump, hook) = next.players[i]
                .tee.tick_core(map, input, &peers)?;
            events.push((jump, hook));
        }

        // CCharacterCore::TickDeferred visits peers in ascending slot
        // order. The owner can modify its own and target's velocities.
        for i in 0..n {
            for j in 0..n {
                if i == j { continue; }
                let (actor, other) = if i < j {
                    let (left, right) = next.players.split_at_mut(j);
                    (&mut left[i], &mut right[0])
                } else {
                    let (left, right) = next.players.split_at_mut(i);
                    (&mut right[0], &mut left[j])
                };
                if !eligible(actor.rules, other.rules) { continue; }
                if can_collide(actor.rules, other.rules, next.player_collision) {
                    collide_velocity(&mut actor.tee, &other.tee);
                }
                if can_hook(actor.rules, other.rules)
                    && actor.tee.hook.state == HookState::Grabbed
                    && actor.tee.hook.hooked_player == Some(other.id)
                {
                    let mut target = HookPeer {
                        id: other.id, position: other.tee.position,
                        velocity: other.tee.character.velocity,
                        can_hook: true, can_keep_hook: true,
                    };
                    actor.tee.hook.apply_player_drag(
                        actor.tee.position,
                        &mut actor.tee.character.velocity,
                        &mut target,
                        actor.tee.hook_tuning,
                    )?;
                    other.tee.character.velocity = target.velocity;
                }
            }
        }

        // Move each tee after all deferred interactions, preserving
        // the other characters' current positions for swept collision.
        let mut steps = Vec::with_capacity(n);
        for (i, &(jump, hook)) in events.iter().enumerate() {
            let old = next.players[i].tee.position;
            let grounded = next.players[i].tee.move_core(map)?;
            let new_pos = next.players[i].tee.position;
            let rule = next.players[i].rules;
            // Super mode overrides both the global collision setting and
            // per-tee collision disabling in original CCharacterCore::Move.
            let colliders: Vec<_> = next.players.iter().enumerate()
                .filter(|(j, p)| *j != i
                    && can_collide(rule, p.rules, next.player_collision))
                .map(|(_, p)| p.tee.position)
                .collect();
            if !colliders.is_empty() {
                let position = stop_at_players(
                    old, new_pos, next.players[i].tee.hook_tuning.physical_size,
                    colliders.into_iter(),
                );
                next.players[i].tee.position = position;
            }
            let tee = next.players[i].tee;
            steps.push((next.players[i].id, SimStep {
                tick: tee.tick, position: tee.position,
                velocity: tee.character.velocity,
                jump, hook, grounded,
            }));
        }
        next.tick += 1;
        *self = next;
        Ok(WorldStep { tick: self.tick, steps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::HookEvent;

    fn flat_map() -> GameMap {
        GameMap::from_tile_bytes(50, 10, &vec![0; 50 * 10 * 4]).unwrap()
    }
    fn player(id: u8, x: f32) -> WorldPlayer {
        WorldPlayer {
            id, tee: TeeSimulator::new(Vec2::new(x, 80.0)),
            rules: PlayerRules::default(),
        }
    }

    #[test]
    fn slot_order_and_shared_tick_are_deterministic() {
        let map = flat_map();
        let mut a = World::default();
        a.insert(player(9, 400.0)).unwrap();
        a.insert(player(2, 100.0)).unwrap();
        assert_eq!(a.players.iter().map(|p|p.id).collect::<Vec<_>>(), vec![2, 9]);
        let mut b = a.clone();
        for tick in 1..=100 {
            let input = SimInput { direction: 1, ..SimInput::default() };
            let x = a.step(&map, &[(9, input), (2, input)]).unwrap();
            let y = b.step(&map, &[(2, input), (9, input)]).unwrap();
            assert_eq!(x, y);
            assert_eq!(x.tick, tick);
            assert_eq!(x.steps.len(), 2);
            assert_eq!(a.players[0].tee.tick, tick);
        }
        assert_eq!(a, b);
    }

    #[test]
    fn shared_world_hook_grabs_and_drags_other_tee() {
        let map = flat_map();
        let mut world = World::default();
        world.insert(player(1, 100.0)).unwrap();
        world.insert(player(2, 230.0)).unwrap();
        let controls = SimInput {
            direction: 0, jump: false, hook: true,
            aim: Vec2::new(1.0, 0.0),
        };
        let result = world.step(&map, &[(1, controls)]).unwrap();
        assert_eq!(result.steps[0].1.hook, HookEvent::AttachPlayer);
        assert_eq!(world.get(1).unwrap().tee.hook.hooked_player, Some(2));
        let target_vel = world.get(2).unwrap().tee.character.velocity.x;
        let actor_vel = world.get(1).unwrap().tee.character.velocity.x;
        assert!(target_vel < 0.0);
        assert!(actor_vel > 0.0);
    }

    #[test]
    fn overlapping_players_are_pushed_apart_in_deferred_phase() {
        let map = flat_map();
        let mut world = World::default();
        world.insert(player(0, 100.0)).unwrap();
        world.insert(player(1, 120.0)).unwrap();
        world.step(&map, &[]).unwrap();
        let a = world.get(0).unwrap().tee.character.velocity.x;
        let b = world.get(1).unwrap().tee.character.velocity.x;
        assert!(a < 0.0, "left tee should be pushed left: {a}");
        assert!(b > 0.0, "right tee should be pushed right: {b}");
    }

    #[test]
    fn team_and_solo_rules_disallow_cross_group_hooking() {
        let map = flat_map();
        let mut world = World::default();
        let mut a = player(0, 100.0);
        a.rules.team = 1;
        let mut b = player(1, 180.0);
        b.rules.team = 2;
        world.insert(a).unwrap();
        world.insert(b).unwrap();
        let control = SimInput { hook: true, aim: Vec2::new(1.0, 0.0),
            ..SimInput::default() };
        let step = world.step(&map, &[(0, control)]).unwrap();
        assert_ne!(step.steps[0].1.hook, HookEvent::AttachPlayer);
        assert_eq!(world.get(0).unwrap().tee.hook.hooked_player, None);
        world.remove(1).unwrap();
        let mut b = player(1, 180.0);
        b.rules.team = 1;
        b.rules.solo = true;
        world.insert(b).unwrap();
        world.step(&map, &[(0, SimInput { hook: false, ..control })]).unwrap();
        let step = world.step(&map, &[(0, control)]).unwrap();
        assert_ne!(step.steps[0].1.hook, HookEvent::AttachPlayer);
    }

    #[test]
    fn super_player_still_blocks_movement_when_normal_collision_is_off() {
        let map = flat_map();
        let mut ordinary = World::default();
        ordinary.player_collision = false;
        let mut moving = player(1, 100.0);
        moving.rules.collision_disabled = true;
        moving.tee.character.velocity.x = 80.0;
        ordinary.insert(moving).unwrap();
        ordinary.insert(player(2, 155.0)).unwrap();

        let mut super_world = ordinary.clone();
        super_world.players[0].rules.super_mode = true;

        let input = [(1, SimInput { direction: 1, ..SimInput::default() })];
        ordinary.step(&map, &input).unwrap();
        super_world.step(&map, &input).unwrap();

        let unblocked = ordinary.get(1).unwrap().tee.position.x;
        let blocked = super_world.get(1).unwrap().tee.position.x;
        assert!(unblocked > 130.0, "normal collision is disabled: {unblocked}");
        assert!(blocked < 130.0, "super tee should hit other player: {blocked}");
        assert!(blocked < unblocked);
    }

    #[test]
    fn invalid_inputs_do_not_commit_partial_world_state() {
        let map = flat_map();
        let mut world = World::default();
        world.insert(player(1, 100.0)).unwrap();
        world.insert(player(2, 200.0)).unwrap();
        let before = world.clone();
        assert_eq!(world.step(&map, &[(1, SimInput::default()), (1, SimInput::default())]),
            Err(WorldError::InvalidInput));
        assert_eq!(world, before);
        assert_eq!(world.step(&map, &[(3, SimInput::default())]), Err(WorldError::InvalidInput));
        assert_eq!(world, before);
        let invalid = SimInput { aim: Vec2::new(f32::NAN, 0.0), ..SimInput::default() };
        assert_eq!(world.step(&map, &[(1, invalid)]),
            Err(WorldError::Sim(SimError::InvalidInput)));
        assert_eq!(world, before);
    }
}
