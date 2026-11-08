//! Diagnostic smoothing between decoded DDNet network snapshots.
//!
//! The official DDNet client has richer interpolation/prediction; this module
//! intentionally smooths only compatible character and hook coordinates.
//! Teleports, missing references and discontinuities snap immediately.

use std::time::Duration;
use crate::snapshot_delta::SnapshotState;

/// DDNet's network game ticks run at 50Hz. A snapshot's tick distance,
/// not window render FPS or an assumed one-tick packet period, determines
/// how long it should be interpolated after arrival.
pub fn online_interpolation_fraction(
    previous_tick: i32, current_tick: i32, elapsed: Duration,
) -> f32 {
    let Some(tick_delta) = current_tick.checked_sub(previous_tick) else {
        return 1.0;
    };
    if tick_delta <= 0 { return 1.0; }
    let span_seconds = f64::from(tick_delta) / 50.0;
    (elapsed.as_secs_f64() / span_seconds).clamp(0.0, 1.0) as f32
}

/// Frame-time invariant exponential camera easing. At 60 FPS this matches
/// the old 0.3-per-frame response, but at low or high FPS it follows the
/// same motion over real time.
pub fn camera_follow_fraction(frame_duration: Duration) -> f32 {
    let frame_seconds = frame_duration.as_secs_f32().min(0.25);
    1.0 - 0.7f32.powf(frame_seconds * 60.0)
}


pub const MAX_SMOOTH_DISTANCE: i64 = 256;

fn lerp_coordinate(previous: i32, current: i32, t: f64) -> i32 {
    (f64::from(previous) + f64::from(current - previous) * t).round() as i32
}

/// Produce a display-only snapshot. Gameplay prediction and the stored
/// network snapshot must always use their untouched authoritative values.
pub fn interpolate_snapshots(
    previous: &SnapshotState,
    current: &SnapshotState,
    progress: f32,
) -> SnapshotState {
    let t = if progress.is_finite() {
        progress.clamp(0.0, 1.0) as f64
    } else {
        1.0
    };
    if t >= 1.0 {
        return current.clone();
    }
    let mut display = current.clone();
    for item in &mut display.items {
        if !matches!(item.type_id, 8 | 9) || item.words.len() < 15 {
            continue;
        }
        let Some(old) = previous.find(item.type_id, item.id) else {
            continue;
        };
        if old.words.len() != item.words.len() {
            continue;
        }

        // Use i64 differences to avoid overflow when decoding adversarial
        // signed 32-bit snapshot coordinates.
        let dx = i64::from(item.words[1]) - i64::from(old.words[1]);
        let dy = i64::from(item.words[2]) - i64::from(old.words[2]);
        if dx.abs() > MAX_SMOOTH_DISTANCE || dy.abs() > MAX_SMOOTH_DISTANCE {
            continue;
        }
        for index in [1usize, 2] {
            item.words[index] = lerp_coordinate(old.words[index], item.words[index], t);
        }

        // Only interpolate hook when it is attached/flying in both snapshots.
        // Never draw a hook line across a grab/release transition.
        if old.words[9] > 0 && item.words[9] == old.words[9] {
            let hook_dx = i64::from(item.words[11]) - i64::from(old.words[11]);
            let hook_dy = i64::from(item.words[12]) - i64::from(old.words[12]);
            if hook_dx.abs() <= MAX_SMOOTH_DISTANCE
                && hook_dy.abs() <= MAX_SMOOTH_DISTANCE
            {
                for index in [11usize, 12] {
                    item.words[index] = lerp_coordinate(
                        old.words[index], item.words[index], t,
                    );
                }
            }
        }
    }
    display
}


