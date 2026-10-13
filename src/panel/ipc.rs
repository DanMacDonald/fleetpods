//! The socket framing the current panel, CLI, and browser observer already speak.
//! Simple verbs arrive whole. JSON families are newline terminated and capped at 8 KiB.

use serde_json::Value;

use crate::types::{
    valid_name, Address, Code, EarBehavior, InvitationId, NoiseMode, PlayKind, PlaybackEvent,
    UnixMs,
};

pub const MAX_BYTES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FleetCommand {
    Remove { address: Address },
    Invite { address: Address, name: String },
    Join { address: Address, name: String, leave: bool },
    Submit { id: InvitationId, code: Code },
    LeaveInvitation { id: InvitationId },
    Cancel { id: InvitationId },
    Bad,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Status,
    FleetReset,
    Activity,
    Connect,
    Disconnect,
    ReconnectPeer,
    Forget,
    Reopen,
    Noise(NoiseMode),
    NoiseCycle,
    Ear(EarBehavior),
    Conversation(bool),
    OneBud(bool),
    Adaptive(u8),
    HandoffClaim,
    HandoffRelease,
    ScanPause,
    ScanResume,
    Solo,
    Priority(String),
    Incoming(Result<Value, ()>),
    Fleet(Result<FleetCommand, ()>),
    Playback(Result<PlaybackEvent, ()>),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    NeedMore,
    TooLarge,
    Ready(Command),
}

pub struct Response {
    pub body: Option<String>,
}

impl Response {
    pub fn none() -> Self {
        Self { body: None }
    }

    pub fn line(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Self { body: Some(text) }
    }
}

/// `true` when the existing CLI blocks until a reply.
pub fn cli_waits(command: &str) -> bool {
    matches!(
        command,
        "fleet"
            | "fleet-reset"
            | "connect"
            | "reconnect-peer"
            | "activity"
            | "handoff:claim"
            | "handoff:release"
            | "status"
            | "reopen"
            | "scan:pause"
            | "scan:resume"
    )
}

