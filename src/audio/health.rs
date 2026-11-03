//! Recover an output that remains idle, suspended or in error during playback.
//! A running output does not prove that sound reached the listener. Neither
//! silence nor an idle output alone is evidence for a reset.

use crate::audio::route::Graph;
use crate::types::{Address, MonoMs};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    pub index: u32,
    pub address: Option<Address>,
    pub profile: Option<String>,
    /// Raw Pulse ABI value. PipeWire also reports -2 (init) and -3 (unlinked).
    pub state: i32,
    pub muted: bool,
    pub audible_volume: bool,
}

#[derive(Default)]
pub struct Health {
    pub outputs: Vec<Output>,
    failed: HashSet<(String, u32)>,
    suspect: Option<(u32, MonoMs)>,
    last_reset: Option<MonoMs>,
}

impl Health {
    /// A failed node may subsequently report suspended. Keep that evidence
    /// until this node runs or disappears; a replacement has its own index.
    pub fn observe(&mut self, outputs: Vec<Output>) {
        self.failed.retain(|(name, index)| {
            outputs.iter().any(|o| o.name == *name && o.index == *index && o.state != 0)
        });
        for output in &outputs {
            if output.state == -3 {
                self.failed.insert((output.name.clone(), output.index));
            }
        }
        self.outputs = outputs;
    }

    pub fn failed(&self, address: Address) -> bool {
        self.output(address).is_some_and(|o| self.failed.contains(&(o.name.clone(), o.index)))
    }

    pub fn running(&self, address: Address) -> bool {
        self.output(address).is_some_and(|o| o.state == 0)
    }

    pub fn forget_server(&mut self) {
        self.outputs.clear();
        self.failed.clear();
        self.suspect = None;
    }

    pub fn output(&self, address: Address) -> Option<&Output> {
        self.outputs.iter().find(|output| {
            output.address == Some(address)
                && output.profile.as_deref().is_some_and(|p| p.contains("a2dp-sink"))
        })
    }

    pub fn check(
        &mut self,
        address: Option<Address>,
        graph: Option<&Graph>,
        eligible: bool,
        now: MonoMs,
    ) -> Option<Output> {
        let bad = address.and_then(|address| self.output(address)).filter(|output| {
            eligible
                && matches!(output.state, -3..=-1 | 1..=2)
                && !output.muted
                && output.audible_volume
                && graph.is_some_and(|graph| {
                    graph.streams.iter().any(|stream| {
                        stream.sink == output.name
                            && !stream.corked
                            && !stream.mute
                            && stream.volume > 0.0
                    })
                })
        });
        let Some(output) = bad.cloned() else {
            self.suspect = None;
            return None;
        };
        let since = match self.suspect {
            Some((index, since)) if index == output.index => since,
            _ => {
                self.suspect = Some((output.index, now));
                now
            }
        };
        if now.saturating_since(since) < 3_000
            || self.last_reset.is_some_and(|last| now.saturating_since(last) < 30_000)
        {
            return None;
        }
        Some(output)
    }

    /// Commit the cooldown only once the recovery owner admits a rebuild.
    pub fn started(&mut self, now: MonoMs) {
        self.last_reset = Some(now);
        self.suspect = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::route::LiveStream;

    fn fixture(state: i32) -> (Health, Address, Graph) {
        let address = Address::parse("CC:22:FE:54:DF:C7").unwrap();
        let health = Health {
            outputs: vec![Output {
                name: "pods".into(),
                index: 7,
                address: Some(address),
                profile: Some("a2dp-sink".into()),
                state,
                muted: false,
                audible_volume: true,
            }],
            ..Health::default()
        };
        let graph = Graph {
            default_sink: "pods".into(),
            sinks: vec![],
            cards: vec![],
            streams: vec![LiveStream {
                id: 8,
                generation: 8,
                sink: "pods".into(),
                role: "".into(),
                node: "Brave".into(),
                mute: false,
                volume: 1.0,
                corked: false,
            }],
        };
        (health, address, graph)
    }

    #[test]
    fn failure_survives_suspension_but_not_running_replacement_or_server_loss() {
        for end in ["running", "replacement", "removed", "server"] {
            let (mut health, address, _) = fixture(-3);
            health.observe(health.outputs.clone());
            assert!(health.failed(address));
            let mut output = health.outputs[0].clone();
            output.state = 2;
            health.observe(vec![output.clone()]);
            assert!(health.failed(address), "suspension does not repair the node");
            assert!(!health.running(address));
            assert!(!health.failed(Address::parse("11:22:33:44:55:66").unwrap()));
            match end {
                "running" => {
                    output.state = 0;
                    health.observe(vec![output]);
                    assert!(health.running(address));
                }
                "replacement" => {
                    output.index += 1;
                    health.observe(vec![output]);
                }
                "removed" => health.observe(vec![]),
                _ => health.forget_server(),
            }
            assert!(!health.failed(address), "{end}");
        }
    }

    #[test]
    fn confirmed_failure_has_a_grace_period_and_rate_limit() {
        let (mut health, address, graph) = fixture(2);
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(0)).is_none());
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(2_999)).is_none());
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(3_000)).is_some());
        // Merely requesting recovery must not consume the cooldown.
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(3_000)).is_some());
        health.started(MonoMs(3_000));
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(4_000)).is_none());
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(32_999)).is_none());
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(33_000)).is_some());
        health.started(MonoMs(33_000));
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(34_000)).is_none());
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(63_000)).is_some());
        health.started(MonoMs(63_000));
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(64_000)).is_none());
        // Attempt budgets belong to the recovery task, not this detector.
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(120_000)).is_some());
    }

    #[test]
    fn quiet_playback_and_ownership_changes_do_not_trigger_resets() {
        for condition in [
            "running",
            "idle",
            "unknown",
            "peer",
            "paused",
            "muted",
            "zero",
            "sink_muted",
            "sink_zero",
        ] {
            let (mut health, address, mut graph) = fixture(2);
            match condition {
                "running" => health.outputs[0].state = 0,
                "idle" => {
                    health.outputs[0].state = 1;
                    graph.streams[0].corked = true;
                }
                "unknown" => health.outputs[0].state = 99,
                "paused" => graph.streams[0].corked = true,
                "muted" => graph.streams[0].mute = true,
                "zero" => graph.streams[0].volume = 0.0,
                "sink_muted" => health.outputs[0].muted = true,
                "sink_zero" => health.outputs[0].audible_volume = false,
                _ => {}
            }
            for now in [0, 3_000, 60_000] {
                assert!(
                    health
                        .check(Some(address), Some(&graph), condition != "peer", MonoMs(now))
                        .is_none(),
                    "{condition}"
                );
            }
        }
    }

    #[test]
    fn recovery_and_server_loss_retire_old_observations() {
        let (mut health, address, graph) = fixture(-3);
        health.check(Some(address), Some(&graph), true, MonoMs(0));
        health.outputs[0].state = 0;
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(2_000)).is_none());
        health.outputs[0].state = -3;
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(4_000)).is_none());
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(6_999)).is_none());
        health.forget_server();
        assert!(health.check(Some(address), Some(&graph), true, MonoMs(7_000)).is_none());
    }
}
