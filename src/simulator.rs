//! A small deterministic 50 Hz vanilla-style offline tee simulation.
//!
//! Integrates the existing scalar CharacterCore movement and jumping,
//! basic map MoveBox collision and isolated ground/nohook Hook physics.
//! Does not cover player collisions/hooks, other physics layers, weapons,
//! switches, tune zones, inputs from a server or full DDNet prediction.
//! Reference version: DDNet 20.2.1.

use crate::hook::{Hook, HookError, HookEvent, HookInput, HookTuning};
use crate::map::{GameMap, MapError};
use crate::physics::{velocity_ramp, Character, Input, JumpEvent, Tuning, Vec2};

/// CCharacterCore::Tick in the pinned DDNet 20.2.1 gamecore.cpp caps
/// resultant velocity after the movement and hook acceleration phases.
pub const MAX_CORE_VELOCITY: f32 = 6000.0;

/// Match the C++ round_to_int used by CCharacterCore::Write. Rust float-to-int
/// casts saturate, so reject out-of-range input before casting to avoid turning
/// a corrupted predicted state into plausible but incorrect wire values.
fn round_wire_int(value: f32) -> Result<i32, SimError> {
    let rounded = value.round();
    if !rounded.is_finite()
        || !(-2_147_483_648.0..2_147_483_648.0).contains(&rounded)
    {
        return Err(SimError::InvalidInput);
    }
    Ok(rounded as i32)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimInput {
    pub direction: i32,
    pub jump: bool,
    pub hook: bool,
    pub aim: Vec2,
}

impl Default for SimInput {
    fn default() -> Self {
        Self {
            direction: 0, jump: false, hook: false, aim: Vec2::new(0.0, -1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimStep {
    pub tick: u64,
    pub position: Vec2,
    pub velocity: Vec2,
    pub jump: JumpEvent,
    pub hook: HookEvent,
    pub grounded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    InvalidInput,
    InvalidTuning,
    Hook(HookError),
    Collision(MapError),
}

impl From<HookError> for SimError {
    fn from(e: HookError) -> Self { Self::Hook(e) }
}
impl From<MapError> for SimError {
    fn from(e: MapError) -> Self { Self::Collision(e) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeeSimulator {
    pub tick: u64,
    pub position: Vec2,
    pub character: Character,
    pub hook: Hook,
    pub movement_tuning: Tuning,
    pub hook_tuning: HookTuning,
}

impl TeeSimulator {
    pub fn new(position: Vec2) -> Self {
        Self {
            tick: 0,
            position,
            character: Character::default(),
            hook: Hook::default(),
            movement_tuning: Tuning::default(),
            hook_tuning: HookTuning::default(),
        }
    }

    /// Tick movement and hook state only; position is integrated later.
    /// Used by the multi-character world to preserve DDNet tick order.
    pub(crate) fn tick_core(
        &mut self,
        map: &GameMap,
        input: SimInput,
        peers: &[crate::hook::HookPeer],
    ) -> Result<(JumpEvent, HookEvent), SimError> {
        if ![input.aim.x, input.aim.y, self.position.x, self.position.y,
            self.character.velocity.x, self.character.velocity.y]
            .iter().all(|value| value.is_finite())
        {
            return Err(SimError::InvalidInput);
        }
        let grounded_before = map.is_on_ground(
            self.position, self.hook_tuning.physical_size,
        );
        let direction = input.direction.clamp(-1, 1);
        let jump = self.character.tick_without_collision(
            Input { direction, jump: input.jump },
            grounded_before,
            &self.movement_tuning,
        );
        let hook = self.hook.tick_with_players(
            HookInput { held: input.hook, aim: input.aim, movement: direction },
            self.position,
            &mut self.character.velocity,
            map,
            self.hook_tuning,
            peers,
        )?;
        // Upstream applies this clamp at the end of Tick, before Move and
        // deferred world interactions. It is a magnitude clamp, not an
        // independent X/Y clamp (which would change the direction).
        let speed = self.character.velocity.x.hypot(self.character.velocity.y);
        if !speed.is_finite() { return Err(SimError::InvalidInput); }
        if speed > MAX_CORE_VELOCITY {
            let factor = MAX_CORE_VELOCITY / speed;
            self.character.velocity.x *= factor;
            self.character.velocity.y *= factor;
        }
        Ok((jump, hook))
    }

    /// Move a ticked tee against static map collisions. This is the
    /// MoveBox half of CCharacterCore::Move, without other tee collisions.
    pub(crate) fn move_core(&mut self, map: &GameMap) -> Result<bool, SimError> {
        let t = self.movement_tuning;
        if ![
            t.velramp_start, t.velramp_range, t.velramp_curvature,
            t.ground_elasticity_x, t.ground_elasticity_y,
        ].iter().all(|v| v.is_finite())
            || t.velramp_start < 0.0
            || t.velramp_range <= 0.0
            || t.velramp_curvature <= 0.0
        {
            return Err(SimError::InvalidTuning);
        }
        let physical = self.hook_tuning.physical_size;
        let speed = self.character.velocity.x.hypot(self.character.velocity.y) * 50.0;
        let ramp = velocity_ramp(
            speed, t.velramp_start, t.velramp_range, t.velramp_curvature,
        );
        if !ramp.is_finite() || ramp <= 0.0
            || !(self.character.velocity.x * ramp).is_finite()
            || !(1.0 / ramp).is_finite()
        {
            return Err(SimError::InvalidTuning);
        }

        self.character.velocity.x *= ramp;
        let mut grounded_on_contact = false;
        map.move_box(
            &mut self.position, &mut self.character.velocity,
            Vec2::new(physical, physical),
            Vec2::new(t.ground_elasticity_x, t.ground_elasticity_y),
            &mut grounded_on_contact,
        )?;
        if grounded_on_contact {
            self.character.jumped &= !2;
            self.character.jumped_total = 0;
        }
        self.character.velocity.x *= 1.0 / ramp;
        // CCharacter::HandleTiles clamps each velocity component based on
        // CCollision::GetMoveRestrictions after the movement phase.
        self.character.velocity =
            map.clamp_restricted_velocity(self.position, self.character.velocity);
        self.tick += 1;
        Ok(grounded_on_contact || map.is_on_ground(self.position, physical))
    }

    /// Reproduce CCharacterCore::Quantize's Write/Read roundtrip for the
    /// supported netobject fields. The official client calls Quantize during
    /// prediction; offline/world simulation deliberately remains unrounded.
    /// Validate all fields before committing any change.
    pub fn quantize_for_prediction(&mut self) -> Result<(), SimError> {
        let x = round_wire_int(self.position.x)?;
        let y = round_wire_int(self.position.y)?;
        let vx = round_wire_int(self.character.velocity.x * 256.0)?;
        let vy = round_wire_int(self.character.velocity.y * 256.0)?;
        let hook_x = round_wire_int(self.hook.position.x)?;
        let hook_y = round_wire_int(self.hook.position.y)?;
        let hook_dx = round_wire_int(self.hook.direction.x * 256.0)?;
        let hook_dy = round_wire_int(self.hook.direction.y * 256.0)?;
        let hook_tick = round_wire_int(self.hook.tick)?;

        self.position = Vec2::new(x as f32, y as f32);
        self.character.velocity = Vec2::new(vx as f32 / 256.0, vy as f32 / 256.0);
        self.hook.position = Vec2::new(hook_x as f32, hook_y as f32);
        self.hook.direction =
            Vec2::new(hook_dx as f32 / 256.0, hook_dy as f32 / 256.0);
        self.hook.tick = hook_tick as f32;
        Ok(())
    }

    /// Single-player convenience API, preserving transactional validation
    /// even when either the Tick or Move phase encounters invalid input.
    pub fn step(
        &mut self, map: &GameMap, input: SimInput,
    ) -> Result<SimStep, SimError> {
        let mut next = *self;
        let (jump, hook) = next.tick_core(map, input, &[])?;
        let grounded = next.move_core(map)?;
        let step = SimStep {
            tick: next.tick,
            position: next.position,
            velocity: next.character.velocity,
            jump,
            hook,
            grounded,
        };
        *self = next;
        Ok(step)
    }

    /// Deterministic offline input replay for regression testing.
    pub fn replay(
        &mut self, map: &GameMap, inputs: &[SimInput],
    ) -> Result<Vec<SimStep>, SimError> {
        let mut result = Vec::with_capacity(inputs.len());
        for &input in inputs {
            result.push(self.step(map, input)?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::HookState;

    fn world() -> GameMap {
        // 20 wide, 8 high, the fourth row is solid ground, plus a wall.
        let mut tiles = vec![0u8; 20 * 8 * 4];
        for x in 0..20 {
            tiles[(4 * 20 + x) * 4] = 1;
        }
        for y in 0..4 {
            tiles[(y * 20 + 7) * 4] = 1;
        }
        GameMap::from_tile_bytes(20, 8, &tiles).unwrap()
    }

    #[test]
    fn upstream_core_velocity_limit_preserves_direction_and_rejects_overflow() {
        let map = GameMap::from_tile_bytes(32, 20, &vec![0; 32 * 20 * 4]).unwrap();
        let mut tee = TeeSimulator::new(Vec2::new(100.0, 100.0));
        tee.character.velocity = Vec2::new(6000.0, 8000.0);
        tee.tick_core(&map, SimInput::default(), &[]).unwrap();
        let speed = tee.character.velocity.x.hypot(tee.character.velocity.y);
        assert!((speed - MAX_CORE_VELOCITY).abs() < 0.001);
        assert!(tee.character.velocity.x > 0.0);
        assert!(tee.character.velocity.y > 0.0);

        // Gamecore limits *negative* speed by the same norm.
        let mut left = TeeSimulator::new(Vec2::new(100.0, 100.0));
        left.character.velocity = Vec2::new(-8000.0, -6000.0);
        left.tick_core(&map, SimInput::default(), &[]).unwrap();
        assert!((left.character.velocity.x.hypot(left.character.velocity.y)
            - MAX_CORE_VELOCITY).abs() < 0.001);
        assert!(left.character.velocity.x < 0.0 && left.character.velocity.y < 0.0);

        // An overflowed vector is rejected without committing step() changes.
        let mut extreme = TeeSimulator::new(Vec2::new(100.0, 100.0));
        extreme.character.velocity = Vec2::new(f32::MAX, f32::MAX);
        let before = extreme;
        assert_eq!(extreme.step(&map, SimInput::default()), Err(SimError::InvalidInput));
        assert_eq!(extreme, before);
    }

    #[test]
    fn prediction_quantize_matches_upstream_character_core_wire_roundtrip() {
        let mut tee = TeeSimulator::new(Vec2::new(12.5, -8.5));
        tee.character.velocity = Vec2::new(1.5 / 256.0, -1.5 / 256.0);
        tee.hook.position = Vec2::new(128.49, -44.51);
        tee.hook.direction = Vec2::new(127.6 / 256.0, -33.5 / 256.0);
        tee.hook.tick = 11.5;
        tee.quantize_for_prediction().unwrap();
        assert_eq!(tee.position, Vec2::new(13.0, -9.0));
        assert_eq!(tee.character.velocity, Vec2::new(2.0 / 256.0, -2.0 / 256.0));
        assert_eq!(tee.hook.position, Vec2::new(128.0, -45.0));
        assert_eq!(tee.hook.direction, Vec2::new(128.0 / 256.0, -34.0 / 256.0));
        assert_eq!(tee.hook.tick, 12.0);
        let once = tee;
        tee.quantize_for_prediction().unwrap();
        assert_eq!(tee, once, "network core quantization must be idempotent");
    }

    #[test]
    fn prediction_quantize_rejects_invalid_wire_values_transactionally() {
        let mut tee = TeeSimulator::new(Vec2::new(4.25, 8.75));
        tee.character.velocity = Vec2::new(2.0, -1.0);
        tee.hook.position.x = f32::MAX;
        let before = tee;
        assert_eq!(tee.quantize_for_prediction(), Err(SimError::InvalidInput));
        assert_eq!(tee, before);
        tee.hook.position.x = 0.0;
        tee.position.x = f32::INFINITY;
        let before = tee;
        assert_eq!(tee.quantize_for_prediction(), Err(SimError::InvalidInput));
        assert_eq!(tee, before);
        tee.position.x = 4.25;
        tee.hook.tick = f32::NEG_INFINITY;
        let before = tee;
        assert_eq!(tee.quantize_for_prediction(), Err(SimError::InvalidInput));
        assert_eq!(tee, before);
    }

    #[test]
    fn stopper_tile_clamps_direction_after_tick_move() {
        let mut tiles = [0u8; 5 * 5 * 4];
        // STOP facing right, situated next to tee's current tile.
        tiles[(2 * 5 + 3) * 4] = crate::map::TILE_STOP;
        tiles[(2 * 5 + 3) * 4 + 1] = 11;
        let map = GameMap::from_tile_bytes(5, 5, &tiles).unwrap();
        let mut tee = TeeSimulator::new(Vec2::new(80.0, 80.0));
        let step = tee.step(&map, SimInput {
            direction: 1, ..SimInput::default()
        }).unwrap();
        assert!(step.position.x >= 80.0);
        assert_eq!(step.velocity.x, 0.0);
        assert_eq!(tee.character.velocity.x, 0.0);

        let mut away = TeeSimulator::new(Vec2::new(80.0, 80.0));
        away.step(&map, SimInput {
            direction: -1, ..SimInput::default()
        }).unwrap();
        assert!(away.character.velocity.x < 0.0,
            "one-way STOP must allow movement away from the blocker");

        let empty = GameMap::from_tile_bytes(5, 5, &[0; 5 * 5 * 4]).unwrap();
        let mut normal = TeeSimulator::new(Vec2::new(80.0, 80.0));
        normal.step(&empty, SimInput {
            direction: 1, ..SimInput::default()
        }).unwrap();
        assert!(normal.character.velocity.x > 0.0,
            "regular maps keep their original movement simulation");
    }

    #[test]
    fn fifty_hz_jump_and_ground_clearance() {
        let map = world();
        let mut sim = TeeSimulator::new(Vec2::new(64.0, 110.0));
        let jumped = sim.step(&map, SimInput {
            jump: true, ..SimInput::default()
        }).unwrap();
        assert_eq!(jumped.tick, 1);
        assert_eq!(jumped.jump, JumpEvent::Ground);
        assert!(jumped.position.y < 110.0);
        assert!(!jumped.grounded);
        assert!((jumped.velocity.y + 13.2).abs() < 1e-5);
    }

    #[test]
    fn wall_collision_restricts_movement_position() {
        let map = world();
        let mut sim = TeeSimulator::new(Vec2::new(190.0, 80.0));
        sim.character.velocity.x = 19.0;
        for _ in 0..15 {
            let step = sim.step(&map, SimInput {
                direction: 1, ..SimInput::default()
            }).unwrap();
            assert!(step.position.x <= 210.0, "wall penetrated: {:?}", step);
        }
    }

    #[test]
    fn hook_and_horizontal_movement_simulate_together() {
        let map = world();
        let mut sim = TeeSimulator::new(Vec2::new(100.0, 80.0));
        let controls = SimInput {
            hook: true,
            aim: Vec2::new(1.0, 0.0),
            direction: 1,
            ..SimInput::default()
        };
        let first = sim.step(&map, controls).unwrap();
        assert_eq!(first.hook, HookEvent::Launch);
        let second = sim.step(&map, controls).unwrap();
        assert_eq!(second.hook, HookEvent::AttachGround);
        assert_eq!(sim.hook.state, HookState::Grabbed);
        assert!(second.velocity.x > first.velocity.x);
    }

    #[test]
    fn input_replay_is_deterministic() {
        let map = world();
        let inputs = (0..200).map(|tick| SimInput {
            direction: if tick % 100 < 50 { 1 } else { -1 },
            jump: tick % 20 == 0,
            hook: tick % 13 < 5,
            aim: Vec2::new(1.0, -1.0),
        }).collect::<Vec<_>>();

        let mut a = TeeSimulator::new(Vec2::new(64.0, 110.0));
        let mut b = TeeSimulator::new(Vec2::new(64.0, 110.0));
        let x = a.replay(&map, &inputs).unwrap();
        let y = b.replay(&map, &inputs).unwrap();
        assert_eq!(x, y);
        assert_eq!(a, b);
        assert_eq!(x.len(), 200);
        assert_eq!(a.tick, 200);
    }

    #[test]
    fn velocity_ramp_reduces_distance_but_does_not_reduce_saved_speed() {
        // Start far enough from boundaries so there are no collisions.
        let map = GameMap::from_tile_bytes(40, 8, &vec![0; 40 * 8 * 4]).unwrap();
        let mut sim = TeeSimulator::new(Vec2::new(128.0, 80.0));
        sim.character.velocity = Vec2::new(40.0, 0.0);
        let input = SimInput { direction: 1, ..SimInput::default() };
        let expected = velocity_ramp(
            40.0f32.hypot(0.5) * 50.0, 550.0, 2000.0, 1.4,
        );
        assert!(expected < 1.0);
        let step = sim.step(&map, input).unwrap();
        assert!((step.position.x - (128.0 + 40.0 * expected)).abs() < 0.02);
        assert!((step.velocity.x - 40.0).abs() < 0.001);

        // With curvature=1 the original VelocityRamp equals 1 (no slowdown).
        let mut no_ramp = TeeSimulator::new(Vec2::new(128.0, 80.0));
        no_ramp.character.velocity = Vec2::new(40.0, 0.0);
        no_ramp.movement_tuning.velramp_curvature = 1.0;
        let normal_step = no_ramp.step(&map, input).unwrap();
        assert!((normal_step.position.x - 168.0).abs() < 0.02);
        assert!(step.position.x < normal_step.position.x);
    }

    #[test]
    fn ground_elasticity_bounce_resets_air_jump_bit_like_cpp_move() {
        let map = world();
        let mut sim = TeeSimulator::new(Vec2::new(64.0, 100.0));
        sim.character.velocity.y = 30.0;
        sim.character.jumped = 2;
        sim.character.jumped_total = 1;
        sim.movement_tuning.ground_elasticity_y = 1.0;
        let step = sim.step(&map, SimInput::default()).unwrap();
        assert!(step.grounded);
        assert!(step.velocity.y < 0.0);
        assert_eq!(sim.character.jumped & 2, 0);
        assert_eq!(sim.character.jumped_total, 0);
        assert!(!map.test_box(step.position, Vec2::new(28.0, 28.0)));
    }

    #[test]
    fn invalid_ramp_tuning_does_not_mutate_simulator() {
        let map = world();
        let mut sim = TeeSimulator::new(Vec2::new(64.0, 80.0));
        sim.movement_tuning.velramp_range = 0.0;
        let before = sim;
        assert_eq!(sim.step(&map, SimInput::default()), Err(SimError::InvalidTuning));
        assert_eq!(sim, before);
        sim.movement_tuning.velramp_range = 2000.0;
        sim.movement_tuning.velramp_curvature = f32::NAN;
        let before = sim;
        assert_eq!(sim.step(&map, SimInput::default()), Err(SimError::InvalidTuning));
        // IEEE-754 NaN != NaN, so compare preserved simulation state
        // and the exact tuning bit pattern rather than PartialEq on Tuning.
        assert_eq!(sim.tick, before.tick);
        assert_eq!(sim.position, before.position);
        assert_eq!(sim.character, before.character);
        assert_eq!(sim.hook, before.hook);
        assert_eq!(
            sim.movement_tuning.velramp_curvature.to_bits(),
            before.movement_tuning.velramp_curvature.to_bits(),
        );
    }

    #[test]
    fn rejected_inputs_do_not_modify_state() {
        let map = world();
        let mut sim = TeeSimulator::new(Vec2::new(64.0, 110.0));
        let before = sim;
        assert_eq!(sim.step(&map, SimInput {
            aim: Vec2::new(f32::NAN, 1.0), ..SimInput::default()
        }), Err(SimError::InvalidInput));
        assert_eq!(sim, before);
    }
}
