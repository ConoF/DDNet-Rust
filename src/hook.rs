//! Partial vanilla DDNet hook motion on static solid/nohook tile grids.
//! Source: pinned DDNet 20.2.1 gamecore.cpp, gamecore.h and tuning.h.
//! Missing player hook, tele-hook, front-layer blockers, switch and tune zones.

use crate::map::GameMap;
use crate::physics::Vec2;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum HookState {
    Retracted = -1,
    #[default]
    Idle = 0,
    RetractStart = 1,
    RetractMid = 2,
    RetractEnd = 3,
    Flying = 4,
    Grabbed = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    None, Launch, AttachGround, HitNoHook, Retracted, Released,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HookTuning {
    pub length: f32,
    pub fire_speed: f32,
    pub drag_accel: f32,
    pub drag_speed: f32,
    pub duration: f32,
    pub physical_size: f32,
}

impl Default for HookTuning {
    fn default() -> Self {
        Self {
            length: 380.0, fire_speed: 80.0, drag_accel: 3.0,
            drag_speed: 15.0, duration: 1.25, physical_size: 28.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hook {
    pub state: HookState,
    pub position: Vec2,
    pub direction: Vec2,
    pub tick: f32,
}

impl Default for Hook {
    fn default() -> Self {
        Self {
            state: HookState::Idle, position: Vec2::default(),
            direction: Vec2::default(), tick: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookError { InvalidInput, InvalidTuning }

fn length(v: Vec2) -> f32 { v.x.hypot(v.y) }
fn difference(a: Vec2, b: Vec2) -> Vec2 { Vec2::new(a.x - b.x, a.y - b.y) }
fn sum(a: Vec2, b: Vec2) -> Vec2 { Vec2::new(a.x + b.x, a.y + b.y) }
fn scaled(v: Vec2, x: f32) -> Vec2 { Vec2::new(v.x * x, v.y * x) }
fn normalize(v: Vec2) -> Vec2 {
    let dist = length(v);
    if dist <= 0.0 { Vec2::default() }
    else { Vec2::new(v.x / dist, v.y / dist) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RayHit { Nothing, Solid, NoHook }

fn intersect_static(map: &GameMap, from: Vec2, to: Vec2) -> (RayHit, Vec2) {
    let displacement = difference(to, from);
    let steps = ((length(displacement) + 1.0) as i32).max(1);
    for n in 0..=steps {
        let point = sum(from, scaled(displacement, n as f32 / steps as f32));
        match map.collision_tile_at_pixel(point.x.round() as i32, point.y.round() as i32) {
            1 => return (RayHit::Solid, point),
            3 => return (RayHit::NoHook, point),
            _ => {}
        }
    }
    (RayHit::Nothing, to)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HookInput {
    pub held: bool,
    pub aim: Vec2,
    pub movement: i32,
}

impl Hook {
    /// One game tick for an isolated static-tile hook. Velocity changes only.
    /// Hook/player/tele collisions need other simulation subsystems.
    pub fn tick(
        &mut self, input: HookInput, pos: Vec2, vel: &mut Vec2,
        map: &GameMap, t: HookTuning,
    ) -> Result<HookEvent, HookError> {
        if ![input.aim.x, input.aim.y, pos.x, pos.y, vel.x, vel.y, t.length,
              t.fire_speed, t.drag_accel, t.drag_speed, t.duration,
              t.physical_size].iter().all(|x| x.is_finite()) {
            return Err(HookError::InvalidInput);
        }
        if t.length <= 0.0 || t.length > 8192.0 || t.fire_speed < 0.0
            || t.fire_speed > 8192.0 || t.drag_accel < 0.0
            || t.drag_speed <= 0.0 || t.duration < 0.0 || t.physical_size <= 0.0
        {
            return Err(HookError::InvalidTuning);
        }

        if !input.held {
            let event = if self.state == HookState::Idle {
                HookEvent::None
            } else { HookEvent::Released };
            self.state = HookState::Idle;
            self.position = pos;
            return Ok(event);
        }

        let mut event = HookEvent::None;
        if self.state == HookState::Idle {
            // Upstream CCharacterCore::Tick launches even if the input
            // target is (0, 0): normalize(0) is (0, 0), so the hook
            // remains at the tee until released.
            let direction = normalize(input.aim);
            self.state = HookState::Flying;
            self.position = sum(pos, scaled(direction, t.physical_size * 1.5));
            self.direction = direction;
            self.tick = 50.0 * (1.25 - t.duration);
            event = HookEvent::Launch;
        }

        match self.state {
            HookState::Idle => self.position = pos,
            HookState::RetractStart => self.state = HookState::RetractMid,
            HookState::RetractMid => self.state = HookState::RetractEnd,
            HookState::RetractEnd => {
                self.state = HookState::Retracted;
                event = HookEvent::Retracted;
            }
            HookState::Flying => {
                let mut next = sum(self.position, scaled(self.direction, t.fire_speed));
                if length(difference(next, pos)) > t.length {
                    next = sum(pos, scaled(normalize(difference(next, pos)), t.length));
                    self.state = HookState::RetractStart;
                }
                // Upstream can switch to HOOK_RETRACT_START *before*
                // the collision trace, when the new tip exceeds HookLength.
                // In that case m_HookPos is not advanced to the clamped
                // end point (even if it happens to touch a wall there).
                if self.state == HookState::Flying {
                    let (hit, collision_pos) = intersect_static(map, self.position, next);
                    self.position = collision_pos;
                    match hit {
                        RayHit::Solid => {
                            self.state = HookState::Grabbed;
                            event = HookEvent::AttachGround;
                        }
                        RayHit::NoHook => {
                            self.state = HookState::RetractStart;
                            event = HookEvent::HitNoHook;
                        }
                        RayHit::Nothing => {}
                    }
                }
            }
            HookState::Grabbed | HookState::Retracted => {}
        }

        if self.state == HookState::Grabbed {
            let delta = difference(self.position, pos);
            if length(delta) > 46.0 {
                let mut drag = scaled(normalize(delta), t.drag_accel);
                if drag.y > 0.0 { drag.y *= 0.3; }
                if (drag.x < 0.0 && input.movement < 0) || (drag.x > 0.0 && input.movement > 0) {
                    drag.x *= 0.95;
                } else {
                    drag.x *= 0.75;
                }
                let candidate = sum(*vel, drag);
                if length(candidate) < t.drag_speed || length(candidate) < length(*vel) {
                    *vel = candidate;
                }
            }
            self.tick += 1.0;
        }
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(tile: u8) -> GameMap {
        let mut bytes = vec![0u8; 20 * 6 * 4];
        for y in 0..6 { bytes[(y * 20 + 7) * 4] = tile; }
        GameMap::from_tile_bytes(20, 6, &bytes).unwrap()
    }

    #[test]
    fn upstream_constants_and_hook_state_numbers() {
        let t = HookTuning::default();
        assert_eq!(t.length, 380.0);
        assert_eq!(t.fire_speed, 80.0);
        assert_eq!(t.drag_accel, 3.0);
        assert_eq!(HookState::Idle as i32, 0);
        assert_eq!(HookState::Flying as i32, 4);
        assert_eq!(HookState::Grabbed as i32, 5);
    }

    #[test]
    fn hook_launch_impact_and_drag() {
        let map = wall(1);
        let mut hook = Hook::default();
        let mut velocity = Vec2::default();
        let tee = Vec2::new(100.0, 80.0);
        let tuning = HookTuning::default();
        assert_eq!(hook.tick(HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 1 },
            tee, &mut velocity, &map, tuning), Ok(HookEvent::Launch));
        assert_eq!(hook.state, HookState::Flying);
        assert_eq!(hook.position, Vec2::new(222.0, 80.0));
        assert_eq!(hook.tick(HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 1 },
            tee, &mut velocity, &map, tuning), Ok(HookEvent::AttachGround));
        assert_eq!(hook.state, HookState::Grabbed);
        assert!((223.0..=230.0).contains(&hook.position.x));
        assert!(velocity.x > 0.0 && velocity.x <= 3.0);
    }

    #[test]
    fn nohook_wall_retracts_and_release_resets() {
        let map = wall(3);
        let mut hook = Hook::default();
        let mut vel = Vec2::default();
        let tee = Vec2::new(100.0, 80.0);
        let t = HookTuning::default();
        let aim = Vec2::new(1.0, 0.0);
        hook.tick(HookInput { held: true, aim, movement: 1 },
            tee, &mut vel, &map, t).unwrap();
        assert_eq!(hook.tick(HookInput { held: true, aim, movement: 1 },
            tee, &mut vel, &map, t),
            Ok(HookEvent::HitNoHook));
        assert_eq!(hook.state, HookState::RetractStart);
        hook.tick(HookInput { held: true, aim, movement: 1 },
            tee, &mut vel, &map, t).unwrap();
        assert_eq!(hook.state, HookState::RetractMid);
        hook.tick(HookInput { held: true, aim, movement: 1 },
            tee, &mut vel, &map, t).unwrap();
        assert_eq!(hook.state, HookState::RetractEnd);
        assert_eq!(hook.tick(HookInput { held: true, aim, movement: 1 },
            tee, &mut vel, &map, t),
            Ok(HookEvent::Retracted));
        assert_eq!(hook.state, HookState::Retracted);
        assert_eq!(hook.tick(HookInput { held: false, aim, movement: 1 },
            tee, &mut vel, &map, t),
            Ok(HookEvent::Released));
        assert_eq!(hook.state, HookState::Idle);
    }

    #[test]
    fn zero_aim_still_launches_and_stays_in_flying_state() {
        // This follows CCharacterCore::Tick: HOOK_IDLE -> HOOK_FLYING
        // even when normalize(vec2(0, 0)) evaluates to (0, 0).
        let map = wall(1);
        let mut hook = Hook::default();
        let mut velocity = Vec2::default();
        let tee = Vec2::new(100.0, 80.0);
        let controls = HookInput {
            held: true, aim: Vec2::default(), movement: 0,
        };
        assert_eq!(
            hook.tick(controls, tee, &mut velocity, &map, HookTuning::default()),
            Ok(HookEvent::Launch),
        );
        assert_eq!(hook.state, HookState::Flying);
        assert_eq!(hook.position, tee);
        assert_eq!(hook.direction, Vec2::default());
        assert_eq!(
            hook.tick(controls, tee, &mut velocity, &map, HookTuning::default()),
            Ok(HookEvent::None),
        );
        assert_eq!(hook.state, HookState::Flying);
        assert_eq!(hook.position, tee);
        assert_eq!(
            hook.tick(HookInput { held: false, ..controls },
                tee, &mut velocity, &map, HookTuning::default()),
            Ok(HookEvent::Released),
        );
        assert_eq!(hook.state, HookState::Idle);
    }

    #[test]
    fn range_limit_retracts_without_moving_tip_to_clamped_position() {
        let map = GameMap::from_tile_bytes(20, 6, &[0; 20 * 6 * 4]).unwrap();
        let mut hook = Hook::default();
        let tee = Vec2::new(100.0, 80.0);
        let mut velocity = Vec2::default();
        let controls = HookInput {
            held: true, aim: Vec2::new(1.0, 0.0), movement: 0,
        };
        let tuning = HookTuning::default();
        for expected_x in [222.0, 302.0, 382.0, 462.0] {
            hook.tick(controls, tee, &mut velocity, &map, tuning).unwrap();
            assert_eq!(hook.state, HookState::Flying);
            assert_eq!(hook.position.x, expected_x);
        }
        // Next step computes x=542 but clamps to x=480, at the
        // 380px length cap. C++ leaves m_HookPos at x=462.
        assert_eq!(
            hook.tick(controls, tee, &mut velocity, &map, tuning),
            Ok(HookEvent::None),
        );
        assert_eq!(hook.state, HookState::RetractStart);
        assert_eq!(hook.position.x, 462.0);
        assert_eq!(velocity, Vec2::default());
    }

    #[test]
    fn wall_exactly_at_length_cap_must_not_attach() {
        let mut bytes = vec![0u8; 20 * 6 * 4];
        for y in 0..6 {
            bytes[(y * 20 + 15) * 4] = 1; // wall begins at x=480
        }
        let map = GameMap::from_tile_bytes(20, 6, &bytes).unwrap();
        let mut hook = Hook::default();
        let tee = Vec2::new(100.0, 80.0);
        let mut velocity = Vec2::default();
        let controls = HookInput {
            held: true, aim: Vec2::new(1.0, 0.0), movement: 0,
        };
        for _ in 0..4 {
            hook.tick(controls, tee, &mut velocity, &map, HookTuning::default()).unwrap();
        }
        assert_eq!(hook.position.x, 462.0);
        assert_eq!(
            hook.tick(controls, tee, &mut velocity, &map, HookTuning::default()),
            Ok(HookEvent::None),
        );
        assert_eq!(hook.state, HookState::RetractStart);
        assert_eq!(hook.position.x, 462.0);
        for expected in [HookState::RetractMid, HookState::RetractEnd, HookState::Retracted] {
            hook.tick(controls, tee, &mut velocity, &map, HookTuning::default()).unwrap();
            assert_eq!(hook.state, expected);
        }
    }

    #[test]
    fn max_hook_length_and_invalid_input() {
        let empty = GameMap::from_tile_bytes(20, 6, &vec![0; 20 * 6 * 4]).unwrap();
        let mut hook = Hook::default();
        let mut velocity = Vec2::default();
        let tee = Vec2::new(100.0, 80.0);
        for _ in 0..10 {
            hook.tick(HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 0 },
            tee, &mut velocity, &empty, HookTuning::default()).unwrap();
        }
        assert_eq!(hook.state, HookState::Retracted);
        assert!(hook.position.x - tee.x <= 380.01);
        let before = hook;
        assert_eq!(hook.tick(HookInput { held: true, aim: Vec2::new(f32::NAN, 0.0), movement: 0 },
            tee, &mut velocity, &empty, HookTuning::default()),
            Err(HookError::InvalidInput));
        assert_eq!(hook, before);
    }
}
