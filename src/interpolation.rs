//! Diagnostic smoothing between decoded DDNet network snapshots.
//!
//! The official DDNet client has richer interpolation/prediction; this module
//! intentionally smooths only compatible character and hook coordinates.
//! Teleports, missing references and discontinuities snap immediately.

use crate::snapshot_delta::SnapshotState;

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
}
