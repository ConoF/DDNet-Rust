//! Loading DDNet 20.2.1's original .wv sound effects using only Rust.
//! Sound set names and filenames are from pinned upstream datasrc/content.py.
//!
//! No audio device is opened here. Decoded and resampled samples are
//! playable through AudioMixer or exportable as standard PCM WAV.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::audio::{AudioError, AudioMixer, Sample, VoiceOptions};
use crate::physics::Vec2;

/// Input and decompressed frame budgets: refuse malformed/hostile .wv files
/// before calling wavicle's allocation-heavy whole-stream decoder.
pub const MAX_WAVPACK_FILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DECODED_SAMPLES: usize = 12_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundError {
    InvalidFile,
    Unsupported,
    TooLarge,
    DecodeFailed(wavicle::Error),
    InconsistentPcm,
    Io,
    Audio(AudioError),
}
impl From<AudioError> for SoundError {
    fn from(value: AudioError) -> Self { Self::Audio(value) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameSound {
    GunFire,
    ShotgunFire,
    GrenadeFire,
    HammerFire,
    HammerHit,
    NinjaFire,
    LaserFire,
    WeaponSwitch,
    PlayerJump,
    PlayerAirjump,
    PlayerLand,
    HookAttach,
    HookNoAttach,
    PlayerDie,
    PlayerSpawn,
}

impl GameSound {
    /// One canonical .wv variant per sound set. Multiple-choice variants
    /// are intentionally postponed until the runtime sound scheduler exists.
    pub fn filename(self) -> &'static str {
        match self {
            Self::GunFire => "wp_gun_fire-01.wv",
            Self::ShotgunFire => "wp_shotty_fire-01.wv",
            Self::GrenadeFire => "wp_flump_launch-01.wv",
            Self::HammerFire => "wp_hammer_swing-01.wv",
            Self::HammerHit => "wp_hammer_hit-01.wv",
            Self::NinjaFire => "wp_ninja_attack-01.wv",
            Self::LaserFire => "wp_laser_fire-01.wv",
            Self::WeaponSwitch => "wp_switch-01.wv",
            Self::PlayerJump => "foley_foot_left-01.wv",
            Self::PlayerAirjump => "foley_dbljump-01.wv",
            Self::PlayerLand => "foley_land-01.wv",
            Self::HookAttach => "hook_attach-01.wv",
            Self::HookNoAttach => "hook_noattach-01.wv",
            Self::PlayerDie => "foley_body_splat-01.wv",
            Self::PlayerSpawn => "vo_teefault_spawn-01.wv",
        }
    }
}

/// Reject compressed files that could expand past our PCM memory budget.
/// Both compressed file size and declared block frame counts are checked.
pub fn decode_wavpack(bytes: &[u8], mixer_rate: u32) -> Result<Sample, SoundError> {
    if bytes.len() < 32 || !bytes.starts_with(b"wvpk") {
        return Err(SoundError::InvalidFile);
    }
    if bytes.len() > MAX_WAVPACK_FILE_BYTES {
        return Err(SoundError::TooLarge);
    }
    if !(8_000..=192_000).contains(&mixer_rate) {
        return Err(SoundError::Audio(AudioError::InvalidRate));
    }

    let info = wavicle::StreamInfo::scan(bytes)
        .map_err(SoundError::DecodeFailed)?;
    if info.channels == 0 || info.channels > 2
        || info.is_float || info.bits_per_sample != 16
        || !(8_000..=192_000).contains(&info.sample_rate)
    {
        return Err(SoundError::Unsupported);
    }
    let channels = usize::try_from(info.channels).map_err(|_| SoundError::Unsupported)?;
    let mut total_frames = 0usize;
    for block in wavicle::Blocks::new(bytes) {
        let block = block.map_err(SoundError::DecodeFailed)?;
        total_frames = total_frames.checked_add(block.header.block_samples as usize)
            .ok_or(SoundError::TooLarge)?;
        let count = total_frames.checked_mul(channels)
            .ok_or(SoundError::TooLarge)?;
        if count > MAX_DECODED_SAMPLES || total_frames > info.sample_rate as usize * 120 {
            return Err(SoundError::TooLarge);
        }
    }
    if total_frames == 0 {
        return Err(SoundError::InvalidFile);
    }
    if info.total_samples.is_some_and(|declared| declared != total_frames as u64) {
        return Err(SoundError::InconsistentPcm);
    }

    let output = wavicle::decode_stream(bytes).map_err(SoundError::DecodeFailed)?;
    if output.samples.len() != total_frames * channels
        || output.channels != info.channels
        || output.sample_rate != info.sample_rate
        || output.bits_per_sample != 16 || output.is_float
    {
        return Err(SoundError::InconsistentPcm);
    }
    let mut pcm = Vec::new();
    pcm.try_reserve_exact(output.samples.len()).map_err(|_| SoundError::TooLarge)?;
    for value in output.samples {
        let value = i16::try_from(value).map_err(|_| SoundError::InconsistentPcm)?;
        pcm.push(value);
    }
    let sample = Sample::new(pcm, channels, info.sample_rate, 0)?;
    Ok(sample.resampled_to(mixer_rate)?)
}

/// Lazy cache. The caller supplies a trusted DDNet data directory.
/// Filenames are static, so server-provided names cannot traverse paths.
#[derive(Debug)]
pub struct SoundBank {
    audio_root: PathBuf,
    sample_rate: u32,
    loaded: HashMap<GameSound, Arc<Sample>>,
}

impl SoundBank {
    pub fn new(data_dir: &Path, sample_rate: u32) -> Result<Self, SoundError> {
        AudioMixer::new(sample_rate)?;
        Ok(Self {
            audio_root: data_dir.join("audio"),
            sample_rate,
            loaded: HashMap::new(),
        })
    }

