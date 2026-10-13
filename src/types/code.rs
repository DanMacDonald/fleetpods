//! Six-digit enrollment code. The value is a SPAKE2 password, not a comparison token.
//! Leading zeroes are significant.

use std::fmt;

use zeroize::Zeroize;

#[derive(Clone, PartialEq, Eq, Zeroize)]
pub struct Code([u8; 6]);

impl Drop for Code {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl Code {
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 6 || !bytes.iter().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut out = [0u8; 6];
        out.copy_from_slice(bytes);
        Some(Self(out))
    }

    /// `n` is in `0..1_000_000`. `42` is `000042`.
    pub fn from_u32(n: u32) -> Option<Self> {
        if n > 999_999 {
            return None;
        }
        Self::parse(&format!("{n:06}"))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn display(&self) -> String {
        // Digits were checked in the constructors.
        std::str::from_utf8(&self.0).unwrap_or("000000").to_string()
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Code([redacted])")
    }
}

/// Uniform draw in `0..1_000_000`, rejecting the biased tail of a `u32`.
pub fn from_os() -> std::io::Result<Code> {
    let span = 1_000_000u32;
    let limit = u32::MAX - (u32::MAX % span);
    loop {
        let mut buf = [0u8; 4];
        crate::linux::os::random_bytes(&mut buf)?;
        let n = u32::from_ne_bytes(buf);
        if n < limit {
            return Ok(Code::from_u32(n % span).expect("remainder is in range"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_zeroes_survive() {
        let code = Code::from_u32(42).unwrap();
        assert_eq!(code.display(), "000042");
        assert_eq!(Code::parse("000042").unwrap().display(), "000042");
        assert!(Code::parse("42").is_none());
        assert!(Code::parse("00042a").is_none());
    }
}
