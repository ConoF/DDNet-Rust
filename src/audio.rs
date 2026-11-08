//! Native Rust PCM mixer, porting the core sample/voice mathematics from
//! DDNet 20.2.1 src/engine/client/sound.cpp.
//!
//! The output is stereo signed 16-bit PCM. No audio device, Opus/WavPack
//! decoder, or automatic sample-rate conversion is implemented yet. A voice
//! loops at the NEXT callback boundary, matching CSound::AdvanceVoice.
//! The Rust accumulation uses i64 so adversarial voice counts cannot overflow.

use std::sync::Arc;

use crate::physics::Vec2;

pub const MAX_VOICES: usize = 256;
pub const MAX_CHANNELS: usize = 16;
pub const MAX_BLOCK_FRAMES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioError {
    InvalidSample,
    InvalidRate,
    RateMismatch,
    InvalidVoice,
    InvalidChannel,
    VoiceLimit,
    BlockTooLarge,
    WavTooLarge,
}

#[derive(Debug, Clone)]
pub struct Sample {
    data: Vec<i16>,
    channels: usize,
    rate: u32,
    loop_start: usize,
}

impl Sample {
    pub fn new(
        data: Vec<i16>, channels: usize, rate: u32, loop_start: usize,
    ) -> Result<Self, AudioError> {
        if !(1..=2).contains(&channels) || !(8_000..=192_000).contains(&rate)
            || data.is_empty() || !data.len().is_multiple_of(channels)
            || data.len() / channels > rate as usize * 120
            || loop_start >= data.len() / channels
        {
            return Err(AudioError::InvalidSample);
        }
        Ok(Self { data, channels, rate, loop_start })
    }

