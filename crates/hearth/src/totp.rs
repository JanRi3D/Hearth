//! Two-step sign-in: time-based one-time passwords as authenticator apps expect them
//! (RFC 6238 with HMAC-SHA1, 6 digits, 30 seconds).

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

const STEP: i64 = 30;
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// 160 random bits in base32, the form authenticator apps take.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::fill(&mut bytes);
    base32_encode(&bytes)
}

/// What a QR code for the authenticator app has to say.
pub fn uri(secret: &str, username: &str, hearth: &str) -> String {
    let label = format!("Hearth {hearth}:{username}").replace(' ', "%20");
    let issuer = format!("Hearth {hearth}").replace(' ', "%20");
    format!("otpauth://totp/{label}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits=6&period=30")
}

pub fn code_at(secret: &str, time: i64) -> Option<String> {
    let key = base32_decode(secret)?;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&((time / STEP) as u64).to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = u32::from_be_bytes([digest[offset] & 0x7f, digest[offset + 1], digest[offset + 2], digest[offset + 3]]);
    Some(format!("{:06}", binary % 1_000_000))
}

/// Accepts the current code and its neighbours, since phone clocks drift.
/// ponytail: a code can be used twice within its 90 seconds. Remember the last used step per
/// account if that ever matters.
pub fn verify(secret: &str, code: &str, now: i64) -> bool {
    let code: String = code.chars().filter(char::is_ascii_digit).collect();
    code.len() == 6 && [-STEP, 0, STEP].iter().any(|shift| code_at(secret, now + shift).as_deref() == Some(code.as_str()))
}

fn base32_encode(bytes: &[u8]) -> String {
    let (mut out, mut buffer, mut bits) = (String::new(), 0u32, 0);
    for &byte in bytes {
        buffer = (buffer << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let (mut out, mut buffer, mut bits) = (Vec::new(), 0u32, 0);
    for c in text.bytes().filter(|c| *c != b'=' && *c != b' ') {
        let value = BASE32.iter().position(|known| *known == c.to_ascii_uppercase())? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rfc_6238_test_vectors() {
        // The RFC's SHA-1 secret is the ASCII string "12345678901234567890"; its 8-digit
        // results end in these 6 digits.
        let secret = base32_encode(b"12345678901234567890");
        assert_eq!(secret, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        for (time, code) in [(59, "287082"), (1111111109, "081804"), (1234567890, "005924"), (2000000000, "279037")] {
            assert_eq!(code_at(&secret, time).as_deref(), Some(code), "at {time}");
        }
    }

    #[test]
    fn verifies_with_clock_drift_and_refuses_everything_else() {
        let secret = new_secret();
        assert_eq!(secret.len(), 32);
        let now = 1_800_000_000;
        let code = code_at(&secret, now).unwrap();
        assert!(verify(&secret, &code, now));
        assert!(verify(&secret, &format!("{} {}", &code[..3], &code[3..]), now + 29), "spaces and a late entry are fine");
        assert!(!verify(&secret, &code, now + 120));
        assert!(!verify(&secret, "12345", now));
        assert!(!verify("not base32!", &code, now));
        assert_eq!(base32_decode(&base32_encode(b"hearth")).unwrap(), b"hearth");
    }
}
