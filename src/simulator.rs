//! A small deterministic 50 Hz vanilla-style offline tee simulation.
//!
//! Integrates the existing scalar CharacterCore movement and jumping,
//! basic map MoveBox collision and isolated ground/nohook Hook physics.
//! Does not cover player collisions/hooks, other physics layers, weapons,
//! switches, tune zones, inputs from a server or full DDNet prediction.
//! Reference version: DDNet 20.2.1.

use crate::hook::{Hook, HookError, HookEvent, HookInput, HookTuning};
use crate::map::{GameMap, MapError};
use crate::physics::{Character, Input, JumpEvent, Tuning, Vec2};

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

    /// A complete tick within this restricted static-map simulation.
    /// On invalid motion, internal state is unchanged.
    pub fn step(
        &mut self, map: &GameMap, input: SimInput,
    ) -> Result<SimStep, SimError> {
        if ![input.aim.x, input.aim.y, self.position.x, self.position.y]
            .iter().all(|value| value.is_finite())
        {
            return Err(SimError::InvalidInput);
        }

        let mut next = *self;
        let physical = next.hook_tuning.physical_size;
        let grounded_before = map.is_on_ground(next.position, physical);
        let direction = input.direction.clamp(-1, 1);

        let jump = next.character.tick_without_collision(
            Input { direction, jump: input.jump },
            grounded_before,
            &next.movement_tuning,
        );

        let hook = next.hook.tick(
            HookInput { held: input.hook, aim: input.aim, movement: direction },
            next.position, &mut next.character.velocity,
            map, next.hook_tuning,
        )?;

        let size = Vec2::new(physical, physical);
        let mut grounded_on_contact = false;
        map.move_box(
            &mut next.position, &mut next.character.velocity, size,
            Vec2::default(), &mut grounded_on_contact,
        )?;

        let grounded_after = grounded_on_contact || map.is_on_ground(next.position, physical);
        next.tick += 1;
        let output = SimStep {
            tick: next.tick,
            position: next.position,
            velocity: next.character.velocity,
            jump,
            hook,
            grounded: grounded_after,
        };
        *self = next;
        Ok(output)
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
