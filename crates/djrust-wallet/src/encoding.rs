//! Hex, base58btc and Keccak-256 (`Encoding.kt`, `Keccak.kt`).

use sha3::{Digest, Keccak256};

pub mod hex {
    use crate::Error;

    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push(DIGITS[(b >> 4) as usize] as char);
            s.push(DIGITS[(b & 0x0f) as usize] as char);
        }
        s
    }

    pub fn encode0x(bytes: &[u8]) -> String {
        format!("0x{}", encode(bytes))
    }

    /// Accepts an optional 0x/0X prefix and either case.
    pub fn decode(s: &str) -> Result<Vec<u8>, Error> {
        let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
        if h.len() % 2 != 0 {
            return Err(Error::invalid("odd-length hex"));
        }
        let digit = |c: u8| -> Result<u8, Error> {
            (c as char).to_digit(16).map(|d| d as u8).ok_or_else(|| Error::invalid("bad hex digit"))
        };
        h.as_bytes().chunks(2).map(|p| Ok((digit(p[0])? << 4) | digit(p[1])?)).collect()
    }

    pub fn decode_n<const N: usize>(s: &str) -> Result<[u8; N], Error> {
        decode(s)?.try_into().map_err(|_| Error::invalid(format!("need {N} bytes")))
    }
}

/// base58btc, the alphabet beats uses for CIDs.
pub mod base58 {
    use crate::Error;

    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    pub fn encode(bytes: &[u8]) -> String {
        let zeros = bytes.iter().take_while(|&&b| b == 0).count();
        // base-58 digits, little-endian
        let mut digits: Vec<u8> = Vec::with_capacity(bytes.len() * 138 / 100 + 1);
        for &b in &bytes[zeros..] {
            let mut carry = b as u32;
            for d in digits.iter_mut() {
                carry += (*d as u32) << 8;
                *d = (carry % 58) as u8;
                carry /= 58;
            }
            while carry > 0 {
                digits.push((carry % 58) as u8);
                carry /= 58;
            }
        }
        let mut s = String::with_capacity(zeros + digits.len());
        for _ in 0..zeros {
            s.push('1');
        }
        for &d in digits.iter().rev() {
            s.push(ALPHABET[d as usize] as char);
        }
        s
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, Error> {
        let zeros = s.bytes().take_while(|&c| c == b'1').count();
        let mut bytes: Vec<u8> = Vec::new(); // little-endian
        for c in s.bytes().skip(zeros) {
            let d = ALPHABET
                .iter()
                .position(|&a| a == c)
                .ok_or_else(|| Error::invalid(format!("bad base58 character '{}'", c as char)))?;
            let mut carry = d as u32;
            for b in bytes.iter_mut() {
                carry += (*b as u32) * 58;
                *b = (carry & 0xff) as u8;
                carry >>= 8;
            }
            while carry > 0 {
                bytes.push((carry & 0xff) as u8);
                carry >>= 8;
            }
        }
        let mut out = vec![0u8; zeros];
        out.extend(bytes.iter().rev());
        Ok(out)
    }
}

/// Keccak-256 with Ethereum's pre-NIST padding.
pub fn keccak(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// Keccak-256 over the concatenation of `parts`.
pub fn keccak_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Keccak256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