pub fn push(bytes: &[u8]) -> Incoming {
    if bytes.len() > MAX_BYTES {
        return Incoming::TooLarge;
    }
    if framed_prefix(bytes) && !bytes.ends_with(b"\n") {
        return Incoming::NeedMore;
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches('\n');
    Incoming::Ready(parse_text(text))
}

fn framed_prefix(bytes: &[u8]) -> bool {
    for prefix in [b"playback:".as_slice(), b"priority:", b"incoming:", b"fleet:"] {
        if bytes.starts_with(prefix) || prefix.starts_with(bytes) {
            return true;
        }
    }
    false
}

fn parse_text(text: &str) -> Command {
    if let Some(json) = text.strip_prefix("fleet:") {
        return Command::Fleet(parse_fleet(json));
    }
    if let Some(json) = text.strip_prefix("incoming:") {
        return Command::Incoming(serde_json::from_str(json).map_err(|_| ()));
    }
    if let Some(json) = text.strip_prefix("playback:") {
        return Command::Playback(parse_playback(json));
    }
    if let Some(level) = text.strip_prefix("adaptive:") {
        if let Some(level) = parse_level(level) {
            return Command::Adaptive(level);
        }
        return Command::Unknown;
    }
    if let Some(payload) = text.strip_prefix("priority:") {
        return Command::Priority(payload.trim().to_string());
    }
    match text {
        "status" => Command::Status,
        "fleet-reset" => Command::FleetReset,
        "activity" => Command::Activity,
        "connect" => Command::Connect,
        "disconnect" => Command::Disconnect,
        "reconnect-peer" => Command::ReconnectPeer,
        "forget" => Command::Forget,
        "reopen" => Command::Reopen,
        "noise:off" => Command::Noise(NoiseMode::Off),
        "noise:anc" => Command::Noise(NoiseMode::Anc),
        "noise:transparency" => Command::Noise(NoiseMode::Transparency),
        "noise:adaptive" => Command::Noise(NoiseMode::Adaptive),
        "noise:cycle" => Command::NoiseCycle,
        "ear:one" => Command::Ear(EarBehavior::PauseOne),
        "ear:both" => Command::Ear(EarBehavior::PauseBoth),
        "ear:off" => Command::Ear(EarBehavior::Disabled),
        "ca:on" => Command::Conversation(true),
        "ca:off" => Command::Conversation(false),
        "onebud:on" => Command::OneBud(true),
        "onebud:off" => Command::OneBud(false),
        "handoff:claim" => Command::HandoffClaim,
        "handoff:release" => Command::HandoffRelease,
        "scan:pause" => Command::ScanPause,
        "scan:resume" => Command::ScanResume,
        "solo" => Command::Solo,
        _ => Command::Unknown,
    }
}

fn parse_level(payload: &str) -> Option<u8> {
    if payload.is_empty() || !payload.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    payload.parse::<u16>().ok().and_then(|n| u8::try_from(n).ok()).filter(|n| *n <= 100)
}

fn parse_fleet(json: &str) -> Result<FleetCommand, ()> {
    let value: Value = serde_json::from_str(json).map_err(|_| ())?;
    let Some(object) = value.as_object() else {
        return Err(());
    };
    let action = object.get("action").and_then(Value::as_str).unwrap_or("");
    let command = match action {
        "remove" => match object.get("address").and_then(Value::as_str).and_then(Address::parse) {
            Some(address) => FleetCommand::Remove { address },
            None => FleetCommand::Bad,
        },
        "invite" | "join" => {
            let address = object.get("address").and_then(Value::as_str).and_then(Address::parse);
            let name = object.get("name").and_then(Value::as_str).filter(|name| valid_name(name));
            match (address, name) {
                (Some(address), Some(name)) => {
                    if action == "join" {
                        FleetCommand::Join {
                            address,
                            name: name.to_string(),
                            leave: object.get("leave").and_then(Value::as_bool).unwrap_or(false),
                        }
                    } else {
                        FleetCommand::Invite { address, name: name.to_string() }
                    }
                }
                _ => FleetCommand::Bad,
            }
        }
        "submit" => {
            let id = object.get("id").and_then(Value::as_str).and_then(InvitationId::parse_hex);
            let code = object.get("code").and_then(Value::as_str).and_then(Code::parse);
            match (id, code) {
                (Some(id), Some(code)) => FleetCommand::Submit { id, code },
                _ => FleetCommand::Bad,
            }
        }
        "cancel" | "leave_invitation" => {
            match object.get("id").and_then(Value::as_str).and_then(InvitationId::parse_hex) {
                Some(id) => {
                    if action == "leave_invitation" {
                        FleetCommand::LeaveInvitation { id }
                    } else {
                        FleetCommand::Cancel { id }
                    }
                }
                None => FleetCommand::Bad,
            }
        }
        _ => FleetCommand::Bad,
    };
    Ok(command)
}

fn parse_playback(json: &str) -> Result<PlaybackEvent, ()> {
    let value: Value = serde_json::from_str(json).map_err(|_| ())?;
    let Some(object) = value.as_object() else {
        return Err(());
    };
    let kind_text = object.get("kind").and_then(Value::as_str).unwrap_or("");
    let known = matches!(kind_text, "heartbeat" | "reset" | "play" | "pause" | "snapshot");
    let kind = match kind_text {
        "reset" => PlayKind::Reset,
        "play" => PlayKind::Play,
        "pause" => PlayKind::Pause,
        "snapshot" => PlayKind::Snapshot,
        _ => PlayKind::Heartbeat,
    };
    // An unknown kind keeps an empty token so `audio::play` answers `invalid_event`
    // once the timestamp itself is plausible.
    let token = if known {
        object.get("token").and_then(Value::as_str).unwrap_or("").to_string()
    } else {
        String::new()
    };
    let player = if known {
        object.get("player").and_then(Value::as_str).unwrap_or("").to_string()
    } else {
        String::new()
    };
    Ok(PlaybackEvent {
        kind: if known { kind } else { PlayKind::Play },
        application: object.get("application").and_then(Value::as_str).unwrap_or("").to_string(),
        time: UnixMs(object.get("time").and_then(Value::as_u64).unwrap_or(0)),
        player,
        token,
        playing: object.get("playing").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_json_waits_and_a_verb_does_not() {
        assert_eq!(push(b"fleet:"), Incoming::NeedMore);
        assert_eq!(push(b"f"), Incoming::NeedMore);
        assert!(matches!(push(b"status"), Incoming::Ready(Command::Status)));
        assert!(matches!(push(b"adaptive:101"), Incoming::Ready(Command::Unknown)));
        assert!(matches!(push(b"adaptive: 5"), Incoming::Ready(Command::Unknown)));
        assert!(matches!(push(b"adaptive:5"), Incoming::Ready(Command::Adaptive(5))));
        let line = b"playback:{\"kind\":\"play\",\"application\":\"Browser\",\"time\":10,\"player\":\"tab\",\"token\":\"t\"}\n";
        assert!(matches!(push(line), Incoming::Ready(Command::Playback(Ok(_)))));
        assert!(matches!(
            push(b"fleet:{}\n"),
            Incoming::Ready(Command::Fleet(Ok(FleetCommand::Bad)))
        ));
        assert!(matches!(push(b"fleet:nope\n"), Incoming::Ready(Command::Fleet(Err(())))));
        assert_eq!(push(&vec![b'x'; 8193]), Incoming::TooLarge);
    }

    #[test]
    fn fleet_json_keeps_the_code_text() {
        let raw = b"fleet:{\"action\":\"submit\",\"id\":\"00112233445566778899aabbccddeeff\",\"code\":\"012345\"}\n";
        match push(raw) {
            Incoming::Ready(Command::Fleet(Ok(FleetCommand::Submit { code, .. }))) => {
                assert_eq!(code.display(), "012345");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fleet_reset_has_a_reply_and_is_distinct_from_forget() {
        assert_eq!(push(b"fleet-reset"), Incoming::Ready(Command::FleetReset));
        assert_eq!(push(b"forget"), Incoming::Ready(Command::Forget));
        assert!(cli_waits("fleet-reset"));
    }
}
