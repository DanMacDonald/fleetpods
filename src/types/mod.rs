//! Shared words: identity, time, and evidence.
//! These types do not talk to BlueZ, PipeWire, or the keyring.

pub mod address;
pub mod code;
pub mod evidence;
pub mod id;
pub mod listening;
pub mod model;
pub mod playback;
pub mod time;

pub use address::{valid_name, Address};
pub use code::Code;
pub use evidence::Evidence;
pub use id::{FleetId, Generation, HostId, InvitationId, OperationId, SessionId};
pub use listening::{Battery, EarBehavior, Lid, LidState, NoiseMode};
pub use model::Model;
pub use playback::{PlayKind, PlaybackEvent};
pub use time::{Clock, MonoMs, UnixMs};
