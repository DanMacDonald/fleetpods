//! One producer, one consumer. The decoder pushes. The PipeWire source pops.
//! The consumer does not block on the decoder.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::mic::{MAX_QUEUE_SAMPLES, PRIME_MS};

pub struct Queue {
    slots: Box<[UnsafeCell<f32>]>,
    written: AtomicU32,
    read: AtomicU32,
    prefill: AtomicU32,
    primed: AtomicBool,
}

unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    pub fn new() -> Self {
        let mut slots = Vec::with_capacity(MAX_QUEUE_SAMPLES as usize);
        slots.resize_with(MAX_QUEUE_SAMPLES as usize, || UnsafeCell::new(0.0));
        Self {
            slots: slots.into_boxed_slice(),
            written: AtomicU32::new(0),
            read: AtomicU32::new(0),
            prefill: AtomicU32::new(0),
            primed: AtomicBool::new(false),
        }
    }

    pub fn prime(&self, rate: u32) {
        let samples = u64::from(rate) * u64::from(PRIME_MS) / 1_000;
        let prefill = u32::try_from(samples).unwrap_or(u32::MAX).min(MAX_QUEUE_SAMPLES / 2);
        self.prefill.store(prefill, Ordering::Relaxed);
        self.primed.store(false, Ordering::Relaxed);
    }

    pub fn discard(&self) {
        let end = self.written.load(Ordering::Acquire);
        self.read.store(end, Ordering::Release);
        self.primed.store(false, Ordering::Relaxed);
    }

    /// `false` when the queue cannot take the whole block. Nothing is written.
    pub fn push(&self, samples: &[f32]) -> bool {
        let count = samples.len() as u32;
        let end = self.written.load(Ordering::Relaxed);
        let used = end.wrapping_sub(self.read.load(Ordering::Acquire));
        if count > MAX_QUEUE_SAMPLES.saturating_sub(used) {
            return false;
        }
        for (index, sample) in samples.iter().enumerate() {
            let slot = end.wrapping_add(index as u32) % MAX_QUEUE_SAMPLES;
            unsafe { *self.slots[slot as usize].get() = *sample };
        }
        self.written.store(end.wrapping_add(count), Ordering::Release);
        true
    }

    /// Copy what is available once the reserve is full. The rest of `out` is silence.
    /// Returns how many real samples were copied. An underrun drops the reserve.
    pub fn pop(&self, out: &mut [f32]) -> u32 {
        let count = out.len() as u32;
        let start = self.read.load(Ordering::Relaxed);
        let available = self.written.load(Ordering::Acquire).wrapping_sub(start);
        if !self.primed.load(Ordering::Relaxed) && available < self.prefill.load(Ordering::Relaxed)
        {
            out.fill(0.0);
            return 0;
        }
        self.primed.store(true, Ordering::Relaxed);
        let copied = count.min(available);
        for index in 0..copied {
            let slot = start.wrapping_add(index) % MAX_QUEUE_SAMPLES;
            out[index as usize] = unsafe { *self.slots[slot as usize].get() };
        }
        out[copied as usize..].fill(0.0);
        self.read.store(start.wrapping_add(copied), Ordering::Release);
        if copied < count {
            self.primed.store(false, Ordering::Relaxed);
        }
        copied
    }
}

/// Shared with the PipeWire thread. The decoder stays on the daemon thread.
pub struct MicPath {
    pub queue: Queue,
    pub delivering: AtomicBool,
    /// A client is linked to the hidden source.
    pub streaming: AtomicBool,
    /// Mute on the hidden source. The PipeWire thread is the writer.
    pub muted: AtomicBool,
    /// PipeWire accepted 32-bit float, mono, at `rate`.
    pub format_ok: AtomicBool,
    pub discard: AtomicBool,
    pub missing: AtomicU64,
    pub dropped: AtomicU64,
    pub decode_errors: AtomicU64,
    pub rate: AtomicU32,
}

impl MicPath {
    pub fn new() -> Self {
        Self {
            queue: Queue::new(),
            delivering: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            format_ok: AtomicBool::new(false),
            discard: AtomicBool::new(true),
            missing: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            decode_errors: AtomicU64::new(0),
            rate: AtomicU32::new(0),
        }
    }
}

impl Default for MicPath {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserve_holds_playback_and_an_underrun_rebuilds_it() {
        let queue = Queue::new();
        queue.prime(64_000);
        assert_eq!(queue.prefill.load(Ordering::Relaxed), 12_800);
        assert!(queue.push(&vec![0.5; 100]));
        let mut out = [1.0; 16];
        assert_eq!(queue.pop(&mut out), 0);
        assert_eq!(out, [0.0; 16]);
        assert!(queue.push(&vec![0.25; 12_800]));
        let mut heard = vec![0.0; 100];
        assert_eq!(queue.pop(&mut heard), 100);
        assert!(heard.iter().all(|sample| *sample == 0.5));
        let mut rest = vec![1.0; 20_000];
        let copied = queue.pop(&mut rest);
        assert!(copied < rest.len() as u32);
        assert!(rest[..copied as usize].iter().all(|sample| *sample == 0.25));
        assert!(rest[copied as usize..].iter().all(|sample| *sample == 0.0));
        let mut again = [1.0; 8];
        assert_eq!(queue.pop(&mut again), 0);
        assert_eq!(again, [0.0; 8]);
        assert!(queue.push(&vec![0.75; 100]));
        assert_eq!(queue.pop(&mut again), 0);
        assert_eq!(again, [0.0; 8]);
    }

    #[test]
    fn a_full_queue_refuses_the_block() {
        let queue = Queue::new();
        queue.prime(8_000);
        assert!(queue.push(&vec![1.0; MAX_QUEUE_SAMPLES as usize]));
        assert!(!queue.push(&[1.0]));
        queue.discard();
        assert!(queue.push(&[0.5]));
    }
}
