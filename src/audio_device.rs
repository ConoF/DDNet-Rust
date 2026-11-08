//! Safe Rust wrapper around native Windows WASAPI / Linux ALSA output (CPAL).
//!
//! The game thread schedules sounds; a separate CPAL callback mixes PCM.
//! The audio callback uses try_lock, never waits for WavPack decoding or file
//! access. No hardware is required by tests; missing devices disable audio
//! without disabling the main client window.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::audio::{AudioMixer, Sample, VoiceOptions, MAX_BLOCK_FRAMES};
use crate::physics::Vec2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAudioError {
    NoDevice,
    Configuration(String),
    Open(String),
    InvalidMixer,
}

pub struct AudioDevice {
    mixer: Arc<Mutex<AudioMixer>>,
    stream: cpal::Stream,
    sample_rate: u32,
}

/// Copy mono/stereo mixer data into an arbitrary device channel layout.
/// Channels >2 receive a quiet copy of the stereo mid signal; all unwritten
/// samples, failed or contended mixer calls, and incomplete frames are silent.
fn fill_output<T: Copy>(
    output: &mut [T],
    channels: usize,
    mixer: &Arc<Mutex<AudioMixer>>,
    silence: T,
    convert: fn(i16) -> T,
) -> bool {
    output.fill(silence);
    if channels == 0 || channels > 32 {
        return false;
    }
    let Ok(mut mix) = mixer.try_lock() else {
        return false;
    };
    for chunk in output.chunks_mut(MAX_BLOCK_FRAMES * channels) {
        let frames = chunk.len() / channels;
        if frames == 0 { continue; }
        let Ok(pcm) = mix.mix_frames(frames) else { return false; };
        for (dest, sample) in chunk.chunks_exact_mut(channels).zip(pcm.chunks_exact(2)) {
            let left = sample[0];
            let right = sample[1];
            dest[0] = convert(left);
            if channels >= 2 {
                dest[1] = convert(right);
                for extra in &mut dest[2..] {
                    *extra = convert(((i32::from(left) + i32::from(right)) / 2) as i16);
                }
            }
        }
    }
    true
}

fn as_f32(pcm: i16) -> f32 {
    f32::from(pcm) / 32768.0
}
fn as_u16(pcm: i16) -> u16 {
    (i32::from(pcm) + 32768) as u16
}