/// Interpolate recorded frames for presentation only. Unlike live snapshot
/// interpolation, a replay must keep the *earlier* frame's object inventory
/// until the next tick: pickups, spawns and despawns must not appear early.
/// Missing characters and jumps over 256px remain unchanged until the tick.
pub fn interpolate_replay_snapshots(
    earlier: &SnapshotState,
    later: &SnapshotState,
    progress: f32,
) -> SnapshotState {
    let t = if progress.is_finite() {
        progress.clamp(0.0, 1.0) as f64
    } else {
        0.0
    };
    if t >= 1.0 { return later.clone(); }
    let mut display = earlier.clone();
    for item in &mut display.items {
        if !matches!(item.type_id, 8 | 9) || item.words.len() < 15 {
            continue;
        }
        let Some(next) = later.find(item.type_id, item.id) else {
            continue;
        };
        if next.words.len() != item.words.len() { continue; }
        let dx = i64::from(next.words[1]) - i64::from(item.words[1]);
        let dy = i64::from(next.words[2]) - i64::from(item.words[2]);
        if dx.abs() > MAX_SMOOTH_DISTANCE || dy.abs() > MAX_SMOOTH_DISTANCE {
            continue;
        }
        for index in [1usize, 2] {
            item.words[index] = lerp_coordinate(item.words[index], next.words[index], t);
        }
        if item.words[9] > 0 && item.words[9] == next.words[9] {
            let hook_dx = i64::from(next.words[11]) - i64::from(item.words[11]);
            let hook_dy = i64::from(next.words[12]) - i64::from(item.words[12]);
            if hook_dx.abs() <= MAX_SMOOTH_DISTANCE
                && hook_dy.abs() <= MAX_SMOOTH_DISTANCE
            {
                for index in [11usize, 12] {
                    item.words[index] = lerp_coordinate(item.words[index], next.words[index], t);
                }
            }
        }
    }
    display
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_delta::SnapshotObject;

    fn snap(id: u16, x: i32, y: i32, hook_state: i32, hook_x: i32) -> SnapshotState {
        let mut words = vec![0; 22];
        words[1] = x;
        words[2] = y;
        words[9] = hook_state;
        words[11] = hook_x;
        SnapshotState { items: vec![SnapshotObject {
            type_id: 9, id, words,
        }] }
    }

    #[test]
    fn network_interpolation_uses_actual_server_tick_spacing() {
        assert!((online_interpolation_fraction(
            100, 102, Duration::from_millis(20)
        ) - 0.5).abs() < 1.0e-6);
        assert_eq!(online_interpolation_fraction(
            100, 102, Duration::from_millis(40)
        ), 1.0);
        assert_eq!(online_interpolation_fraction(
            200, 198, Duration::from_millis(1)
        ), 1.0);
        assert_eq!(online_interpolation_fraction(
            300, 300, Duration::ZERO
        ), 1.0);
        assert_eq!(online_interpolation_fraction(
            100, 101, Duration::ZERO
        ), 0.0);
    }

    #[test]
    fn camera_easing_is_independent_of_render_fps() {
        let full_frame = camera_follow_fraction(Duration::from_millis(100));
        let half_frame = camera_follow_fraction(Duration::from_millis(50));
        let composed = 1.0 - (1.0 - half_frame) * (1.0 - half_frame);
        assert!((full_frame - composed).abs() < 1.0e-6);
        assert_eq!(camera_follow_fraction(Duration::ZERO), 0.0);
        // Frame stalls are intentionally capped at 250 ms so recovery
        // does not teleport the camera after a long window pause.
        assert_eq!(
            camera_follow_fraction(Duration::from_secs(1)),
            camera_follow_fraction(Duration::from_millis(250)),
        );
    }

    #[test]
    fn character_and_hook_positions_are_smoothed_without_mutating_source() {
        let first = snap(2, 10, 20, 5, 100);
        let last = snap(2, 30, 60, 5, 120);
        let middle = interpolate_snapshots(&first, &last, 0.5);
        let obj = middle.find(9, 2).unwrap();
        assert_eq!((obj.words[1], obj.words[2], obj.words[11]), (20, 40, 110));
        assert_eq!(first.find(9, 2).unwrap().words[1], 10);
        assert_eq!(last.find(9, 2).unwrap().words[1], 30);
        assert_eq!(interpolate_snapshots(&first, &last, 0.0).find(9, 2).unwrap().words[1], 10);
        assert_eq!(interpolate_snapshots(&first, &last, 1.0), last);
    }

    #[test]
    fn teleport_discontinuity_and_hook_state_changes_snap_instantly() {
        let old = snap(2, 10, 20, 5, 100);
        let jumped = snap(2, 5000, 50, 5, 110);
        assert_eq!(interpolate_snapshots(&old, &jumped, 0.1), jumped);
        let released = snap(2, 30, 20, 0, 110);
        let render = interpolate_snapshots(&old, &released, 0.5);
        assert_eq!(render.find(9, 2).unwrap().words[1], 20);
        assert_eq!(render.find(9, 2).unwrap().words[11], 110);
    }

    #[test]
    fn invalid_fraction_missing_reference_and_i32_extremes_are_safe() {
        let old = snap(1, i32::MIN, 0, 0, 0);
        let new = snap(1, i32::MAX, 0, 0, 0);
        assert_eq!(interpolate_snapshots(&old, &new, 0.3), new);
        assert_eq!(interpolate_snapshots(&old, &new, f32::NAN), new);
        assert_eq!(interpolate_snapshots(&SnapshotState::default(), &new, 0.1), new);
        let other_id = snap(2, 42, 30, 0, 0);
        assert_eq!(interpolate_snapshots(&old, &other_id, 0.5), other_id);
    }
    #[test]
    fn demo_interpolation_does_not_anticipate_object_creation_or_deletion() {
        let mut old = snap(2, 100, 200, 5, 130);
        old.items.push(snap(4, 30, 30, 0, 0).items.remove(0));
        let mut new = snap(2, 120, 220, 5, 150);
        new.items.push(snap(3, 300, 300, 0, 0).items.remove(0));
        let halfway = interpolate_replay_snapshots(&old, &new, 0.5);
        let tee = halfway.find(9, 2).unwrap();
        assert_eq!((tee.words[1], tee.words[2], tee.words[11]), (110, 210, 140));
        assert!(halfway.find(9, 3).is_none());
        assert!(halfway.find(9, 4).is_some());
        assert_eq!(interpolate_replay_snapshots(&old, &new, 1.0), new);
        assert_eq!(old.find(9, 2).unwrap().words[1], 100);
    }

    #[test]
    fn demo_interpolation_holds_teleport_and_invalid_fraction() {
        let old = snap(2, 0, 0, 5, 20);
        let teleported = snap(2, 10000, 0, 5, 25);
        assert_eq!(interpolate_replay_snapshots(&old, &teleported, 0.9), old);
        assert_eq!(interpolate_replay_snapshots(&old, &teleported, f32::NAN), old);
        let unhooked = snap(2, 40, 0, 0, 400);
        let middle = interpolate_replay_snapshots(&old, &unhooked, 0.5);
        assert_eq!(middle.find(9, 2).unwrap().words[1], 20);
        assert_eq!(middle.find(9, 2).unwrap().words[11], 20);
    }

}