    pub fn sample(&mut self, sound: GameSound) -> Result<Arc<Sample>, SoundError> {
        if let Some(sample) = self.loaded.get(&sound) {
            return Ok(Arc::clone(sample));
        }
        let path = self.audio_root.join(sound.filename());
        let metadata = std::fs::metadata(&path).map_err(|_| SoundError::Io)?;
        if metadata.len() > MAX_WAVPACK_FILE_BYTES as u64 {
            return Err(SoundError::TooLarge);
        }
        let bytes = std::fs::read(path).map_err(|_| SoundError::Io)?;
        let sample = Arc::new(decode_wavpack(&bytes, self.sample_rate)?);
        self.loaded.insert(sound, Arc::clone(&sample));
        Ok(sample)
    }

    /// Schedules an original DDNet sound in the mixer. A spatial
    /// position uses the existing 1500px circular attenuation model.
    pub fn play(
        &mut self, mixer: &mut AudioMixer, kind: GameSound, location: Option<Vec2>,
    ) -> Result<(), SoundError> {
        let sample = self.sample(kind)?;
        mixer.play(sample, VoiceOptions {
            position: location,
            ..VoiceOptions::default()
        })?;
        Ok(())
    }

    pub fn loaded_count(&self) -> usize { self.loaded.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_set_paths_match_pinned_ddnet_assets() {
        assert_eq!(GameSound::GunFire.filename(), "wp_gun_fire-01.wv");
        assert_eq!(GameSound::HookAttach.filename(), "hook_attach-01.wv");
        assert_eq!(GameSound::PlayerAirjump.filename(), "foley_dbljump-01.wv");
        assert_eq!(GameSound::LaserFire.filename(), "wp_laser_fire-01.wv");
        assert_eq!(GameSound::PlayerSpawn.filename(), "vo_teefault_spawn-01.wv");
    }

    #[test]
    fn invalid_wavpack_input_and_rate_are_rejected() {
        assert_eq!(decode_wavpack(b"not a wavpack file", 48_000).err(),
            Some(SoundError::InvalidFile));
        assert_eq!(decode_wavpack(&[0; 33], 48_000).err(),
            Some(SoundError::InvalidFile));
        assert_eq!(decode_wavpack(b"wvpk0123456789012345678901234567", 0).err(),
            Some(SoundError::Audio(AudioError::InvalidRate)));
        assert_eq!(decode_wavpack(&vec![0; MAX_WAVPACK_FILE_BYTES + 1],
            48_000).err(), Some(SoundError::InvalidFile));
        let mut oversized = b"wvpk".to_vec();
        oversized.resize(MAX_WAVPACK_FILE_BYTES + 1, 0);
        assert_eq!(decode_wavpack(&oversized, 48_000).err(), Some(SoundError::TooLarge));
    }

    #[test]
    fn bank_rejects_missing_assets_without_mutating_cache() {
        let mut bank = SoundBank::new(Path::new("nonexistent-ddnet-assets"), 48_000).unwrap();
        assert_eq!(bank.loaded_count(), 0);
        assert!(matches!(bank.sample(GameSound::GunFire), Err(SoundError::Io)));
        assert_eq!(bank.loaded_count(), 0);
        assert!(SoundBank::new(Path::new("."), 500).is_err());
    }
}