    pub fn frames(&self) -> usize { self.data.len() / self.channels }
    pub fn channels(&self) -> usize { self.channels }
    pub fn rate(&self) -> u32 { self.rate }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpatialShape {
    Circle { radius: f32 },
    Rectangle { width: f32, height: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceOptions {
    pub channel: usize,
    pub volume: f32,
    pub position: Option<Vec2>,
    pub shape: SpatialShape,
    pub falloff: f32,
    pub no_panning: bool,
    pub looping: bool,
}

impl Default for VoiceOptions {
    fn default() -> Self {
        Self {
            channel: 0,
            volume: 1.0,
            position: None,
            shape: SpatialShape::Circle { radius: 1500.0 },
            falloff: 0.0,
            no_panning: false,
            looping: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Channel {
    volume: u8,
    pan: bool,
}

#[derive(Debug, Clone)]
struct Voice {
    sample: Arc<Sample>,
    cursor: usize,
    volume: u8,
    options: VoiceOptions,
    finished: bool,
}

#[derive(Debug, Clone)]
pub struct AudioMixer {
    rate: u32,
    channels: [Channel; MAX_CHANNELS],
    voices: Vec<Voice>,
    listener: Vec2,
    master_volume: u8,
}

impl AudioMixer {
    pub fn new(rate: u32) -> Result<Self, AudioError> {
        if !(8_000..=192_000).contains(&rate) {
            return Err(AudioError::InvalidRate);
        }
        Ok(Self {
            rate,
            channels: [Channel { volume: 255, pan: false }; MAX_CHANNELS],
            voices: Vec::new(),
            listener: Vec2::default(),
            master_volume: 100,
        })
    }

    pub fn set_master_volume(&mut self, volume: u8) {
        self.master_volume = volume.min(100);
    }

    pub fn set_channel(&mut self, index: usize, volume: u8, pan: bool) -> Result<(), AudioError> {
        let channel = self.channels.get_mut(index).ok_or(AudioError::InvalidChannel)?;
        *channel = Channel { volume, pan };
        Ok(())
    }

    pub fn set_listener(&mut self, position: Vec2) -> Result<(), AudioError> {
        if !position.x.is_finite() || !position.y.is_finite() {
            return Err(AudioError::InvalidVoice);
        }
        self.listener = position;
        Ok(())
    }

    pub fn voice_count(&self) -> usize { self.voices.len() }
    pub fn is_empty(&self) -> bool { self.voices.is_empty() }
    pub fn stop_all(&mut self) { self.voices.clear(); }

    pub fn play(&mut self, sample: Arc<Sample>, options: VoiceOptions) -> Result<(), AudioError> {
        if sample.rate != self.rate {
            return Err(AudioError::RateMismatch);
        }
        if self.voices.len() >= MAX_VOICES {
            return Err(AudioError::VoiceLimit);
        }
        if options.channel >= MAX_CHANNELS || !options.volume.is_finite()
            || !(0.0..=1.0).contains(&options.volume)
            || !(0.0..=1.0).contains(&options.falloff)
            || options.position.is_some_and(|p| !p.x.is_finite() || !p.y.is_finite())
        {
            return Err(AudioError::InvalidVoice);
        }
        let valid_shape = match options.shape {
            SpatialShape::Circle { radius } => radius.is_finite() && radius > 0.0,
            SpatialShape::Rectangle { width, height } => {
                width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0
            }
        };
        if !valid_shape {
            return Err(AudioError::InvalidVoice);
        }
        self.voices.push(Voice {
            sample,
            cursor: 0,
            volume: (options.volume * 255.0) as u8,
            options,
            finished: false,
        });
        Ok(())
    }

    fn volume_pair(&self, voice: &Voice) -> (i64, i64) {
        let channel = &self.channels[voice.options.channel];
        let volume = (f32::from(channel.volume) * (f32::from(voice.volume) / 255.0))
            .trunc() as i64;
        let Some(position) = voice.options.position else { return (volume, volume); };
        if !channel.pan {
            return (volume, volume);
        }

        let delta = Vec2::new(position.x - self.listener.x, position.y - self.listener.y);
        let (range_x, mut falloff_x, mut falloff_y, inside) = match voice.options.shape {
            SpatialShape::Circle { radius } => {
                let distance = delta.x.hypot(delta.y);
                let f = if distance >= radius {
                    0.0
                } else if distance > radius * voice.options.falloff {
                    (radius - distance) / (radius * (1.0 - voice.options.falloff))
                } else { 1.0 };
                (radius, f, f, distance < radius)
            }
            SpatialShape::Rectangle { width, height } => {
                let half_x = width / 2.0;
                let half_y = height / 2.0;
                let abs_x = delta.x.abs();
                let abs_y = delta.y.abs();
                let fx = if abs_x >= half_x { 0.0 }
                    else if abs_x > half_x * voice.options.falloff {
                        (half_x - abs_x) / (half_x * (1.0 - voice.options.falloff))
                    } else { 1.0 };
                let fy = if abs_y >= half_y { 0.0 }
                    else if abs_y > half_y * voice.options.falloff {
                        (half_y - abs_y) / (half_y * (1.0 - voice.options.falloff))
                    } else { 1.0 };
                (half_x, fx, fy, abs_x < half_x && abs_y < half_y)
            }
        };
        if !inside {
            return (0, 0);
        }
        falloff_x = falloff_x.clamp(0.0, 1.0);
        falloff_y = falloff_y.clamp(0.0, 1.0);
        let mut left = volume;
        let mut right = volume;
        if !voice.options.no_panning {
            let factor = ((range_x - delta.x.abs()) / range_x).clamp(0.0, 1.0);
            if delta.x > 0.0 {
                left = (left as f32 * factor) as i64;
            } else {
                right = (right as f32 * factor) as i64;
            }
        }
        let gain = falloff_x * falloff_y;
        ((left as f32 * gain) as i64, (right as f32 * gain) as i64)
    }

    /// Mix one bounded block using the original DDNet master volume expression.
    pub fn mix_frames(&mut self, frames: usize) -> Result<Vec<i16>, AudioError> {
        if frames > MAX_BLOCK_FRAMES {
            return Err(AudioError::BlockTooLarge);
        }
        let mut accumulator = vec![0i64; frames * 2];
        for index in 0..self.voices.len() {
            let (left, right) = self.volume_pair(&self.voices[index]);
            let voice = &mut self.voices[index];
            let sample = &voice.sample;
            let end = frames.min(sample.frames() - voice.cursor);
            for i in 0..end {
                let source = (voice.cursor + i) * sample.channels;
                let l = i64::from(sample.data[source]);
                let r = i64::from(sample.data[source + sample.channels - 1]);
                accumulator[i * 2] += l * left;
                accumulator[i * 2 + 1] += r * right;
            }
            voice.cursor += end;
            if voice.cursor == sample.frames() {
                if voice.options.looping {
                    voice.cursor = sample.loop_start;
                } else {
                    voice.finished = true;
                }
            }
        }
        self.voices.retain(|voice| !voice.finished);
        Ok(accumulator.into_iter().map(|sum| {
            let scaled = ((sum * i64::from(self.master_volume)) / 101) >> 8;
            scaled.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
        }).collect())
    }

    /// Produce a standard PCM16/stereo WAV without opening an audio device.
    pub fn wav_bytes(&self, stereo: &[i16]) -> Result<Vec<u8>, AudioError> {
        if !stereo.len().is_multiple_of(2) {
            return Err(AudioError::InvalidSample);
        }
        let payload = stereo.len().checked_mul(2).ok_or(AudioError::WavTooLarge)?;
        let payload = u32::try_from(payload).map_err(|_| AudioError::WavTooLarge)?;
        let chunk_size = payload.checked_add(36).ok_or(AudioError::WavTooLarge)?;
        let mut bytes = Vec::with_capacity(payload as usize + 44);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&chunk_size.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&self.rate.to_le_bytes());
        bytes.extend_from_slice(&(self.rate * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&payload.to_le_bytes());
        for sample in stereo {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(data: &[i16], channels: usize, loop_start: usize) -> Arc<Sample> {
        Arc::new(Sample::new(data.to_vec(), channels, 48_000, loop_start).unwrap())
    }

    fn ddnet_mix(input: i16, volume: i64, master: i64) -> i16 {
        (((i64::from(input) * volume * master / 101) >> 8)
            .clamp(i64::from(i16::MIN), i64::from(i16::MAX))) as i16
    }

    #[test]
    fn one_mono_sample_matches_ddnet_master_volume_expression() {
        let mut mix = AudioMixer::new(48_000).unwrap();
        mix.play(sample(&[1000, -2000, 3000], 1, 0), VoiceOptions::default()).unwrap();
        let output = mix.mix_frames(4).unwrap();
        assert_eq!(output, vec![
            ddnet_mix(1000, 255, 100), ddnet_mix(1000, 255, 100),
            ddnet_mix(-2000, 255, 100), ddnet_mix(-2000, 255, 100),
            ddnet_mix(3000, 255, 100), ddnet_mix(3000, 255, 100),
            0, 0,
        ]);
        assert!(mix.is_empty());
    }

    #[test]
    fn stereo_mixing_saturation_and_master_mute() {
        let mut mix = AudioMixer::new(48_000).unwrap();
        for _ in 0..4 {
            mix.play(sample(&[i16::MAX, i16::MIN], 2, 0),
                VoiceOptions::default()).unwrap();
        }
        assert_eq!(mix.mix_frames(1).unwrap(), [i16::MAX, i16::MIN]);
        mix.play(sample(&[500, -1000], 2, 0), VoiceOptions::default()).unwrap();
        mix.set_master_volume(0);
        assert_eq!(mix.mix_frames(1).unwrap(), [0, 0]);
    }

    #[test]
    fn ddnet_loop_boundary_is_next_callback() {
        let mut mix = AudioMixer::new(48_000).unwrap();
        mix.play(sample(&[100, 200, 300], 1, 1), VoiceOptions {
            looping: true, ..VoiceOptions::default()
        }).unwrap();
        let first = mix.mix_frames(5).unwrap();
        assert_eq!(first[0], ddnet_mix(100, 255, 100));
        assert_eq!(first[2], ddnet_mix(200, 255, 100));
        assert_eq!(first[4], ddnet_mix(300, 255, 100));
        assert_eq!(first[6], 0);
        assert_eq!(mix.voice_count(), 1);
        let next = mix.mix_frames(2).unwrap();
        assert_eq!(next[0], ddnet_mix(200, 255, 100));
        assert_eq!(next[2], ddnet_mix(300, 255, 100));
    }

    #[test]
    fn positional_circle_pan_and_falloff() {
        let mut mix = AudioMixer::new(48_000).unwrap();
        mix.set_channel(0, 255, true).unwrap();
        mix.play(sample(&[2000], 1, 0), VoiceOptions {
            position: Some(Vec2::new(50.0, 0.0)),
            shape: SpatialShape::Circle { radius: 100.0 },
            ..VoiceOptions::default()
        }).unwrap();
        let output = mix.mix_frames(1).unwrap();
        assert!(output[0] > 0 && output[0] < output[1]);
        let mut mix = AudioMixer::new(48_000).unwrap();
        mix.set_channel(0, 255, true).unwrap();
        mix.play(sample(&[2000], 1, 0), VoiceOptions {
            position: Some(Vec2::new(101.0, 0.0)),
            shape: SpatialShape::Circle { radius: 100.0 },
            ..VoiceOptions::default()
        }).unwrap();
        assert_eq!(mix.mix_frames(1).unwrap(), [0, 0]);
    }

    #[test]
    fn wav_header_has_correct_sizes_and_pcm_bytes() {
        let mixer = AudioMixer::new(48_000).unwrap();
        let bytes = mixer.wav_bytes(&[1, -1, 32767, -32768]).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[4..8], &44u32.to_le_bytes());
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(&bytes[24..28], &48_000u32.to_le_bytes());
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(&bytes[40..44], &8u32.to_le_bytes());
        assert_eq!(&bytes[44..46], &1i16.to_le_bytes());
        assert_eq!(bytes.len(), 52);
    }

    #[test]
    fn invalid_input_and_voice_bounds() {
        assert_eq!(AudioMixer::new(1).err(), Some(AudioError::InvalidRate));
        assert_eq!(Sample::new(vec![10, 20, 30], 2, 48_000, 0).err(),
            Some(AudioError::InvalidSample));
        let mut mix = AudioMixer::new(48_000).unwrap();
        assert_eq!(mix.mix_frames(MAX_BLOCK_FRAMES + 1), Err(AudioError::BlockTooLarge));
        assert_eq!(mix.set_channel(MAX_CHANNELS, 100, false),
            Err(AudioError::InvalidChannel));
        let invalid = VoiceOptions { volume: f32::NAN, ..VoiceOptions::default() };
        assert_eq!(mix.play(sample(&[0], 1, 0), invalid), Err(AudioError::InvalidVoice));
        for _ in 0..MAX_VOICES {
            mix.play(sample(&[0], 1, 0), VoiceOptions::default()).unwrap();
        }
        assert_eq!(mix.play(sample(&[0], 1, 0), VoiceOptions::default()),
            Err(AudioError::VoiceLimit));
        assert_eq!(mix.voice_count(), MAX_VOICES);
        mix.stop_all();
        assert!(mix.is_empty());
    }
}
