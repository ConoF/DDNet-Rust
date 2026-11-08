//! DDNet 20.2.1 map envelope decoding and timing.
//! Source: game/mapitems.h (CMapItemEnvelope, CEnvPoint) and
//! game/map/render_map.cpp (RenderEvalEnvelope).
//! In original Teeworlds v3 maps, Bezier handles are embedded in the 88-byte
//! envpoint. DDNet's separate UUID-based Bezier extension is not decoded yet.

use crate::datafile::Datafile;
use crate::map_visual::VisualError;

const MAP_ENVELOPE: u16 = 3;
const MAP_ENVPOINTS: u16 = 6;
const MAX_ENVELOPES: usize = 2048;
const MAX_POINTS: usize = 65536;
const ENVPOINT_SIZE: usize = 24;
const ENVPOINT_BEZIER_SIZE: usize = 88;
const FIXED_SCALE: f32 = 1024.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnvelopeTimes {
    /// Time from the server's 50-Hz tick clock (milliseconds).
    pub game_ms: f64,
    /// Time since loading the current map (milliseconds).
    pub local_ms: f64,
}

impl EnvelopeTimes {
    pub fn from_tick(tick: f64) -> Self {
        Self { game_ms: tick * 20.0, local_ms: tick * 20.0 }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BezierHandle {
    pub in_dx: [i32; 4],
    pub in_dy: [i32; 4],
    pub out_dx: [i32; 4],
    pub out_dy: [i32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvelopePoint {
    pub time_ms: i32,
    pub curve: i32,
    pub values: [i32; 4],
    pub bezier: Option<BezierHandle>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub channels: usize,
    pub synchronized: bool,
    pub points: Vec<EnvelopePoint>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnvelopeSet {
    pub envelopes: Vec<Envelope>,
}

fn word(src: &[u8], index: usize) -> Result<i32, VisualError> {
    let data: [u8; 4] = src.get(index..index + 4)
        .ok_or(VisualError::InvalidMap)?
        .try_into().map_err(|_| VisualError::InvalidMap)?;
    Ok(i32::from_le_bytes(data))
}

fn parse_point(src: &[u8], extended: bool) -> Result<EnvelopePoint, VisualError> {
    let time_ms = word(src, 0)?;
    if time_ms < 0 { return Err(VisualError::InvalidMap); }
    let mut values = [0; 4];
    for (index, value) in values.iter_mut().enumerate() {
        *value = word(src, 8 + index * 4)?;
    }
    let bezier = if extended {
        let mut b = BezierHandle::default();
        for index in 0..4 {
            b.in_dx[index] = word(src, 24 + index * 4)?;
            b.in_dy[index] = word(src, 40 + index * 4)?;
            b.out_dx[index] = word(src, 56 + index * 4)?;
            b.out_dy[index] = word(src, 72 + index * 4)?;
        }
        Some(b)
    } else { None };
    Ok(EnvelopePoint { time_ms, curve: word(src, 4)?, values, bezier })
}

impl EnvelopeSet {
    pub fn from_datafile(file: &Datafile<'_>) -> Result<Self, VisualError> {
        let items = file.items.iter().filter(|item| item.type_id == MAP_ENVELOPE)
            .collect::<Vec<_>>();
        if items.len() > MAX_ENVELOPES { return Err(VisualError::TooLarge); }
        if items.is_empty() { return Ok(Self::default()); }

        let extended = items.iter().any(|item| word(item.payload, 0)
            .is_ok_and(|version| version >= 3));
        let stride = if extended { ENVPOINT_BEZIER_SIZE } else { ENVPOINT_SIZE };
        // Envelope point arrays are map items, not raw data blocks. They are
        // normally stored as a single item, but concatenating by ID handles
        // maps with more than one while preserving their logical point order.
        let mut point_items = file.items.iter()
            .filter(|item| item.type_id == MAP_ENVPOINTS).collect::<Vec<_>>();
        point_items.sort_by_key(|item| item.id);
        let mut points = Vec::new();
        for item in point_items {
            if !item.payload.len().is_multiple_of(stride) {
                return Err(VisualError::InvalidMap);
            }
            let count = item.payload.len() / stride;
            if count > MAX_POINTS.saturating_sub(points.len()) {
                return Err(VisualError::TooLarge);
            }
            for part in item.payload.chunks_exact(stride) {
                points.push(parse_point(part, extended)?);
            }
        }

        let mut envelopes = Vec::with_capacity(items.len());
        for item in items {
            let raw = item.payload;
            if raw.len() < 48 { return Err(VisualError::InvalidMap); }
            let version = word(raw, 0)?;
            if !(1..=3).contains(&version) { return Err(VisualError::InvalidMap); }
            let channels = usize::try_from(word(raw, 4)?)
                .map_err(|_| VisualError::InvalidMap)?;
            if !(1..=4).contains(&channels) { return Err(VisualError::InvalidMap); }
            let start = usize::try_from(word(raw, 8)?)
                .map_err(|_| VisualError::InvalidMap)?;
            let count = usize::try_from(word(raw, 12)?)
                .map_err(|_| VisualError::InvalidMap)?;
            let end = start.checked_add(count).ok_or(VisualError::TooLarge)?;
            if count == 0 || end > points.len() {
                return Err(VisualError::InvalidMap);
            }
            let selection = &points[start..end];
            if selection.windows(2).any(|pair| pair[1].time_ms < pair[0].time_ms) {
                return Err(VisualError::InvalidMap);
            }
            let synchronized = if version >= 2 {
                raw.len() >= 52 && word(raw, 48)? != 0
            } else { false };
            envelopes.push(Envelope {
                channels, synchronized, points: selection.to_vec(),
            });
        }
        Ok(Self { envelopes })
    }

    /// Evaluate one envelope. A missing envelope retains the caller-supplied
    /// default (position: zero, color: one), as in upstream render_map.cpp.
    pub fn evaluate(
        &self, index: i32, offset_ms: i32, times: EnvelopeTimes,
        default: [f32; 4],
    ) -> [f32; 4] {
        let Ok(index) = usize::try_from(index) else { return default };
        let Some(env) = self.envelopes.get(index) else { return default };
        env.evaluate(offset_ms, times)
    }
}

fn bezier(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let inv = 1.0 - t;
    inv * inv * inv * p0 + 3.0 * inv * inv * t * p1
        + 3.0 * inv * t * t * p2 + t * t * t * p3
}

impl Envelope {
    pub fn evaluate(&self, offset_ms: i32, times: EnvelopeTimes) -> [f32; 4] {
        let mut result = [0.0f32; 4];
        if self.points.is_empty() { return result; }
        if self.points.len() == 1 {
            for (index, value) in result.iter_mut().enumerate().take(self.channels) {
                *value = self.points[0].values[index] as f32 / FIXED_SCALE;
            }
            return result;
        }
        let last = self.points.last().expect("non-empty");
        let max_ms = f64::from(last.time_ms);
        let time_ms = if self.synchronized { times.game_ms } else { times.local_ms };
        let time = if time_ms.is_finite() && max_ms > 0.0 {
            (time_ms + f64::from(offset_ms)).rem_euclid(max_ms)
        } else { 0.0 };
        let next_index = self.points.partition_point(|point|
            f64::from(point.time_ms) <= time);
        let (start, end) = if next_index == 0 {
            // Upstream leaves the last sample in case the time is outside
            // defined points, e.g. the first sample starts after t=0.
            (self.points.len() - 1, self.points.len() - 1)
        } else if next_index >= self.points.len() {
            (self.points.len() - 1, self.points.len() - 1)
        } else {
            (next_index - 1, next_index)
        };
        let a = &self.points[start];
        let b = &self.points[end];
        let delta = f64::from(b.time_ms - a.time_ms);
        let fraction = if delta > 0.0 {
            ((time - f64::from(a.time_ms)) / delta).clamp(0.0, 1.0)
        } else { 0.0 };
        let fraction = match a.curve {
            0 => 0.0, // STEP
            2 => fraction.powi(3), // SLOW
            3 => 1.0 - (1.0 - fraction).powi(3), // FAST
            4 => fraction * fraction * (3.0 - 2.0 * fraction), // SMOOTH
            _ => fraction, // LINEAR and unknown
        };
        for (channel, value) in result.iter_mut().enumerate().take(self.channels) {
            let v0 = f64::from(a.values[channel]) / f64::from(FIXED_SCALE);
            let v1 = f64::from(b.values[channel]) / f64::from(FIXED_SCALE);
            let sampled = if a.curve == 5 {
                if let (Some(from), Some(to)) = (a.bezier, b.bezier) {
                    let t0 = f64::from(a.time_ms);
                    let t3 = f64::from(b.time_ms);
                    let t1 = (t0 + f64::from(from.out_dx[channel])).clamp(t0, t3);
                    let t2 = (t3 + f64::from(to.in_dx[channel])).clamp(t0, t3);
                    let mut low = 0.0;
                    let mut high = 1.0;
                    // Monotonic curve: bounded binary search for x(t) = time.
                    for _ in 0..28 {
                        let middle = (low + high) / 2.0;
                        if bezier(t0, t1, t2, t3, middle) < time {
                            low = middle;
                        } else {
                            high = middle;
                        }
                    }
                    let parameter = (low + high) / 2.0;
                    let p1 = v0 + f64::from(from.out_dy[channel]) / f64::from(FIXED_SCALE);
                    let p2 = v1 + f64::from(to.in_dy[channel]) / f64::from(FIXED_SCALE);
                    bezier(v0, p1, p2, v1, parameter)
                } else {
                    v0 + (v1 - v0) * fraction
                }
            } else {
                v0 + (v1 - v0) * fraction
            };
            *value = sampled as f32;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datafile::Item;

    fn words(input: &[i32]) -> Vec<u8> {
        input.iter().flat_map(|value| value.to_le_bytes()).collect()
    }

    fn point(ms: i32, curve: i32, value: i32) -> [i32; 6] {
        [ms, curve, value, value, value, value]
    }

    fn fixture(version: i32, synchronized: bool, curve: i32) -> EnvelopeSet {
        let payload = words(&[
            version, 4, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0,
            i32::from(synchronized),
        ]);
        let points = words(&[
            point(0, curve, 0), point(1000, 1, 1024),
        ].concat());
        let file = Datafile {
            version: 3,
            items: vec![
                Item { type_id: MAP_ENVELOPE, id: 0, payload: &payload },
                Item { type_id: MAP_ENVPOINTS, id: 0, payload: &points },
            ],
            item_types: vec![], raw_blocks: vec![],
        };
        EnvelopeSet::from_datafile(&file).unwrap()
    }

    #[test]
    fn file_decoding_and_linear_repeat_use_ddnet_fixed_point() {
        let list = fixture(2, true, 1);
        assert_eq!(list.envelopes.len(), 1);
        assert_eq!(list.envelopes[0].channels, 4);
        let at = EnvelopeTimes { game_ms: 250.0, local_ms: 800.0 };
        let value = list.evaluate(0, 0, at, [1.0; 4]);
        assert!((value[0] - 0.25).abs() < 0.00001);
        assert_eq!(list.evaluate(0, 0, EnvelopeTimes {
            game_ms: 1250.0, local_ms: 0.0
        }, [1.0; 4])[0], value[0]);
        assert_eq!(list.evaluate(-1, 0, at, [1.0; 4]), [1.0; 4]);
        assert!((list.evaluate(0, 250, at, [0.0; 4])[0] - 0.5).abs() < 0.00001);
    }

    #[test]
    fn unsynchronized_envelopes_use_local_clock() {
        let list = fixture(2, false, 1);
        let result = list.evaluate(0, 0,
            EnvelopeTimes { game_ms: 20.0, local_ms: 750.0 }, [1.0; 4]);
        assert_eq!(result, [0.75; 4]);
    }

    #[test]
    fn step_slow_fast_smooth_curve_shapes() {
        let at = EnvelopeTimes::from_tick(12.5);

        assert_eq!(fixture(2, true, 0).evaluate(0, 0, at, [1.0; 4])[0], 0.0);
        let slow = fixture(2, true, 2).evaluate(0, 0, at, [1.0; 4])[0];
        let fast = fixture(2, true, 3).evaluate(0, 0, at, [1.0; 4])[0];
        let smooth = fixture(2, true, 4).evaluate(0, 0, at, [1.0; 4])[0];
        assert!((slow - 0.015625).abs() < 1e-5);
        assert!((fast - 0.578125).abs() < 1e-5);
        assert!((smooth - 0.15625).abs() < 1e-5);

    }

    #[test]
    fn teeworlds_v3_embedded_bezier_handles_are_interpolated() {
        let envelope = words(&[
            3, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ]);
        let mut points = vec![0u8; ENVPOINT_BEZIER_SIZE * 2];
        // First envelope point: time zero, Bezier curve, value zero.
        points[4..8].copy_from_slice(&5i32.to_le_bytes());
        // m_aOutTangentDeltaX[0] = +333 ms.
        points[56..60].copy_from_slice(&333i32.to_le_bytes());
        // Second point: time 1000 ms, value 1.0 in 22.10.
        let next = ENVPOINT_BEZIER_SIZE;
        points[next..next + 4].copy_from_slice(&1000i32.to_le_bytes());
        points[next + 8..next + 12].copy_from_slice(&1024i32.to_le_bytes());
        // m_aInTangentDeltaX[0] = -333 ms.
        points[next + 24..next + 28].copy_from_slice(&(-333i32).to_le_bytes());
        let file = Datafile {
            version: 4,
            items: vec![
                Item { type_id: MAP_ENVELOPE, id: 0, payload: &envelope },
                Item { type_id: MAP_ENVPOINTS, id: 0, payload: &points },
            ],
            item_types: vec![], raw_blocks: vec![],
        };
        let parsed = EnvelopeSet::from_datafile(&file).unwrap();
        let at_250 = parsed.evaluate(0, 0,
            EnvelopeTimes { game_ms: 250.0, local_ms: 0.0 }, [0.0; 4]);
        assert!(at_250[0] > 0.12 && at_250[0] < 0.20,
            "Bezier interpolation should differ from linear: {}", at_250[0]);
        let at_500 = parsed.evaluate(0, 0,
            EnvelopeTimes { game_ms: 500.0, local_ms: 0.0 }, [0.0; 4]);
        assert!((at_500[0] - 0.5).abs() < 0.005);
    }

    #[test]
    fn rejects_missing_out_of_range_and_non_monotonic_points() {
        let payload = words(&[2, 4, 0, 3, 0,0,0,0,0,0,0,0,0]);
        let points = words(&[point(0, 1, 0), point(200, 1, 100),
            point(50, 1, 200)].concat());
        let file = Datafile {
            version: 3, items: vec![
                Item { type_id: MAP_ENVELOPE, id: 0, payload: &payload },
                Item { type_id: MAP_ENVPOINTS, id: 0, payload: &points },
            ], item_types: vec![], raw_blocks: vec![],
        };
        assert_eq!(EnvelopeSet::from_datafile(&file), Err(VisualError::InvalidMap));
    }
}
