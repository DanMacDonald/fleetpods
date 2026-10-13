//! Browser and player observations. Freshness is decided by `audio::play`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayKind {
    Heartbeat,
    Reset,
    Play,
    Pause,
    Snapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackEvent {
    pub kind: PlayKind,
    pub application: String,
    pub time: super::UnixMs,
    pub player: String,
    pub token: String,
    pub playing: bool,
}
