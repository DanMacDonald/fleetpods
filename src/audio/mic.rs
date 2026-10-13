//! Capture demand for the verified high-resolution microphone.
//! A linked, unmuted hidden source is demand. Playback profile selection stays
//! in [`super::play`]. This module does not switch the accessory to a headset
//! profile, and it does not store PCM.

use crate::types::{Model, MonoMs};

pub const MAX_QUEUE_SAMPLES: u32 = 65_536;
pub const PRIME_MS: u32 = 200;
pub const RESTART_BUDGET: u8 = 2;
/// No decoded audio for this long is a stall. Two restarts, then the recorder reopens.
pub const STALL_MS: u64 = 2_000;
/// Hidden PipeWire source. WirePlumber sends the AirPods input here instead of opening a headset profile.
pub const SOURCE_NAME: &str = "fleetpods.hires-microphone";
/// Linear gain on decoded microphone samples. Peaks are held at full scale.
pub const MIC_GAIN: f32 = 2.5;

pub fn amplify(samples: &mut [f32]) {
    for sample in samples {
        *sample = (*sample * MIC_GAIN).clamp(-1.0, 1.0);
    }
}

const AUDIO_PREFIX: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
const AUDIO_HEADER: usize = 22;
const FRAME_HEADER: usize = 5;

/// Ask the accessory to send microphone audio on the control link.
pub fn start_command() -> Vec<u8> {
    vec![
        0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x01, 0x82, 0x00, 0x00,
        0x00, 0x04, 0x96, 0x00,
    ]
}

/// Ask the accessory to stop that audio.
pub fn stop_command() -> Vec<u8> {
    vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x01]
}

/// One `0x58` audio packet. The AAC bytes are not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioRead {
    /// Not a microphone audio packet.
    Other,
    /// The prefix matched and the frame list was cut short.
    Malformed,
    Frames(u32),
}

pub fn read_audio(packet: &[u8]) -> AudioRead {
    if packet.len() < AUDIO_PREFIX.len() || !packet.starts_with(&AUDIO_PREFIX) {
        return AudioRead::Other;
    }
    if packet.len() <= AUDIO_HEADER {
        return AudioRead::Malformed;
    }
    let mut offset = AUDIO_HEADER;
    let mut frames = 0u32;
    while offset < packet.len() {
        if packet.len() - offset < FRAME_HEADER {
            return AudioRead::Malformed;
        }
        let length = packet[offset + 4] as usize;
        offset += FRAME_HEADER;
        if length == 0 || length > packet.len() - offset {
            return AudioRead::Malformed;
        }
        offset += length;
        frames = frames.saturating_add(1);
    }
    AudioRead::Frames(frames)
}

/// The AAC access units inside one audio packet. `None` means this is not microphone audio.
pub fn audio_units(packet: &[u8]) -> Option<Result<Vec<&[u8]>, ()>> {
    if packet.len() < AUDIO_PREFIX.len() || !packet.starts_with(&AUDIO_PREFIX) {
        return None;
    }
    if packet.len() <= AUDIO_HEADER {
        return Some(Err(()));
    }
    let mut offset = AUDIO_HEADER;
    let mut units = Vec::new();
    while offset < packet.len() {
        if packet.len() - offset < FRAME_HEADER {
            return Some(Err(()));
        }
        let length = packet[offset + 4] as usize;
        offset += FRAME_HEADER;
        if length == 0 || length > packet.len() - offset {
            return Some(Err(()));
        }
        units.push(&packet[offset..offset + length]);
        offset += length;
    }
    Some(Ok(units))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub model: Model,
    pub audio_specific_config: [u8; 4],
    pub coding_hz: u32,
    pub delivery_hz: u32,
    pub samples_per_frame: u32,
}

impl Format {
    pub fn prime_samples(self) -> u32 {
        self.delivery_hz / 1000 * PRIME_MS
    }
}