impl AudioDevice {
    pub fn new() -> Result<Self, DeviceAudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(DeviceAudioError::NoDevice)?;
        let supported = device.default_output_config()
            .map_err(|e| DeviceAudioError::Configuration(e.to_string()))?;
        let sample_rate = supported.sample_rate().0;
        let channels = usize::from(supported.channels());
        if !(8_000..=192_000).contains(&sample_rate) || !(1..=32).contains(&channels) {
            return Err(DeviceAudioError::Configuration(format!(
                "Unsupported device rate={sample_rate}, channels={channels}",
            )));
        }
        let mixer = Arc::new(Mutex::new(
            AudioMixer::new(sample_rate).map_err(|_| DeviceAudioError::InvalidMixer)?,
        ));
        mixer.lock().map_err(|_| DeviceAudioError::InvalidMixer)?
            .set_channel(0, 255, true).map_err(|_| DeviceAudioError::InvalidMixer)?;
        let config: cpal::StreamConfig = supported.config();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I16 => {
                let share = Arc::clone(&mixer);
                device.build_output_stream(&config,
                    move |samples: &mut [i16], _| {
                        fill_output(samples, channels, &share, 0i16, |x| x);
                    },
                    |err| eprintln!("DDNet-Rust audio output error: {err}"),
                    None)
            }
            cpal::SampleFormat::F32 => {
                let share = Arc::clone(&mixer);
                device.build_output_stream(&config,
                    move |samples: &mut [f32], _| {
                        fill_output(samples, channels, &share, 0.0f32, as_f32);
                    },
                    |err| eprintln!("DDNet-Rust audio output error: {err}"),
                    None)
            }
            cpal::SampleFormat::U16 => {
                let share = Arc::clone(&mixer);
                device.build_output_stream(&config,
                    move |samples: &mut [u16], _| {
                        fill_output(samples, channels, &share, 32768u16, as_u16);
                    },
                    |err| eprintln!("DDNet-Rust audio output error: {err}"),
                    None)
            }
            unsupported => return Err(DeviceAudioError::Configuration(
                format!("Unsupported output sample format: {unsupported:?}"),
            )),
        }.map_err(|e| DeviceAudioError::Open(e.to_string()))?;
        stream.play().map_err(|e| DeviceAudioError::Open(e.to_string()))?;
        Ok(Self { mixer, stream, sample_rate })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_master_volume(&self, volume: u8) -> Result<(), DeviceAudioError> {
        if volume > 100 { return Err(DeviceAudioError::InvalidMixer); }
        self.mixer.lock().map_err(|_| DeviceAudioError::InvalidMixer)?
            .set_master_volume(volume);
        Ok(())
    }

    pub fn play(&self, sample: Arc<Sample>, location: Option<Vec2>)
        -> Result<(), DeviceAudioError>
    {
        let mut mix = self.mixer.lock().map_err(|_| DeviceAudioError::InvalidMixer)?;
        mix.play(sample, VoiceOptions {
            position: location,
            ..VoiceOptions::default()
        }).map_err(|_| DeviceAudioError::InvalidMixer)
    }

    pub fn set_listener(&self, location: Vec2) -> Result<(), DeviceAudioError> {
        self.mixer.lock().map_err(|_| DeviceAudioError::InvalidMixer)?
            .set_listener(location).map_err(|_| DeviceAudioError::InvalidMixer)
    }

    pub fn is_running(&self) -> bool {
        let _keep_stream_alive = &self.stream;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_device_conversion_preserves_channel_order() {
        let mixer = Arc::new(Mutex::new(AudioMixer::new(48_000).unwrap()));
        let source = Arc::new(Sample::new(vec![1000, -2000, -3000, 4000],
            2, 48_000, 0).unwrap());
        mixer.lock().unwrap().play(source, VoiceOptions::default()).unwrap();
        let mut out = [0i16; 8];
        assert!(fill_output(&mut out, 2, &mixer, 0, |x| x));
        assert!(out[0] > 0 && out[1] < 0);
        assert!(out[2] < 0 && out[3] > 0);
        assert_eq!(&out[4..], &[0; 4]);
    }

    #[test]
    fn mono_multichannel_float_and_unsigned_output_are_validated() {
        let mixer = Arc::new(Mutex::new(AudioMixer::new(48_000).unwrap()));
        let voice = Arc::new(Sample::new(vec![8000], 1, 48_000, 0).unwrap());
        mixer.lock().unwrap().play(voice.clone(), VoiceOptions::default()).unwrap();
        let mut mono = [0.0f32; 2];
        assert!(fill_output(&mut mono, 1, &mixer, 0.0, as_f32));
        assert!(mono[0] > 0.0 && mono[0] <= 1.0);
        assert_eq!(mono[1], 0.0);

        mixer.lock().unwrap().play(voice, VoiceOptions::default()).unwrap();
        let mut multi = [0u16; 4];
        assert!(fill_output(&mut multi, 4, &mixer, 32768u16, as_u16));
        assert!(multi.iter().all(|x| *x > 32768));
        assert_eq!(as_u16(i16::MIN), 0);
        assert_eq!(as_u16(i16::MAX), u16::MAX);
        assert_eq!(as_f32(i16::MIN), -1.0);
    }

    #[test]
    fn audio_thread_never_waits_for_contended_mixer() {
        let mixer = Arc::new(Mutex::new(AudioMixer::new(48_000).unwrap()));
        let guard = mixer.lock().unwrap();
        let mut data = [42i16; 16];
        assert!(!fill_output(&mut data, 2, &mixer, 0i16, |x| x));
        assert_eq!(data, [0; 16]);
        drop(guard);
        assert!(!fill_output(&mut data, 0, &mixer, 0i16, |x| x));
    }
}
