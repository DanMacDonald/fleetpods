//! Fleet wire constants.
//! Codecs and cryptography land here. They do not own enrollment or routing outcomes.
//!
//! The enrollment label, the session label, and the keyring lookup are shared
//! with the previous daemon. They stay until FleetPods only enrolls with itself.

use serde_json::{json, Value};

use crate::types::{Address, InvitationId};

/// Connectionless fleet advertisements. No GATT service and no LAN transport.
pub const FLEET_SERVICE_UUID: &str = "bf3f771e-2938-4a3e-b227-746d5d706f64";

/// Prefix of the enrollment SPAKE2 context.
pub const ENROLLMENT_CONTEXT: &str = "omnipods-fleet-v1";

/// Secret Service schema for the fleet item. Lookup uses this with [`KEYRING_APP`].
pub const KEYRING_SCHEMA: &str = "io.github.danmacdonald.omnipods.Pairing";

/// Secret Service attribute `application`.
pub const KEYRING_APP: &str = "omnipods";

/// HKDF domain for sealed presence. Shared with the previous daemon.
pub const PRESENCE_DOMAIN: &[u8] = b"omnipods-presence-v1";

/// HKDF domain for sealed roster advertisements. Shared with the previous daemon.
pub const ROSTER_DOMAIN: &[u8] = b"omnipods-roster-v1";

/// HKDF domain for sealed routing advertisements. Shared with the previous daemon.
pub const ROUTE_DOMAIN: &[u8] = b"omnipods-routing-v1";

/// SPAKE2 identity for one role: `{context}/{role}/` plus the enrollment transcript.
pub fn role_identity(role: &str, context: &[u8]) -> Vec<u8> {
    let mut identity =
        Vec::with_capacity(ENROLLMENT_CONTEXT.len() + role.len() + 2 + context.len());
    identity.extend_from_slice(ENROLLMENT_CONTEXT.as_bytes());
    identity.push(b'/');
    identity.extend_from_slice(role.as_bytes());
    identity.push(b'/');
    identity.extend_from_slice(context);
    identity
}

/// HKDF info for the enrollment session key.
pub fn session_label() -> Vec<u8> {
    let mut label = ENROLLMENT_CONTEXT.as_bytes().to_vec();
    label.extend_from_slice(b"/session");
    label
}

/// Compact JSON array used as the SPAKE2 context.
///
/// Both roles produce the inviter's address and name first. `attempt` is omitted
/// while it is zero and appended as a number after that. This matches the C++
/// field order for ASCII names. Capture a fixture from the pinned daemon before
/// treating the bytes as a cryptographic input.
pub fn enrollment_context(
    id: InvitationId,
    initiator: bool,
    local: Address,
    peer: Address,
    local_name: &str,
    peer_name: &str,
    attempt: u8,
) -> Vec<u8> {
    let (inviter_addr, joiner_addr, inviter_name, joiner_name) = if initiator {
        (local, peer, local_name, peer_name)
    } else {
        (peer, local, peer_name, local_name)
    };
    let mut items = vec![
        json!(ENROLLMENT_CONTEXT),
        json!(id.hex()),
        json!(inviter_addr.display()),
        json!(joiner_addr.display()),
        json!(inviter_name),
        json!(joiner_name),
    ];
    if attempt > 0 {
        items.push(json!(attempt));
    }
    serde_json::to_vec(&Value::Array(items)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_roles_bind_the_same_context() {
        let id = InvitationId::new([1; 16]);
        let gram = Address::parse("11:22:33:44:55:66").unwrap();
        let xps = Address::parse("AA:BB:CC:DD:EE:FF").unwrap();
        let from_inviter = enrollment_context(id, true, gram, xps, "gram", "xps", 0);
        let from_joiner = enrollment_context(id, false, xps, gram, "xps", "gram", 0);
        assert_eq!(from_inviter, from_joiner);
        let text = String::from_utf8(from_inviter).unwrap();
        assert!(text.starts_with(&format!("[\"{ENROLLMENT_CONTEXT}\"")));
        assert!(!text.contains(' '));
        let retried = enrollment_context(id, true, gram, xps, "gram", "xps", 2);
        assert!(String::from_utf8(retried).unwrap().ends_with(",2]"));
    }
}