/// Pro 3 is the verified entry. Other SKUs stay hidden until they have one.
pub fn format_for_sku(sku: &str) -> Option<Format> {
    match sku {
        "A3063" | "A3064" | "A3065" => Some(Format {
            model: Model::AirPodsPro3,
            audio_specific_config: [0xf8, 0xe6, 0x30, 0x00],
            coding_hz: 48_000,
            delivery_hz: 64_000,
            samples_per_frame: 480,
        }),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Control is down, so the source is not offered.
    Down,
    Idle,
    Capturing,
    /// Two automatic restarts were spent. The recorder has to reopen the source.
    NeedsReopen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    None,
    Start,
    Stop,
}

pub struct Microphone {
    phase: Phase,
    restarts: u8,
    started_at: Option<MonoMs>,
    last_packet: Option<MonoMs>,
    frames: u32,
    malformed: u32,
    max_gap_ms: u64,
    max_packet_frames: u32,
}

impl Microphone {
    pub fn new() -> Self {
        Self {
            phase: Phase::Down,
            restarts: 0,
            started_at: None,
            last_packet: None,
            frames: 0,
            malformed: 0,
            max_gap_ms: 0,
            max_packet_frames: 0,
        }
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    pub fn malformed(&self) -> u32 {
        self.malformed
    }

    pub fn max_gap_ms(&self) -> u64 {
        self.max_gap_ms
    }

    pub fn max_packet_frames(&self) -> u32 {
        self.max_packet_frames
    }

    pub fn phase_wire(&self) -> &'static str {
        match self.phase {
            Phase::Down => "down",
            Phase::Idle => "idle",
            Phase::Capturing => "capturing",
            Phase::NeedsReopen => "needs_reopen",
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn capturing(&self) -> bool {
        self.phase == Phase::Capturing
    }

    pub fn set_control_ready(&mut self, ready: bool) {
        if ready {
            if self.phase == Phase::Down {
                self.phase = Phase::Idle;
                self.restarts = 0;
            }
        } else {
            self.phase = Phase::Down;
            self.restarts = 0;
        }
    }

    /// `live` means a recorder is linked to the hidden source and that source is not muted.
    /// Leaving the source clears a spent restart budget, so the recorder can open it again.
    pub fn note_live(&mut self, live: bool) -> Motion {
        if self.phase == Phase::Down {
            return Motion::None;
        }
        if !live {
            let stop = self.phase == Phase::Capturing;
            self.phase = Phase::Idle;
            self.restarts = 0;
            self.started_at = None;
            self.last_packet = None;
            return if stop { Motion::Stop } else { Motion::None };
        }
        if self.phase != Phase::Idle {
            return Motion::None;
        }
        self.phase = Phase::Capturing;
        Motion::Start
    }

    /// A stalled capture may restart twice. The third stall waits for a new client.
    pub fn stalled(&mut self) -> Phase {
        if self.phase != Phase::Capturing {
            return self.phase;
        }
        if self.restarts < RESTART_BUDGET {
            self.restarts += 1;
        } else {
            self.phase = Phase::NeedsReopen;
        }
        self.phase
    }

    pub fn arm(&mut self, now: MonoMs) {
        self.started_at = Some(now);
        self.last_packet = None;
    }

    pub fn quiet_for(&self, now: MonoMs) -> Option<u64> {
        let from = self.last_packet.or(self.started_at)?;
        Some(now.saturating_since(from))
    }

    /// Count one well-formed audio packet. The bytes are dropped by the caller.
    pub fn note_frames(&mut self, frames: u32, now: MonoMs) {
        if let Some(previous) = self.last_packet {
            let gap = now.saturating_since(previous);
            if gap > self.max_gap_ms {
                self.max_gap_ms = gap;
            }
        }
        self.last_packet = Some(now);
        self.frames = self.frames.saturating_add(frames);
        if frames > self.max_packet_frames {
            self.max_packet_frames = frames;
        }
    }

    pub fn note_malformed(&mut self) {
        self.malformed = self.malformed.saturating_add(1);
    }
}

impl Default for Microphone {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pro_3_format_is_exposed_and_stalls_are_bounded() {
        let format = format_for_sku("A3064").unwrap();
        assert_eq!(format.coding_hz, 48_000);
        assert_eq!(format.delivery_hz, 64_000);
        assert_eq!(format.samples_per_frame, 480);
        assert_eq!(format.audio_specific_config, [0xf8, 0xe6, 0x30, 0x00]);
        assert_eq!(format.prime_samples(), 12_800);
        assert!(format_for_sku("A2084").is_none());

        let mut mic = Microphone::new();
        assert_eq!(mic.note_live(true), Motion::None);
        mic.set_control_ready(true);
        assert_eq!(mic.note_live(true), Motion::Start);
        assert_eq!(mic.note_live(true), Motion::None);
        assert_eq!(mic.note_live(false), Motion::Stop);
        assert_eq!(mic.note_live(true), Motion::Start);
        assert_eq!(mic.stalled(), Phase::Capturing);
        assert_eq!(mic.stalled(), Phase::Capturing);
        assert_eq!(mic.stalled(), Phase::NeedsReopen);
        assert_eq!(mic.note_live(true), Motion::None);
        assert_eq!(mic.note_live(false), Motion::None);
        assert_eq!(mic.phase(), Phase::Idle);
        assert_eq!(mic.note_live(true), Motion::Start);
        mic.set_control_ready(false);
        assert_eq!(mic.phase(), Phase::Down);
        assert_eq!(mic.note_live(true), Motion::None);
    }

    #[test]
    fn an_audio_packet_yields_frames_and_a_short_one_does_not() {
        let mut packet = vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
        packet.extend(std::iter::repeat_n(0, 14));
        packet.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 2, 0xaa, 0xbb]);
        packet.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 1, 0xcc]);
        assert_eq!(read_audio(&packet), AudioRead::Frames(2));
        assert_eq!(read_audio(&packet[..packet.len() - 1]), AudioRead::Malformed);
        assert_eq!(read_audio(&start_command()), AudioRead::Other);
        assert_eq!(read_audio(&[0x04, 0x00]), AudioRead::Other);
    }

    #[test]
    fn decoded_samples_are_amplified_and_peaks_stay_in_range() {
        let mut samples = [0.2, -0.2, 0.5, -0.8];
        amplify(&mut samples);
        assert!((samples[0] - 0.5).abs() < 1e-5);
        assert!((samples[1] + 0.5).abs() < 1e-5);
        assert_eq!(samples[2], 1.0);
        assert_eq!(samples[3], -1.0);
    }
}
