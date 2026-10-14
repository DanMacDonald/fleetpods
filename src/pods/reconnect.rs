//! The Apple reconnect cadence.
//!
//! Pause local audio, ask seated fleet peers to leave, drop this link, then
//! rejoin. The state machine owns the phases and their deadlines; the daemon
//! supplies the facts and performs the returned action.

use crate::types::MonoMs;

const PAUSE_WAIT_MS: u64 = 3_000;
pub(crate) const PEER_LEAVE_MS: u64 = 5_000;
const DROP_WAIT_MS: u64 = 5_000;
/// How long the pods sit fully free before this laptop rejoins. In the
/// reconnect logs, a laptop back within ~3 s of the drop landed with the
/// iPhone already in the roster only about half the time, while at 5 s or
/// more it was nearly always there: an Apple device needs a few seconds
/// to notice the free slot and take it.
const REJOIN_DELAY_MS: u64 = 5_000;
const RETURN_WAIT_MS: u64 = 15_000;
pub(crate) const PEER_STAYED: &str =
    "Omarchy peer did not disconnect; update that peer or disconnect it manually";

/// What the daemon must do after a phase transition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Advance {
    None,
    SendRequests,
    DropLink,
    Rejoin,
    Done,
    Fail(&'static str),
}

/// The facts one tick is decided from.
#[derive(Clone, Copy)]
pub struct Facts {
    pub quiet: bool,
    pub peers_left: bool,
    pub link_down: bool,
    pub link_ready: bool,
    /// The link layer is mid-attempt (connecting, handshaking, or between
    /// its own retries): its deadlines, not the return wait, judge it.
    pub attempting: bool,
}

enum State {
    Idle,
    Pausing {
        until: MonoMs,
    },
    /// The other laptops were asked to leave. This link stays up until they do.
    Awaiting {
        until: MonoMs,
    },
    Dropping {
        until: MonoMs,
    },
    Waiting {
        rejoin_at: MonoMs,
        until: MonoMs,
        sent: bool,
    },
}

pub struct Reconnect {
    state: State,
}

impl Reconnect {
    pub fn new() -> Self {
        Self { state: State::Idle }
    }

    pub fn begin(&mut self, now: MonoMs) {
        self.state = State::Pausing { until: now.saturating_add(PAUSE_WAIT_MS) };
    }

    pub fn cancel(&mut self) {
        self.state = State::Idle;
    }

    pub fn advance(&mut self, now: MonoMs, facts: Facts) -> Advance {
        match self.state {
            State::Idle => Advance::None,
            State::Pausing { until } => {
                if !facts.quiet && now.0 < until.0 {
                    return Advance::None;
                }
                if !facts.quiet {
                    self.state = State::Idle;
                    return Advance::Fail("local audio did not pause");
                }
                self.state = State::Awaiting { until: now.saturating_add(PEER_LEAVE_MS) };
                Advance::SendRequests
            }
            State::Awaiting { until } => {
                if facts.peers_left {
                    self.state = State::Dropping { until: now.saturating_add(DROP_WAIT_MS) };
                    Advance::DropLink
                } else if now.0 >= until.0 {
                    self.state = State::Idle;
                    Advance::Fail(PEER_STAYED)
                } else {
                    Advance::None
                }
            }
            State::Dropping { until } => {
                if facts.link_down {
                    self.state = State::Waiting {
                        rejoin_at: now.saturating_add(REJOIN_DELAY_MS),
                        until: now.saturating_add(RETURN_WAIT_MS),
                        sent: false,
                    };
                    Advance::None
                } else if now.0 >= until.0 {
                    self.state = State::Idle;
                    Advance::Fail("link did not drop")
                } else {
                    Advance::None
                }
            }
            State::Waiting { rejoin_at, until, sent } => {
                if facts.link_ready {
                    self.state = State::Idle;
                    Advance::Done
                } else if !sent && now.0 >= rejoin_at.0 {
                    self.state = State::Waiting { rejoin_at, until, sent: true };
                    Advance::Rejoin
                } else if now.0 >= until.0 && !facts.attempting {
                    self.state = State::Idle;
                    Advance::Fail("link did not return")
                } else {
                    Advance::None
                }
            }
        }
    }
}

impl Default for Reconnect {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(link_down: bool) -> Facts {
        Facts { quiet: true, peers_left: true, link_down, link_ready: false, attempting: false }
    }

    fn attempting(link_down: bool) -> Facts {
        Facts { attempting: true, ..facts(link_down) }
    }

    #[test]
    fn the_pods_sit_free_for_the_rejoin_delay_before_rejoining() {
        let mut reconnect = Reconnect::new();
        let start = MonoMs(10_000);
        reconnect.begin(start);
        assert_eq!(reconnect.advance(start, facts(false)), Advance::SendRequests);
        assert_eq!(reconnect.advance(start, facts(false)), Advance::DropLink);
        let down = start.saturating_add(500);
        assert_eq!(reconnect.advance(down, facts(true)), Advance::None);
        // Still inside the free window: no rejoin yet.
        assert_eq!(
            reconnect.advance(down.saturating_add(REJOIN_DELAY_MS - 1), facts(true)),
            Advance::None
        );
        assert_eq!(
            reconnect.advance(down.saturating_add(REJOIN_DELAY_MS), facts(true)),
            Advance::Rejoin
        );
    }

    #[test]
    fn an_attempt_in_flight_is_not_failed_by_the_return_wait() {
        let mut reconnect = Reconnect::new();
        let start = MonoMs(10_000);
        reconnect.begin(start);
        assert_eq!(reconnect.advance(start, facts(false)), Advance::SendRequests);
        assert_eq!(reconnect.advance(start, facts(false)), Advance::DropLink);
        let down = start.saturating_add(500);
        assert_eq!(reconnect.advance(down, facts(true)), Advance::None);
        assert_eq!(
            reconnect.advance(down.saturating_add(REJOIN_DELAY_MS), facts(true)),
            Advance::Rejoin
        );
        // The link layer is still working on the attempt when the wait
        // expires (a first attempt failed and a retry is mid-setup): the
        // cadence must not record a failure under it.
        assert_eq!(
            reconnect.advance(down.saturating_add(RETURN_WAIT_MS + 1_000), attempting(true)),
            Advance::None
        );
        // Once no attempt is in flight, the expired wait fails as before.
        assert_eq!(
            reconnect.advance(down.saturating_add(RETURN_WAIT_MS + 2_000), facts(true)),
            Advance::Fail("link did not return")
        );
    }
}
