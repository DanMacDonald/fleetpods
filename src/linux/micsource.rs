//! The hidden microphone source. WirePlumber routes the AirPods input here.
//! This thread copies samples the daemon already decoded. It reports when a
//! recorder is linked, when the source is muted, and when the format matches.

use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::mic::SOURCE_NAME;
use crate::audio::queue::MicPath;
use crate::types::Address;

#[repr(C)]
struct Hooks {
    fill: Option<extern "C" fn(*mut c_void, *mut f32, i32)>,
    streaming: Option<extern "C" fn(*mut c_void, i32)>,
    muted: Option<extern "C" fn(*mut c_void, i32)>,
    format_ok: Option<extern "C" fn(*mut c_void, i32)>,
}

extern "C" {
    fn mic_source_run(
        name: *const i8,
        description: *const i8,
        address: *const i8,
        rate: u32,
        user: *mut c_void,
        hooks: *const Hooks,
        stop: *mut i32,
    );
}

pub struct Running {
    stop: Arc<AtomicI32>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    pub fn start(
        path: Arc<MicPath>,
        address: Address,
        rate: u32,
        description: String,
    ) -> Option<Self> {
        let stop = Arc::new(AtomicI32::new(0));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("fleetpods-mic".into())
            .spawn(move || {
                let name = CString::new(SOURCE_NAME).ok();
                let description =
                    CString::new(description).or_else(|_| CString::new("AirPods")).ok();
                let address = CString::new(address.display()).ok();
                let (Some(name), Some(description), Some(address)) = (name, description, address)
                else {
                    return;
                };
                let hooks = Hooks {
                    fill: Some(fill),
                    streaming: Some(on_streaming),
                    muted: Some(on_muted),
                    format_ok: Some(on_format),
                };
                let user = Arc::into_raw(path) as *mut c_void;
                unsafe {
                    mic_source_run(
                        name.as_ptr(),
                        description.as_ptr(),
                        address.as_ptr(),
                        rate,
                        user,
                        &hooks,
                        flag.as_ptr(),
                    );
                    drop(Arc::from_raw(user as *const MicPath));
                }
            })
            .ok()?;
        Some(Self { stop, thread: Some(thread) })
    }

    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(1, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

extern "C" fn on_streaming(user: *mut c_void, on: i32) {
    if user.is_null() {
        return;
    }
    let path = unsafe { &*(user as *const MicPath) };
    path.streaming.store(on != 0, Ordering::Relaxed);
}

extern "C" fn on_muted(user: *mut c_void, on: i32) {
    if user.is_null() {
        return;
    }
    let path = unsafe { &*(user as *const MicPath) };
    path.muted.store(on != 0, Ordering::Relaxed);
}

extern "C" fn on_format(user: *mut c_void, ok: i32) {
    if user.is_null() {
        return;
    }
    let path = unsafe { &*(user as *const MicPath) };
    let was = path.format_ok.swap(ok != 0, Ordering::Relaxed);
    if ok != 0 && !was {
        path.discard.store(true, Ordering::Relaxed);
    }
}

extern "C" fn fill(user: *mut c_void, samples: *mut f32, count: i32) {
    if user.is_null() || samples.is_null() || count <= 0 {
        return;
    }
    let path = unsafe { &*(user as *const MicPath) };
    let out = unsafe { std::slice::from_raw_parts_mut(samples, count as usize) };
    if path.discard.swap(false, Ordering::Relaxed) {
        path.queue.discard();
        let rate = path.rate.load(Ordering::Relaxed);
        path.queue.prime(if rate == 0 { 64_000 } else { rate });
    }
    let playing = path.delivering.load(Ordering::Relaxed)
        && path.streaming.load(Ordering::Relaxed)
        && path.format_ok.load(Ordering::Relaxed)
        && !path.muted.load(Ordering::Relaxed);
    if playing {
        let copied = path.queue.pop(out);
        path.missing.fetch_add((count as u64).saturating_sub(u64::from(copied)), Ordering::Relaxed);
    } else {
        path.queue.discard();
        out.fill(0.0);
    }
}
