//! AAC-ELD through FFmpeg. A frame is kept only when it matches the Pro 3 table.

use super::mic::Format;

extern "C" {
    fn eld_open(config: *const u8, config_len: i32, rate: i32) -> *mut u8;
    fn eld_decode(
        handle: *mut u8,
        data: *const u8,
        len: i32,
        out: *mut f32,
        out_cap: i32,
        expect_samples: i32,
        expect_rate: i32,
    ) -> i32;
    fn eld_close(handle: *mut u8);
}

pub struct Decoder {
    handle: *mut u8,
    samples: i32,
    rate: i32,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn open(format: &Format) -> Option<Self> {
        let handle =
            unsafe { eld_open(format.audio_specific_config.as_ptr(), 4, format.coding_hz as i32) };
        if handle.is_null() {
            None
        } else {
            Some(Self {
                handle,
                samples: format.samples_per_frame as i32,
                rate: format.coding_hz as i32,
            })
        }
    }

    pub fn decode(&mut self, unit: &[u8]) -> Option<Vec<f32>> {
        if unit.is_empty() || unit.len() > i32::MAX as usize {
            return None;
        }
        let mut out = vec![0.0; self.samples as usize];
        let count = unsafe {
            eld_decode(
                self.handle,
                unit.as_ptr(),
                unit.len() as i32,
                out.as_mut_ptr(),
                self.samples,
                self.samples,
                self.rate,
            )
        };
        if count == self.samples {
            Some(out)
        } else {
            None
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { eld_close(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mic::format_for_sku;

    #[test]
    fn the_pro_3_decoder_opens_and_rejects_a_garbage_frame() {
        let format = format_for_sku("A3064").unwrap();
        let mut decoder = Decoder::open(&format).expect("aac decoder");
        assert!(decoder.decode(&[]).is_none());
    }
}
