//! Local desktop audio: profile choice, playback observations,
//! ear-detection pauses, and conversation-awareness volume.
//! Stream mute/volume holds and their restoration belong to audio::route.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::types::{EarBehavior, MonoMs, PlayKind, PlaybackEvent, UnixMs};

const TOKEN_LIMIT: usize = 64;
const EAR_DEBOUNCE_MS: u64 = 400;
const OBSERVER_TTL_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    pub priority: i32,
    pub available: bool,
    pub sinks: u32,
    pub sources: u32,
}

pub fn codec_bitrate_kbps(codec: &str) -> i32 {
    match codec {
        "SBC-XQ" => 453,
        "SBC" => 328,
        "AAC" => 256,
        _ => 0,
    }
}

pub fn codec_from_profile_name(name: &str) -> Option<String> {
    let suffix = name.rsplit_once('-')?.1;
    if matches!(suffix, "sink" | "source" | "unit" | "headset") {
        return None;
    }
    Some(suffix.to_ascii_uppercase().replace('_', "-"))
}

pub fn codec_from_description(description: &str) -> Option<String> {
    let rest = description.split_once("codec ")?.1;
    let codec = rest.split(')').next().unwrap_or(rest).trim();
    if codec.is_empty() {
        None
    } else {
        Some(codec.to_string())
    }
}

/// Highest ranked stereo profile. A source means a headset profile, which is skipped.
/// Called on every activation; the previous choice is not sticky.
pub fn best_playback(candidates: &[Profile]) -> Option<&Profile> {
    let mut best: Option<&Profile> = None;
    let mut best_bitrate = -1;
    let mut best_priority = i32::MIN;
    for candidate in candidates {
        if !candidate.available || candidate.sinks == 0 || candidate.sources > 0 {
            continue;
        }
        let mut bitrate = codec_from_profile_name(&candidate.name)
            .map(|codec| codec_bitrate_kbps(&codec))
            .unwrap_or(0);
        if bitrate == 0 {
            bitrate = codec_from_description(&candidate.description)
                .map(|codec| codec_bitrate_kbps(&codec))
                .unwrap_or(0);
        }
        if bitrate > best_bitrate || (bitrate == best_bitrate && candidate.priority > best_priority)
        {
            best_bitrate = bitrate;
            best_priority = candidate.priority;
            best = Some(candidate);
        }
    }
    best
}

