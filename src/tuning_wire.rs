//! DDNet 20.2.1 0.6 Sv_TuneParams (47 CTuneParam values).
//! Wire values are integers in hundredths, in tuning.h declaration order.
//! The packet carries no zone identifier.
use crate::game_protocol::MessageError;
use crate::hook::HookTuning;
use crate::physics::Tuning;
use crate::protocol::Reader;

pub const SV_TUNEPARAMS: u16 = 6;
pub const TUNE_PARAM_COUNT: usize = 47;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerTuning {
    pub values: [i32; TUNE_PARAM_COUNT],
}

impl ServerTuning {
    pub fn decode(payload: &[u8]) -> Result<Self, MessageError> {
        let mut reader = Reader::new(payload);
        let mut values = [0; TUNE_PARAM_COUNT];
        for value in &mut values { *value = reader.read_int()?; }
        if reader.remaining() != 0 {
            return Err(MessageError::InvalidMessage);
        }
        Ok(Self { values })
    }

    /// Extract only the values implemented by the present Rust physics.
    /// Invalid or extreme parameters must not reach the prediction engine.
    pub fn simulation_params(&self) -> Option<(Tuning, HookTuning)> {
        let v = |i: usize| self.values[i] as f32 / 100.0;
        let movement = Tuning {
            ground_control_speed: v(0), ground_control_accel: v(1),
            ground_friction: v(2), ground_jump_impulse: v(3),
            air_jump_impulse: v(4), air_control_speed: v(5),
            air_control_accel: v(6), air_friction: v(7),
            gravity: v(12), velramp_start: v(13),
            velramp_range: v(14), velramp_curvature: v(15),
            ground_elasticity_x: v(45), ground_elasticity_y: v(46),
        };
        let hook = HookTuning {
            length: v(8), fire_speed: v(9), drag_accel: v(10),
            drag_speed: v(11), duration: v(37),
            physical_size: 28.0, player_hooking: self.values[32] != 0,
        };
        let values = [
            movement.ground_control_speed, movement.ground_control_accel,
            movement.ground_friction, movement.ground_jump_impulse,
            movement.air_jump_impulse, movement.air_control_speed,
            movement.air_control_accel, movement.air_friction, movement.gravity,
            movement.velramp_start, movement.velramp_range,
            movement.velramp_curvature, movement.ground_elasticity_x,
            movement.ground_elasticity_y, hook.length, hook.fire_speed,
            hook.drag_accel, hook.drag_speed, hook.duration,
        ];
        if values.iter().any(|x| !x.is_finite() || x.abs() > 20_000.0)
            || movement.ground_control_speed < 0.0
            || movement.ground_control_accel < 0.0
            || movement.ground_friction < 0.0
            || movement.air_control_speed < 0.0
            || movement.air_control_accel < 0.0
            || movement.air_friction < 0.0
            || movement.velramp_start < 0.0
            || movement.velramp_range <= 0.0
            || movement.velramp_curvature <= 0.0
            || hook.length <= 0.0 || hook.length > 8192.0
            || hook.fire_speed < 0.0 || hook.fire_speed > 8192.0
            || hook.drag_accel < 0.0 || hook.drag_speed <= 0.0
            || hook.duration < 0.0
        {
            return None;
        }
        Some((movement, hook))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::write_int;

    pub(crate) fn vanilla_values() -> [i32; TUNE_PARAM_COUNT] {
        let mut values = [100i32; TUNE_PARAM_COUNT];
        for (i, v) in [
            (0,1000),(1,200),(2,50),(3,1320),(4,1200),
            (5,500),(6,150),(7,95),(8,38000),(9,8000),
            (10,300),(11,1500),(12,50),(13,55000),
            (14,200000),(15,140),(31,100),(32,100),
            (37,125),(45,0),(46,0),
        ] { values[i] = v; }
        values
    }

    #[test]
    fn parses_all_47_fixed_point_values_like_ddnet_cpp() {
        let values = vanilla_values();
        let mut bytes = Vec::new();
        for value in values { write_int(value, &mut bytes); }
        let tuning = ServerTuning::decode(&bytes).unwrap();
        assert_eq!(tuning.values, vanilla_values());
        assert_eq!(tuning.simulation_params(),
            Some((Tuning::default(), HookTuning::default())));
    }

    #[test]
    fn truncated_extra_and_malformed_tuning_fails_closed() {
        let mut bytes = Vec::new();
        for value in vanilla_values() { write_int(value, &mut bytes); }
        assert!(ServerTuning::decode(&bytes[..bytes.len()-1]).is_err());
        bytes.push(0);
        assert_eq!(ServerTuning::decode(&bytes), Err(MessageError::InvalidMessage));
        let mut values = vanilla_values();
        values[14] = 0;
        assert!(ServerTuning { values }.simulation_params().is_none());
        values[14] = 200000; values[8] = -1;
        assert!(ServerTuning { values }.simulation_params().is_none());
        values[8] = 38000; values[12] = i32::MAX;
        assert!(ServerTuning { values }.simulation_params().is_none());
    }
}
