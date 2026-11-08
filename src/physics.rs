//! Port of the scalar movement and jumping portion of CCharacterCore::Tick.
//!
//! Original source: upstream/ddnet/src/game/gamecore.cpp and tuning.h, DDNet 20.2.1.
//! Collision, player interaction, hook, tiles, tune zones, Move and Quantize are
//! not implemented here. This module is NOT a complete prediction engine.

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub ground_control_speed: f32,
    pub ground_control_accel: f32,
    pub ground_friction: f32,
    pub ground_jump_impulse: f32,
    pub air_jump_impulse: f32,
    pub air_control_speed: f32,
    pub air_control_accel: f32,
    pub air_friction: f32,
    pub gravity: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            ground_control_speed: 10.0,
            ground_control_accel: 100.0 / 50.0,
            ground_friction: 0.5,
            ground_jump_impulse: 13.2,
            air_jump_impulse: 12.0,
            air_control_speed: 250.0 / 50.0,
            air_control_accel: 1.5,
            air_friction: 0.95,
            gravity: 0.5,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    /// Expected input values: -1 (left), 0 (neutral), 1 (right).
    pub direction: i32,
    pub jump: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpEvent {
    None,
    Ground,
    Air,
}

/// Character velocity/jump state before collision-aware movement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Character {
    pub velocity: Vec2,
    pub jumped: u8,
    pub jumped_total: u32,
    pub jumps: i32,
    pub direction: i32,
}

impl Default for Character {
    fn default() -> Self {
        Self {
            velocity: Vec2::default(),
            jumped: 0,
            jumped_total: 0,
            jumps: 2,
            direction: 0,
        }
    }
}

impl Character {
    /// Simulates only the movement/jump subsection of one DDNet 50 Hz tick.
    /// `grounded` must be supplied by the future collision system.
    pub fn tick_without_collision(
        &mut self,
        input: Input,
        grounded: bool,
        tuning: &Tuning,
    ) -> JumpEvent {
        self.velocity.y += tuning.gravity;
        self.direction = input.direction.clamp(-1, 1);

        let mut event = JumpEvent::None;
        if input.jump {
            if self.jumped & 1 == 0 {
                if grounded && (self.jumped & 2 == 0 || self.jumps != 0) {
                    event = JumpEvent::Ground;
                    self.velocity.y = -tuning.ground_jump_impulse;
                    if self.jumps > 1 {
                        self.jumped |= 1;
                    } else {
                        self.jumped |= 3;
                    }
                    self.jumped_total = 0;
                } else if self.jumped & 2 == 0 {
                    event = JumpEvent::Air;
                    self.velocity.y = -tuning.air_jump_impulse;
                    self.jumped |= 3;
                    self.jumped_total += 1;
                }
            }
        } else {
            self.jumped &= !1;
        }

        if grounded {
            self.jumped &= !2;
            self.jumped_total = 0;
        }

        let (max_speed, accel, friction) = if grounded {
            (
                tuning.ground_control_speed,
                tuning.ground_control_accel,
                tuning.ground_friction,
            )
        } else {
            (
                tuning.air_control_speed,
                tuning.air_control_accel,
                tuning.air_friction,
            )
        };

        match self.direction {
            -1 => self.velocity.x = saturated_add(-max_speed, max_speed, self.velocity.x, -accel),
            1 => self.velocity.x = saturated_add(-max_speed, max_speed, self.velocity.x, accel),
            _ => self.velocity.x *= friction,
        }
        event
    }
}

/// Preserves DDNet's out-of-range behavior when current speed exceeds limits.
pub fn saturated_add(min: f32, max: f32, current: f32, modifier: f32) -> f32 {
    if modifier < 0.0 {
        if current < min {
            return current;
        }
        (current + modifier).max(min)
    } else {
        if current > max {
            return current;
        }
        (current + modifier).min(max)
    }
}

/// Exact formula from upstream src/game/gamecore.h::CalcPos.
pub fn projectile_position(
    pos: Vec2,
    velocity: Vec2,
    curvature: f32,
    speed: f32,
    time: f32,
) -> Vec2 {
    let time = time * speed;
    Vec2::new(
        pos.x + velocity.x * time,
        pos.y + velocity.y * time + curvature / 10000.0 * (time * time),
    )
}

/// Exact formula from upstream src/game/gamecore.cpp::VelocityRamp.
pub fn velocity_ramp(value: f32, start: f32, range: f32, curvature: f32) -> f32 {
    if value < start {
        1.0
    } else {
        1.0 / curvature.powf((value - start) / range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_default_tuning() {
        let t = Tuning::default();
        assert_eq!(t.ground_control_speed, 10.0);
        assert_eq!(t.ground_control_accel, 2.0);
        assert_eq!(t.air_control_speed, 5.0);
        assert_eq!(t.air_control_accel, 1.5);
        assert_eq!(t.gravity, 0.5);
    }

    #[test]
    fn ground_acceleration_caps_at_tuned_speed() {
        let t = Tuning::default();
        let mut c = Character::default();
        for _ in 0..10 {
            assert_eq!(
                c.tick_without_collision(Input { direction: 1, jump: false }, true, &t),
                JumpEvent::None
            );
        }
        assert_eq!(c.velocity.x, 10.0);
    }

    #[test]
    fn jump_press_release_and_second_jump() {
        let t = Tuning::default();
        let mut c = Character::default();
        let press = Input { direction: 0, jump: true };
        assert_eq!(c.tick_without_collision(press, true, &t), JumpEvent::Ground);
        assert_eq!(c.velocity.y, -13.2);
        assert_eq!(c.jumped, 1);
        assert_eq!(c.tick_without_collision(press, false, &t), JumpEvent::None);
        assert_eq!(c.velocity.y, -12.7);
        c.tick_without_collision(Input::default(), false, &t);
        assert_eq!(c.tick_without_collision(press, false, &t), JumpEvent::Air);
        assert_eq!(c.velocity.y, -12.0);
        assert_eq!(c.jumped, 3);
        assert_eq!(c.jumped_total, 1);
    }

    #[test]
    fn air_friction_and_gravity() {
        let t = Tuning::default();
        let mut c = Character {
            velocity: Vec2::new(4.0, 0.0),
            ..Character::default()
        };
        c.tick_without_collision(Input::default(), false, &t);
        assert_eq!(c.velocity.x, 3.8);
        assert_eq!(c.velocity.y, 0.5);
    }

    #[test]
    fn projectile_and_ramp_math() {
        assert_eq!(saturated_add(-10.0, 10.0, 9.0, 2.0), 10.0);
        assert_eq!(saturated_add(-10.0, 10.0, 20.0, 2.0), 20.0);
        assert_eq!(
            projectile_position(Vec2::new(0.0, 0.0), Vec2::new(1.0, 2.0), 10000.0, 1.0, 1.0),
            Vec2::new(1.0, 3.0)
        );
        assert_eq!(velocity_ramp(100.0, 550.0, 2000.0, 1.4), 1.0);
        assert_eq!(velocity_ramp(550.0, 550.0, 2000.0, 1.4), 1.0);
    }
}