/// Round half up onto a percentage grid. Stem changes use a step of 5.
pub fn snap_percent(value: i32, step: i32) -> i32 {
    let step = if step <= 0 { 1 } else { step.min(100) };
    let clamped = value.clamp(0, 100);
    let snapped = ((clamped + step / 2) / step) * step;
    snapped.clamp(0, 100)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamSnap {
    pub id: u32,
    pub generation: u64,
    pub mute: bool,
    /// Linear gain. Values above 1.0 are preserved.
    pub volume: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayReply {
    Ok,
    Accepted { claim: bool },
    InvalidOrStale,
    InvalidEvent,
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal {
    None,
    One,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarAction {
    Hold,
    Pause,
    Resume,
}

#[derive(Clone, Debug)]
struct Duck {
    saved: Option<f64>,
    active: bool,
}

#[derive(Clone, Debug)]
struct Ears {
    left: Option<bool>,
    right: Option<bool>,
    settled: Removal,
    pending: Option<(Removal, MonoMs)>,
    paused_by_us: bool,
}

pub struct Audio {
    offers: Vec<Profile>,
    selected: Option<String>,
    local_playing: bool,
    routed: bool,
    observers: BTreeMap<String, UnixMs>,
    tokens: VecDeque<String>,
    token_set: BTreeSet<String>,
    duck: Duck,
    ears: Ears,
    conversation_suspended: Option<bool>,
    known_players: BTreeMap<String, bool>,
    /// Playback reported by a socket command, kept separate from MPRIS.
    socket_playing: bool,
    /// The startup picture has been seen. Players already playing then do not claim.
    players_primed: bool,
    /// This host gave the seat away. A player that is still reported playing does not take it back.
    yielded: bool,
}

/// One MPRIS player the audio server can see right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerSeen {
    pub service: String,
    pub playing: bool,
}

impl Audio {
    pub fn new() -> Self {
        Self {
            offers: Vec::new(),
            selected: None,
            local_playing: false,
            routed: false,
            observers: BTreeMap::new(),
            tokens: VecDeque::new(),
            token_set: BTreeSet::new(),
            duck: Duck { saved: None, active: false },
            ears: Ears {
                left: None,
                right: None,
                settled: Removal::None,
                pending: None,
                paused_by_us: false,
            },
            conversation_suspended: None,
            known_players: BTreeMap::new(),
            socket_playing: false,
            players_primed: false,
            yielded: false,
        }
    }

    /// Stop treating current playback as a claim. Returns the MPRIS players that are playing.
    pub fn yield_playback(&mut self) -> Vec<String> {
        let playing = self
            .known_players
            .iter()
            .filter(|(_, on)| **on)
            .map(|(service, _)| service.clone())
            .collect();
        self.yielded = true;
        self.socket_playing = false;
        self.recompute_local();
        playing
    }

    /// A new claim may treat players as playing again. It does not invent a play edge.
    pub fn clear_yield(&mut self) {
        self.yielded = false;
        self.recompute_local();
    }

    pub fn yielded(&self) -> bool {
        self.yielded
    }

    #[cfg(test)]
    pub fn set_local_playing_for_test(&mut self, playing: bool) {
        self.local_playing = playing;
    }

    pub fn player_playing(&self, service: &str) -> bool {
        self.known_players.get(service).copied().unwrap_or(false)
    }

    pub fn local_playing(&self) -> bool {
        self.local_playing
    }

    /// An observed desktop player can explicitly pause even while the accessory
    /// retains its previous nonzero source category. Absence alone is not a pause.
    pub fn observed_playback(&self) -> Option<bool> {
        (!self.known_players.is_empty() || self.socket_playing).then_some(self.local_playing)
    }

    pub fn routed(&self) -> bool {
        self.routed
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn set_offers(&mut self, offers: Vec<Profile>) {
        self.offers = offers;
    }

    pub fn activate(&mut self) -> Option<&str> {
        self.selected = best_playback(&self.offers).map(|profile| profile.name.clone());
        self.selected.as_deref()
    }

    pub fn set_routed(&mut self, routed: bool) {
        self.routed = routed;
    }

    pub fn player_event(&mut self, event: &PlaybackEvent, now: UnixMs) -> PlayReply {
        if event.application.is_empty() || event.application.chars().count() > 256 {
            return PlayReply::InvalidOrStale;
        }
        let age = now.0 as i64 - event.time.0 as i64;
        if !(-250..=2_000).contains(&age) {
            return PlayReply::InvalidOrStale;
        }
        match event.kind {
            PlayKind::Heartbeat => {
                self.observers.insert(event.application.clone(), now);
                PlayReply::Ok
            }
            PlayKind::Reset => {
                self.observers.remove(&event.application);
                PlayReply::Ok
            }
            PlayKind::Play | PlayKind::Pause | PlayKind::Snapshot => {
                if event.token.is_empty()
                    || event.player.is_empty()
                    || event.token.chars().count() > 512
                    || event.player.chars().count() > 512
                {
                    return PlayReply::InvalidEvent;
                }
                if !self.token_set.insert(event.token.clone()) {
                    return PlayReply::Duplicate;
                }
                self.tokens.push_back(event.token.clone());
                if self.tokens.len() > TOKEN_LIMIT {
                    if let Some(old) = self.tokens.pop_front() {
                        self.token_set.remove(&old);
                    }
                }
                self.observers.insert(event.application.clone(), now);
                if event.kind == PlayKind::Play {
                    self.yielded = false;
                    self.ears.paused_by_us = false;
                }
                self.socket_playing = match event.kind {
                    PlayKind::Play => true,
                    PlayKind::Pause => false,
                    PlayKind::Snapshot => event.playing,
                    PlayKind::Heartbeat | PlayKind::Reset => self.socket_playing,
                };
                self.recompute_local();
                PlayReply::Accepted { claim: event.kind == PlayKind::Play }
            }
        }
    }

    /// The first picture. Players already playing do not claim.
    pub fn startup_players(&mut self, seen: &[PlayerSeen]) {
        self.players_primed = true;
        self.known_players.clear();
        for player in seen {
            if player.service.is_empty() || player.service.chars().count() > 256 {
                continue;
            }
            self.known_players.insert(player.service.clone(), player.playing);
        }
        self.recompute_local();
    }

    /// One player changed. A transition to playing after the startup picture claims.
    /// One player pausing leaves `local_playing` set while another is still playing.
    /// After a yield, a player that was already playing does not claim again.
    pub fn update_player(&mut self, service: &str, playing: Option<bool>) -> bool {
        if service.is_empty() || service.chars().count() > 256 {
            return false;
        }
        if !self.players_primed {
            self.players_primed = true;
            if let Some(playing) = playing {
                self.known_players.insert(service.to_string(), playing);
            }
            self.recompute_local();
            return false;
        }
        let was = self.known_players.get(service).copied();
        let claim = match playing {
            Some(now_playing) => {
                let fresh = now_playing && was != Some(true);
                self.known_players.insert(service.to_string(), now_playing);
                if fresh {
                    // The user's new play supersedes an earlier removal pause.
                    // Snapshots and repeated Playing reports do not clear it.
                    self.ears.paused_by_us = false;
                }
                if self.yielded {
                    if was != Some(true) && now_playing {
                        self.yielded = false;
                        true
                    } else {
                        false
                    }
                } else {
                    fresh
                }
            }
            None => {
                self.known_players.remove(service);
                false
            }
        };
        self.recompute_local();
        claim
    }

    /// MPRIS names stay fresh without a synthetic playback event.
    pub fn touch_players(&mut self, now: UnixMs) {
        let names: Vec<String> = self.known_players.keys().cloned().collect();
        for name in names {
            self.observers.insert(name, now);
        }
    }

    fn recompute_local(&mut self) {
        if self.ears.paused_by_us {
            return;
        }
        let mpris = self.known_players.values().any(|playing| *playing);
        self.local_playing = self.socket_playing || mpris;
    }

    pub fn observer_fresh(&self, application: &str, now: UnixMs) -> bool {
        self.observers
            .get(application)
            .is_some_and(|seen| now.0.saturating_sub(seen.0) <= OBSERVER_TTL_MS)
    }

    pub fn begin_duck(&mut self, current: f64) {
        if !self.duck.active {
            self.duck.saved = Some(current);
            self.duck.active = true;
        }
    }

    pub fn end_duck(&mut self) -> Option<f64> {
        self.duck.active = false;
        self.duck.saved.take()
    }

    /// A notification caused by our own duck must not replace the saved level.
    pub fn user_volume(&mut self, level: f64) {
        if !self.duck.active {
            self.duck.saved = Some(level);
        }
    }

    pub fn suspend_conversation(&mut self, currently_on: bool) {
        if self.conversation_suspended.is_none() {
            self.conversation_suspended = Some(currently_on);
        }
    }

    pub fn resume_conversation(&mut self) -> Option<bool> {
        self.conversation_suspended.take()
    }

    /// Closely spaced ear reports wait out the debounce before pausing or resuming.
    /// Resume only happens for playback this daemon paused.
    pub fn ears(
        &mut self,
        left: Option<bool>,
        right: Option<bool>,
        behavior: EarBehavior,
        now: MonoMs,
    ) -> EarAction {
        self.ears.left = left;
        self.ears.right = right;
        let Some(removal) = removal(left, right) else {
            self.ears.pending = None;
            return EarAction::Hold;
        };
        if removal != self.ears.settled {
            match self.ears.pending {
                Some((pending, since))
                    if pending == removal && now.saturating_since(since) >= EAR_DEBOUNCE_MS =>
                {
                    self.ears.settled = removal;
                    self.ears.pending = None;
                    self.ear_effect(behavior)
                }
                Some((pending, _)) if pending == removal => EarAction::Hold,
                _ => {
                    self.ears.pending = Some((removal, now));
                    EarAction::Hold
                }
            }
        } else {
            self.ears.pending = None;
            EarAction::Hold
        }
    }

    pub fn ears_tick(&mut self, behavior: EarBehavior, now: MonoMs) -> EarAction {
        let Some((removal, since)) = self.ears.pending else {
            return EarAction::Hold;
        };
        if now.saturating_since(since) < EAR_DEBOUNCE_MS {
            return EarAction::Hold;
        }
        self.ears.settled = removal;
        self.ears.pending = None;
        self.ear_effect(behavior)
    }

    fn ear_effect(&mut self, behavior: EarBehavior) -> EarAction {
        let should_pause = match behavior {
            EarBehavior::Disabled => false,
            EarBehavior::PauseOne => self.ears.settled != Removal::None,
            EarBehavior::PauseBoth => self.ears.settled == Removal::Both,
        };
        if should_pause {
            if self.local_playing {
                self.ears.paused_by_us = true;
                self.local_playing = false;
                EarAction::Pause
            } else {
                EarAction::Hold
            }
        } else if self.ears.paused_by_us {
            self.ears.paused_by_us = false;
            self.local_playing = true;
            EarAction::Resume
        } else {
            EarAction::Hold
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

fn removal(left: Option<bool>, right: Option<bool>) -> Option<Removal> {
    match (left, right) {
        (Some(true), Some(true)) => Some(Removal::None),
        (Some(false), Some(false)) => Some(Removal::Both),
        (Some(_), Some(_)) => Some(Removal::One),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, description: &str, priority: i32, sources: u32) -> Profile {
        Profile {
            name: name.into(),
            description: description.into(),
            priority,
            available: true,
            sinks: 1,
            sources,
        }
    }

    #[test]
    fn activation_picks_the_highest_ranked_stereo_profile_every_time() {
        let offers = vec![
            profile("a2dp-sink-aac", "High Fidelity Playback (A2DP Sink, codec AAC)", 50, 0),
            profile("a2dp-sink-sbc_xq", "High Fidelity Playback (A2DP Sink, codec SBC-XQ)", 10, 0),
            profile("headset-head-unit-msbc", "Headset Head Unit (HSP/HFP, codec mSBC)", 100, 1),
        ];
        assert_eq!(best_playback(&offers).unwrap().name, "a2dp-sink-sbc_xq");
        let without_xq =
            vec![profile("a2dp-sink-aac", "", 80, 0), profile("a2dp-sink-sbc", "", 1, 0)];
        assert_eq!(best_playback(&without_xq).unwrap().name, "a2dp-sink-sbc");
        let only_aac = vec![profile("a2dp-sink-aac", "", 1, 0)];
        assert_eq!(best_playback(&only_aac).unwrap().name, "a2dp-sink-aac");

        let mut audio = Audio::new();
        audio.set_offers(offers);
        assert_eq!(audio.activate(), Some("a2dp-sink-sbc_xq"));
        audio.set_offers(without_xq);
        assert_eq!(audio.activate(), Some("a2dp-sink-sbc"));
    }

    #[test]
    fn volume_steps_and_duck_restore_the_pre_duck_level() {
        assert_eq!(snap_percent(7, 5), 5);
        assert_eq!(snap_percent(8, 5), 10);
        assert_eq!(snap_percent(140, 5), 100);
        let mut audio = Audio::new();
        audio.begin_duck(0.8);
        audio.user_volume(0.16);
        assert_eq!(audio.end_duck(), Some(0.8));
    }

    #[test]
    fn a_fresh_play_overrides_an_ear_pause_but_a_snapshot_does_not() {
        for browser in [false, true] {
            let mut audio = Audio::new();
            let service = "org.mpris.MediaPlayer2.brave";
            audio.startup_players(&[]);
            assert!(audio.update_player(service, Some(true)));
            audio.ears(Some(false), Some(true), EarBehavior::PauseOne, MonoMs(0));
            assert_eq!(audio.ears_tick(EarBehavior::PauseOne, MonoMs(400)), EarAction::Pause);
            audio.update_player(service, Some(false));
            let mut event = PlaybackEvent {
                kind: PlayKind::Snapshot,
                application: "browser".into(),
                time: UnixMs(1_000),
                player: "tab".into(),
                token: "snapshot".into(),
                playing: true,
            };
            assert_eq!(
                audio.player_event(&event, UnixMs(1_000)),
                PlayReply::Accepted { claim: false }
            );
            assert!(!audio.local_playing());
            if browser {
                event.kind = PlayKind::Play;
                event.token = "play".into();
                assert_eq!(
                    audio.player_event(&event, UnixMs(1_000)),
                    PlayReply::Accepted { claim: true }
                );
            } else {
                assert!(audio.update_player(service, Some(true)), "a new MPRIS play must claim");
            }
            assert!(audio.local_playing(), "the old ear pause must not suppress explicit play");
            assert_eq!(
                audio.ears(Some(false), Some(true), EarBehavior::PauseOne, MonoMs(500)),
                EarAction::Hold
            );
            assert!(audio.local_playing(), "unchanged placement must not pause it again");
        }
    }

    #[test]
    fn a_flicker_does_not_pause_and_only_our_pause_resumes() {
        let mut audio = Audio::new();
        audio.player_event(
            &PlaybackEvent {
                kind: PlayKind::Play,
                application: "browser".into(),
                time: UnixMs(1_000),
                player: "tab".into(),
                token: "t1".into(),
                playing: true,
            },
            UnixMs(1_000),
        );
        assert_eq!(
            audio.ears(Some(false), Some(true), EarBehavior::PauseOne, MonoMs(0)),
            EarAction::Hold
        );
        assert_eq!(
            audio.ears(Some(true), Some(true), EarBehavior::PauseOne, MonoMs(50)),
            EarAction::Hold
        );
        assert!(audio.local_playing());
        assert_eq!(
            audio.ears(Some(false), Some(true), EarBehavior::PauseOne, MonoMs(1_000)),
            EarAction::Hold
        );
        assert_eq!(audio.ears_tick(EarBehavior::PauseOne, MonoMs(1_400)), EarAction::Pause);
        assert!(!audio.local_playing());
        audio.player_event(
            &PlaybackEvent {
                kind: PlayKind::Pause,
                application: "browser".into(),
                time: UnixMs(1_500),
                player: "tab".into(),
                token: "t2".into(),
                playing: false,
            },
            UnixMs(1_500),
        );
        // The user paused after we did. Reinsertion must not steal playback back
        // once our own pause was already overtaken... we still paused_by_us.
        // The second event sets local_playing false. Resume still belongs to us
        // because we paused first. That matches "resume only what we paused".
        assert_eq!(
            audio.ears(Some(true), Some(true), EarBehavior::PauseOne, MonoMs(2_000)),
            EarAction::Hold
        );
        assert_eq!(audio.ears_tick(EarBehavior::PauseOne, MonoMs(2_400)), EarAction::Resume);
    }

    #[test]
    fn snapshots_do_not_claim_and_stale_events_are_rejected() {
        let mut audio = Audio::new();
        let snapshot = audio.player_event(
            &PlaybackEvent {
                kind: PlayKind::Snapshot,
                application: "browser".into(),
                time: UnixMs(5_000),
                player: "tab".into(),
                token: "s".into(),
                playing: true,
            },
            UnixMs(5_000),
        );
        assert_eq!(snapshot, PlayReply::Accepted { claim: false });
        assert!(audio.local_playing());
        let stale = audio.player_event(
            &PlaybackEvent {
                kind: PlayKind::Play,
                application: "browser".into(),
                time: UnixMs(0),
                player: "tab".into(),
                token: "late".into(),
                playing: true,
            },
            UnixMs(5_000),
        );
        assert_eq!(stale, PlayReply::InvalidOrStale);
        let dup = audio.player_event(
            &PlaybackEvent {
                kind: PlayKind::Play,
                application: "browser".into(),
                time: UnixMs(5_000),
                player: "tab".into(),
                token: "s".into(),
                playing: true,
            },
            UnixMs(5_000),
        );
        assert_eq!(dup, PlayReply::Duplicate);
    }

    #[test]
    fn the_first_sight_of_a_player_does_not_claim_and_one_pause_keeps_another_playing() {
        let mut audio = Audio::new();
        audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.spotify".into(),
            playing: true,
        }]);
        assert!(audio.local_playing());
        assert!(!audio.update_player("org.mpris.MediaPlayer2.spotify", Some(false)));
        assert!(!audio.local_playing());
        assert!(audio.update_player("org.mpris.MediaPlayer2.spotify", Some(true)));

        let mut later = Audio::new();
        later.startup_players(&[]);
        assert!(later.update_player("org.mpris.MediaPlayer2.firefox", Some(true)));
        assert!(later.update_player("org.mpris.MediaPlayer2.rhythmbox", Some(true)));
        assert!(!later.update_player("org.mpris.MediaPlayer2.firefox", Some(false)));
        assert!(later.local_playing());
    }

    #[test]
    fn a_yield_holds_playback_until_a_new_play() {
        let mut audio = Audio::new();
        audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.spotify".into(),
            playing: true,
        }]);
        let paused = audio.yield_playback();
        assert_eq!(paused, vec!["org.mpris.MediaPlayer2.spotify".to_string()]);
        assert!(audio.local_playing(), "the pause has not landed yet");
        assert!(!audio.update_player("org.mpris.MediaPlayer2.spotify", Some(true)));
        assert!(audio.local_playing());
        assert!(!audio.update_player("org.mpris.MediaPlayer2.spotify", Some(false)));
        assert!(!audio.local_playing());
        assert!(audio.update_player("org.mpris.MediaPlayer2.spotify", Some(true)));
        assert!(audio.local_playing());
    }
}
