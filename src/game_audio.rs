//! Game-event sound scheduler; keeps decoding off CPAL's callback thread.
//! Canonical sound sets use the pinned DDNet 20.2.1 audio directory.
//!
//! Audio is optional: a missing output device or unsupported hybrid WavPack
//! must never prevent input, physics or map rendering.

use std::collections::HashSet;
use std::path::Path;

use crate::audio_device::{AudioDevice, DeviceAudioError};
use crate::hook::HookEvent;
use crate::physics::{JumpEvent, Vec2};
use crate::simulator::SimStep;
use crate::sound_assets::{GameSound, SoundBank, SoundError};

pub struct GameAudio {
    device: AudioDevice,
    bank: SoundBank,
    failed: HashSet<GameSound>,
}

pub fn attack_sound_for_weapon(weapon: i32) -> Option<GameSound> {
    match weapon {
        0 => Some(GameSound::HammerFire),
        1 => Some(GameSound::GunFire),
        2 => Some(GameSound::ShotgunFire),
        3 => Some(GameSound::GrenadeFire),
        4 => Some(GameSound::LaserFire),
        5 => Some(GameSound::NinjaFire),
        _ => None,
    }
}

pub fn movement_sounds(step: SimStep) -> Vec<GameSound> {
    let mut events = Vec::new();
    match step.jump {
        JumpEvent::Ground => events.push(GameSound::PlayerJump),
        JumpEvent::Air => events.push(GameSound::PlayerAirjump),
        JumpEvent::None => {}
    }
    match step.hook {
        HookEvent::AttachGround | HookEvent::AttachPlayer =>
            events.push(GameSound::HookAttach),
        HookEvent::HitNoHook => events.push(GameSound::HookNoAttach),
        _ => {}
    }
    events
}

impl GameAudio {
    /// Attempts to open the OS default audio device and identify original
    /// DDNet data/audio files. If either fails, caller should run silently.
    pub fn new(data_dir: &Path) -> Result<Self, String> {
        let device = AudioDevice::new().map_err(|e| format!("audio device: {e:?}"))?;
        if !data_dir.join("audio").join(GameSound::GunFire.filename()).is_file() {
            return Err(format!("DDNet audio assets unavailable: {}", data_dir.display()));
        }
        let bank = SoundBank::new(data_dir, device.sample_rate())
            .map_err(|e| format!("audio resources: {e:?}"))?;
        Ok(Self { device, bank, failed: HashSet::new() })
    }

    pub fn play(&mut self, kind: GameSound, at: Option<Vec2>) {
        if self.failed.contains(&kind) { return; }
        let result = self.bank.sample(kind)
            .map_err(|e| format!("sound {:?}: {e:?}", kind))
            .and_then(|sample| self.device.play(sample, at)
                .map_err(|e| format!("audio output {:?}: {e:?}", kind)));
        if let Err(error) = result {
            self.failed.insert(kind);
            eprintln!("DDNet-Rust audio: {error} (muting this sound)");
        }
    }

    pub fn listener(&self, position: Vec2) {
        let _ = self.device.set_listener(position);
    }

    pub fn play_movement(&mut self, step: SimStep) {
        for kind in movement_sounds(step) {
            self.play(kind, Some(step.position));
        }
    }

    pub fn preload(&mut self) {
        for kind in [
            GameSound::GunFire, GameSound::HammerFire,
            GameSound::ShotgunFire, GameSound::GrenadeFire,
            GameSound::PlayerJump, GameSound::PlayerLand,
            GameSound::WeaponSwitch, GameSound::NinjaFire,
        ] {
            // All preloading is done on the game thread, not an OS callback.
            if let Err(error) = self.bank.sample(kind) {
                if !matches!(error, SoundError::Unsupported) {
                    eprintln!("DDNet-Rust preload {:?}: {error:?}", kind);
                }
                self.failed.insert(kind);
            }
        }
    }

    pub fn output_rate(&self) -> u32 {
        self.device.sample_rate()
    }
}

/// Avoid creating an audio backend in CI or in an environment with no output
/// device, while making the actual program use sound when it exists.
pub fn try_start(roots: &[std::path::PathBuf]) -> Option<GameAudio> {
    let root = roots.iter().find(|root| root.join("audio")
        .join(GameSound::GunFire.filename()).is_file())?;
    match GameAudio::new(root) {
        Ok(mut audio) => {
            audio.preload();
            eprintln!("DDNet-Rust audio output ready: {}Hz", audio.output_rate());
            Some(audio)
        }
        Err(error) => {
            eprintln!("DDNet-Rust: continuing without sound: {error}");
            None
        }
    }
}

/// Convert selected device initialization errors to stable diagnostics.
pub fn device_error_label(error: &DeviceAudioError) -> &'static str {
    match error {
        DeviceAudioError::NoDevice => "no default output device",
        DeviceAudioError::Configuration(_) => "unsupported audio configuration",
        DeviceAudioError::Open(_) => "failed to open playback device",
        DeviceAudioError::InvalidMixer => "invalid internal mixer",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weapon_attack_mapping_matches_ddnet_indices() {
        assert_eq!(attack_sound_for_weapon(0), Some(GameSound::HammerFire));
        assert_eq!(attack_sound_for_weapon(1), Some(GameSound::GunFire));
        assert_eq!(attack_sound_for_weapon(2), Some(GameSound::ShotgunFire));
        assert_eq!(attack_sound_for_weapon(3), Some(GameSound::GrenadeFire));
        assert_eq!(attack_sound_for_weapon(4), Some(GameSound::LaserFire));
        assert_eq!(attack_sound_for_weapon(5), Some(GameSound::NinjaFire));
        assert_eq!(attack_sound_for_weapon(-1), None);
        assert_eq!(attack_sound_for_weapon(6), None);
    }

    #[test]
    fn jump_and_hook_events_are_one_shot() {
        let step = SimStep {
            tick: 1, position: Vec2::default(), velocity: Vec2::default(),
            jump: JumpEvent::Ground, hook: HookEvent::AttachGround,
            grounded: true,
        };
        assert_eq!(movement_sounds(step),
            vec![GameSound::PlayerJump, GameSound::HookAttach]);
        assert!(movement_sounds(SimStep {
            jump: JumpEvent::None, hook: HookEvent::None, ..step
        }).is_empty());
        assert_eq!(movement_sounds(SimStep {
            jump: JumpEvent::Air, hook: HookEvent::HitNoHook, ..step
        }), vec![GameSound::PlayerAirjump, GameSound::HookNoAttach]);
    }
}
