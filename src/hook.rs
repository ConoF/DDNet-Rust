//! Partial vanilla DDNet hook motion on static solid/nohook tile grids.
//! Source: pinned DDNet 20.2.1 gamecore.cpp, gamecore.h and tuning.h.
//! Player hooks are modeled via explicit peer snapshots, but world-team policy,
//! teleporters, front-layer blockers and movement restrictions remain separate.

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
    None, Launch, AttachGround, AttachPlayer, HitNoHook, Retracted, Released,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HookTuning {
    pub length: f32,
    pub fire_speed: f32,
    pub drag_accel: f32,
    pub drag_speed: f32,
    pub duration: f32,
    pub physical_size: f32,
    pub player_hooking: bool,
}

impl Default for HookTuning {
    fn default() -> Self {
        Self {
            length: 380.0, fire_speed: 80.0, drag_accel: 3.0,
            drag_speed: 15.0, duration: 1.25, physical_size: 28.0,
            player_hooking: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hook {
    pub state: HookState,
    pub position: Vec2,
    pub direction: Vec2,
    pub tick: f32,
    /// The DDNet player slot caught by the hook; None means wall or no target.
    pub hooked_player: Option<u8>,
}

impl Default for Hook {
    fn default() -> Self {
        Self {
            state: HookState::Idle, position: Vec2::default(),
            direction: Vec2::default(), tick: 0.0,
            hooked_player: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookError { InvalidInput, InvalidTuning, InvalidPeers }

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

pub const MAX_HOOK_PLAYERS: usize = 64;

/// A peer from the same 0..63 client slot space as CWorldCore. Callers
/// supply eligible team/solo/super flags through the can_hook boolean.
/// The source player itself must not appear in this list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HookPeer {
    pub id: u8,
    pub position: Vec2,
    pub velocity: Vec2,
    /// True when source/target team, solo and super rules permit hooking.
    pub can_hook: bool,
    /// True while the source is permitted to retain an existing hook.
    pub can_keep_hook: bool,
}

/// The projection is clamped to the segment as in closest_point_on_line
/// from upstream src/base/vmath.h. Zero-length segments have no projection.
fn closest_point_on_segment(from: Vec2, to: Vec2, target: Vec2) -> Option<Vec2> {
    let segment = difference(to, from);
    let length_squared = segment.x * segment.x + segment.y * segment.y;
    if length_squared <= 0.0 { return None; }
    let point = difference(target, from);
    let fraction = ((point.x * segment.x + point.y * segment.y) / length_squared).clamp(0.0, 1.0);
    Some(sum(from, scaled(segment, fraction)))
}

/// A per-component SaturatedAdd port, matching gamecore.h semantics.
/// Speeds already outside the legal limit remain unchanged if the force
/// would push farther out, rather than being forcibly clamped first.
fn saturated_add(minimum: f32, maximum: f32, current: f32, delta: f32) -> f32 {
    if delta < 0.0 {
        if current < minimum { return current; }
        (current + delta).max(minimum)
    } else {
        if current > maximum { return current; }
        (current + delta).min(maximum)
    }
}

impl Hook {
    /// One game tick for an isolated static-tile hook. Velocity changes only.
    /// Hook/player/tele collisions need other simulation subsystems.
    pub fn tick(
        &mut self, input: HookInput, pos: Vec2, vel: &mut Vec2,
        map: &GameMap, t: HookTuning,
    ) -> Result<HookEvent, HookError> {
        self.tick_with_players(input, pos, vel, map, t, &[])
    }

    /// Hook simulation with up to 64 peer snapshots. Peers exclude the
    /// source tee and must have unique, valid client IDs.
    ///
    /// The caller determines team/solo/super eligibility. This method
    /// does not mutate another player's velocity: use apply_player_drag
    /// in a deferred physics phase to model CCharacterCore::TickDeferred.
    pub fn tick_with_players(
        &mut self, input: HookInput, pos: Vec2, vel: &mut Vec2,
        map: &GameMap, t: HookTuning, peers: &[HookPeer],
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

        if peers.len() > MAX_HOOK_PLAYERS {
            return Err(HookError::InvalidPeers);
        }
        let mut seen = [false; MAX_HOOK_PLAYERS];
        for peer in peers {
            let id = usize::from(peer.id);
            if id >= MAX_HOOK_PLAYERS || seen[id]
                || ![peer.position.x, peer.position.y, peer.velocity.x, peer.velocity.y]
                    .iter().all(|value| value.is_finite())
            {
                return Err(HookError::InvalidPeers);
            }
            seen[id] = true;
        }

        if !input.held {
            let event = if self.state == HookState::Idle {
                HookEvent::None
            } else { HookEvent::Released };
            self.state = HookState::Idle;
            self.position = pos;
            self.hooked_player = None;
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
            self.hooked_player = None;
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
                // Upstream tests the hook's segment against the map first,
                // then checks eligible players on the *shortened* segment
                // before applying the ground collision result.
                let (hit, collision_pos) = intersect_static(map, self.position, next);
                let mut selected: Option<(u8, f32)> = None;
                if t.player_hooking {
                    for peer in peers.iter().filter(|peer| peer.can_hook) {
                        let Some(closest) = closest_point_on_segment(
                            self.position, collision_pos, peer.position,
                        ) else { continue; };
                        if length(difference(peer.position, closest)) >= t.physical_size + 2.0 {
                            continue;
                        }
                        let distance = length(difference(peer.position, self.position));
                        // Upstream traverses slots 0..63 and keeps the
                        // nearest; break ties by ID independent of input order.
                        if selected.is_none_or(|(best_id, best_dist)| {
                            distance < best_dist
                                || (distance == best_dist && peer.id < best_id)
                        }) {
                            selected = Some((peer.id, distance));
                        }
                    }
                }
                if let Some((id, _)) = selected {
                    self.state = HookState::Grabbed;
                    self.hooked_player = Some(id);
                    event = HookEvent::AttachPlayer;
                } else if self.state == HookState::Flying {
                    // A range-limit retraction keeps the original tip
                    // stationary; touching a wall at the length boundary
                    // cannot convert that into a ground hook.
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
            if let Some(id) = self.hooked_player {
                if let Some(peer) = peers.iter().find(|peer| peer.id == id && peer.can_keep_hook) {
                    self.position = peer.position;
                } else {
                    self.hooked_player = None;
                    self.state = HookState::Retracted;
                    self.position = pos;
                }
            }
            let delta = difference(self.position, pos);
            // The hook owner receives *no* static-wall acceleration while
            // grabbing a player. Its deferred pull is handled separately.
            if self.state == HookState::Grabbed && self.hooked_player.is_none() && length(delta) > 46.0 {
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
            // CCharacterCore::Tick releases player hooks once HookTick > 60.
            // Ground hooks are not subject to this player-only timeout.
            if self.hooked_player.is_some() && self.tick > 60.0 {
                self.hooked_player = None;
                self.state = HookState::Retracted;
                self.position = pos;
            }
        }
        Ok(event)
    }

    /// Deferred two-player hook influence from CCharacterCore::TickDeferred.
    /// This applies the pairwise force but deliberately omits movement
    /// restriction clipping, player collisions and multi-player world order.
    /// Returns whether a force was applied.
    pub fn apply_player_drag(
        &self, own_position: Vec2, own_velocity: &mut Vec2,
        target: &mut HookPeer, t: HookTuning,
    ) -> Result<bool, HookError> {
        if ![own_position.x, own_position.y, own_velocity.x, own_velocity.y,
            target.position.x, target.position.y,
            target.velocity.x, target.velocity.y].iter().all(|v| v.is_finite())
        {
            return Err(HookError::InvalidInput);
        }
        if ![t.length, t.drag_accel, t.drag_speed].iter().all(|v| v.is_finite())
            || t.length <= 0.0 || t.drag_accel < 0.0 || t.drag_speed <= 0.0
        {
            return Err(HookError::InvalidTuning);
        }
        if self.state != HookState::Grabbed || self.hooked_player != Some(target.id)
            || !t.player_hooking || !target.can_keep_hook
        {
            return Ok(false);
        }
        let delta = difference(own_position, target.position);
        let distance = length(delta);
        if distance <= t.physical_size * 1.5 {
            return Ok(false);
        }
        let direction = normalize(delta);
        let acceleration = t.drag_accel * (distance / t.length);
        if !acceleration.is_finite() {
            return Err(HookError::InvalidInput);
        }
        let target_x = saturated_add(
            -t.drag_speed, t.drag_speed, target.velocity.x,
            acceleration * direction.x * 1.5,
        );
        let target_y = saturated_add(
            -t.drag_speed, t.drag_speed, target.velocity.y,
            acceleration * direction.y * 1.5,
        );
        let own_x = saturated_add(
            -t.drag_speed, t.drag_speed, own_velocity.x,
            -acceleration * direction.x * 0.25,
        );
        let own_y = saturated_add(
            -t.drag_speed, t.drag_speed, own_velocity.y,
            -acceleration * direction.y * 0.25,
        );
        target.velocity = Vec2::new(target_x, target_y);
        *own_velocity = Vec2::new(own_x, own_y);
        Ok(true)
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

    fn peer(id: u8, x: f32, y: f32) -> HookPeer {
        HookPeer {
            id, position: Vec2::new(x, y), velocity: Vec2::default(),
            can_hook: true, can_keep_hook: true,
        }
    }

    #[test]
    fn nearest_eligible_player_is_selected_before_ground() {
        let mut hook = Hook::default();
        let mut vel = Vec2::default();
        let start = Vec2::new(100.0, 80.0);
        let mut bytes = vec![0u8; 20 * 6 * 4];
        for y in 0..6 { bytes[(y * 20 + 7) * 4] = 1; }
        let map = GameMap::from_tile_bytes(20, 6, &bytes).unwrap();
        let controls = HookInput {
            held: true, aim: Vec2::new(1.0, 0.0), movement: 0,
        };
        // First tick travels from 142 to 222. A player near 200 is
        // caught even though the wall begins at x=224.
        let peers = [peer(7, 210.0, 80.0), peer(2, 180.0, 80.0)];
        let result = hook.tick_with_players(
            controls, start, &mut vel, &map, HookTuning::default(), &peers,
        );
        assert_eq!(result, Ok(HookEvent::AttachPlayer));
        assert_eq!(hook.state, HookState::Grabbed);
        assert_eq!(hook.hooked_player, Some(2));
        assert_eq!(hook.position, peers[1].position);
        assert_eq!(vel, Vec2::default());
        // The reference client keeps the same target while eligible
        // and follows its latest position, without ground-hook drag.
        let moved = [peer(2, 190.0, 78.0)];
        assert_eq!(hook.tick_with_players(
            controls, start, &mut vel, &map, HookTuning::default(), &moved,
        ), Ok(HookEvent::None));
        assert_eq!(hook.position, moved[0].position);
        assert_eq!(hook.hooked_player, Some(2));
    }

    #[test]
    fn blocked_peers_do_not_capture_and_player_hooking_can_be_disabled() {
        let map = wall(1);
        let start = Vec2::new(100.0, 80.0);
        let control = HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 0 };
        let mut velocity = Vec2::default();
        let blocked = HookPeer { can_hook: false, ..peer(3, 210.0, 80.0) };
        let mut hook = Hook::default();
        assert_eq!(hook.tick_with_players(
            control, start, &mut velocity, &map, HookTuning::default(), &[blocked],
        ), Ok(HookEvent::Launch));
        assert_eq!(hook.hooked_player, None);
        // Second step collides with the wall normally.
        assert_eq!(hook.tick_with_players(
            control, start, &mut velocity, &map, HookTuning::default(), &[blocked],
        ), Ok(HookEvent::AttachGround));

        let mut disabled = Hook::default();
        let tune = HookTuning { player_hooking: false, ..HookTuning::default() };
        disabled.tick_with_players(control, start, &mut velocity, &map, tune,
            &[peer(3, 180.0, 80.0)]).unwrap();
        assert_eq!(disabled.hooked_player, None);
        assert_eq!(disabled.state, HookState::Flying);
    }

    #[test]
    fn player_hook_releases_if_target_disappears_or_team_revokes_permission() {
        let map = wall(0);
        let start = Vec2::new(100.0, 80.0);
        let control = HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 0 };
        let mut velocity = Vec2::default();
        let mut hook = Hook::default();
        hook.tick_with_players(control, start, &mut velocity, &map,
            HookTuning::default(), &[peer(4, 200.0, 80.0)]).unwrap();
        assert_eq!(hook.hooked_player, Some(4));
        assert_eq!(hook.tick_with_players(control, start, &mut velocity,
            &map, HookTuning::default(), &[]), Ok(HookEvent::None));
        assert_eq!(hook.state, HookState::Retracted);
        assert_eq!(hook.hooked_player, None);
        assert_eq!(hook.position, start);

        hook.tick_with_players(HookInput { held: false, ..control },
            start, &mut velocity, &map, HookTuning::default(), &[]).unwrap();
        hook.tick_with_players(control, start, &mut velocity, &map,
            HookTuning::default(), &[peer(4, 200.0, 80.0)]).unwrap();
        let revoked = HookPeer { can_keep_hook: false, ..peer(4, 210.0, 80.0) };
        hook.tick_with_players(control, start, &mut velocity, &map,
            HookTuning::default(), &[revoked]).unwrap();
        assert_eq!(hook.state, HookState::Retracted);
        assert_eq!(hook.hooked_player, None);
    }

    #[test]
    fn player_hook_expires_after_sixty_ticks_but_wall_hook_does_not() {
        let map = wall(0);
        let start = Vec2::new(100.0, 80.0);
        let control = HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 0 };
        let mut velocity = Vec2::default();
        let target = [peer(1, 180.0, 80.0)];
        let mut hook = Hook::default();
        hook.tick_with_players(control, start, &mut velocity,
            &map, HookTuning::default(), &target).unwrap();
        assert_eq!(hook.hooked_player, Some(1));
        for _ in 0..59 {
            hook.tick_with_players(control, start, &mut velocity,
                &map, HookTuning::default(), &target).unwrap();
        }
        assert_eq!(hook.tick, 60.0);
        assert_eq!(hook.state, HookState::Grabbed);
        hook.tick_with_players(control, start, &mut velocity,
            &map, HookTuning::default(), &target).unwrap();
        assert_eq!(hook.state, HookState::Retracted);
        assert_eq!(hook.hooked_player, None);

        // Distinct rule: the 60 tick limit applies to a grabbed player,
        // not a ground tile.
        let mut wall_hook = Hook::default();
        wall_hook.state = HookState::Grabbed;
        wall_hook.position = Vec2::new(250.0, 80.0);
        for _ in 0..100 {
            wall_hook.tick(control, start, &mut velocity, &map, HookTuning::default()).unwrap();
        }
        assert_eq!(wall_hook.state, HookState::Grabbed);
        assert_eq!(wall_hook.hooked_player, None);
    }

    #[test]
    fn deferred_hook_force_pulls_both_tees_with_saturated_component_speeds() {
        let mut hook = Hook::default();
        hook.state = HookState::Grabbed;
        hook.hooked_player = Some(3);
        let mut target = peer(3, 250.0, 80.0);
        let mut own_velocity = Vec2::default();
        assert_eq!(hook.apply_player_drag(
            Vec2::new(100.0, 80.0), &mut own_velocity,
            &mut target, HookTuning::default(),
        ), Ok(true));
        assert!((target.velocity.x + 1.7763158).abs() < 0.0001);
        assert!((own_velocity.x - 0.29605263).abs() < 0.0001);
        assert_eq!(target.velocity.y, 0.0);
        let another = &mut peer(5, 250.0, 80.0);
        let saved = own_velocity;
        assert_eq!(hook.apply_player_drag(Vec2::new(100.0, 80.0),
            &mut own_velocity, another, HookTuning::default()), Ok(false));
        assert_eq!(own_velocity, saved);

        let mut adjacent = peer(3, 115.0, 80.0);
        assert_eq!(hook.apply_player_drag(Vec2::new(100.0, 80.0),
            &mut own_velocity, &mut adjacent, HookTuning::default()), Ok(false));
    }

    #[test]
    fn malformed_peers_are_rejected_without_mutating_hook() {
        let map = wall(0);
        let mut hook = Hook::default();
        let before = hook;
        let mut velocity = Vec2::default();
        let control = HookInput { held: true, aim: Vec2::new(1.0, 0.0), movement: 0 };
        assert_eq!(hook.tick_with_players(control, Vec2::new(100.0, 80.0),
            &mut velocity, &map, HookTuning::default(),
            &[peer(4, 190.0, 80.0), peer(4, 210.0, 80.0)]),
            Err(HookError::InvalidPeers));
        assert_eq!(hook, before);
        let bad = HookPeer { position: Vec2::new(f32::NAN, 80.0), ..peer(3, 190.0, 80.0) };
        assert_eq!(hook.tick_with_players(control, Vec2::new(100.0, 80.0),
            &mut velocity, &map, HookTuning::default(), &[bad]),
            Err(HookError::InvalidPeers));
        assert_eq!(hook, before);
        let overflow = vec![peer(0, 200.0, 80.0); 65];
        assert_eq!(hook.tick_with_players(control, Vec2::new(100.0, 80.0),
            &mut velocity, &map, HookTuning::default(), &overflow),
            Err(HookError::InvalidPeers));
        assert_eq!(hook, before);
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
