//! Telegram Mini App authorization: `initData` is the only credential.
//!
//! Telegram signs the launch payload with the bot token, so a valid signature
//! proves the payload came from Telegram for this bot. The HTTP surface then
//! admits exactly one user, `cfg.owner_id`; static assets stay public because
//! the page has to load before JavaScript can read `initData`.

use crate::*;
use sha2::{Digest, Sha256};

/// Every `initData` this bot accepts is far smaller; the cap keeps a hostile
/// header from turning parsing into work.
const MAX_INIT_DATA_BYTES: usize = 8 * 1024;
const WEB_APP_KEY: &[u8] = b"WebAppData";
/// A payload whose `auth_date` lies further ahead is a clock anomaly, not a
/// launch. Telegram's own clock is never this far off.
const FUTURE_SKEW_SECS: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthError {
    Missing,
    Malformed,
    BadSignature,
    Expired,
    NotOwner { user_id: u64 },
}

/// The single decision endpoint: which owner, or which rejection.
pub(crate) fn verify(
    init_data: &str,
    bot_token: &str,
    owner_id: UserId,
    max_age_secs: u64,
) -> Result<u64, AuthError> {
    let raw = init_data.trim();
    if raw.is_empty() {
        return Err(AuthError::Missing);
    }
    if raw.len() > MAX_INIT_DATA_BYTES {
        return Err(AuthError::Malformed);
    }
    let pairs = parse_pairs(raw)?;
    let provided = pairs
        .iter()
        .find(|(key, _)| key == "hash")
        .map(|(_, value)| value.as_str())
        .ok_or(AuthError::Malformed)?;
    let provided = decode_hex(provided).ok_or(AuthError::Malformed)?;

    let mut signed: Vec<&(String, String)> =
        pairs.iter().filter(|(key, _)| key != "hash").collect();
    signed.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut check = String::new();
    for (index, (key, value)) in signed.iter().enumerate() {
        if index > 0 {
            check.push('\n');
        }
        check.push_str(key);
        check.push('=');
        check.push_str(value);
    }

    let secret = hmac_sha256(WEB_APP_KEY, bot_token.as_bytes());
    let expected = hmac_sha256(&secret, check.as_bytes());
    if !constant_time_eq(&expected, &provided) {
        return Err(AuthError::BadSignature);
    }

    let auth_date = pairs
        .iter()
        .find(|(key, _)| key == "auth_date")
        .map(|(_, value)| value.as_str())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(AuthError::Malformed)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    if auth_date > now.saturating_add(FUTURE_SKEW_SECS)
        || now.saturating_sub(auth_date) > max_age_secs
    {
        return Err(AuthError::Expired);
    }

    let user = pairs
        .iter()
        .find(|(key, _)| key == "user")
        .map(|(_, value)| value.as_str())
        .ok_or(AuthError::Malformed)?;
    let user = serde_json::from_str::<Value>(user).map_err(|_| AuthError::Malformed)?;
    let user_id = user
        .get("id")
        .and_then(Value::as_u64)
        .ok_or(AuthError::Malformed)?;
    if user_id != owner_id.0 {
        return Err(AuthError::NotOwner { user_id });
    }
    Ok(user_id)
}

/// `Authorization: tma <initData>`. The scheme is case-insensitive and the
/// payload itself may contain spaces after decoding, so only the first space
/// separates them.
pub(crate) fn init_data_from_authorization(header: Option<&str>) -> Option<&str> {
    let (scheme, data) = header?.split_once(' ')?;
    let data = data.trim();
    (scheme.eq_ignore_ascii_case("tma") && !data.is_empty()).then_some(data)
}

fn parse_pairs(raw: &str) -> Result<Vec<(String, String)>, AuthError> {
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let mut seen = std::collections::HashSet::new();
    for (key, _) in &pairs {
        if !seen.insert(key.as_str()) {
            return Err(AuthError::Malformed);
        }
    }
    Ok(pairs)
}

fn decode_hex(raw: &str) -> Option<[u8; 32]> {
    let bytes = raw.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let mut digest = [0u8; 32];
    for (slot, pair) in digest.iter_mut().zip(pairs) {
        let &[high, low] = pair;
        *slot = (nibble(high)? << 4) | nibble(low)?;
    }
    Some(digest)
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// HMAC-SHA256 built from the already-present `sha2`: one keyed inner hash and
/// one outer hash, with the key block zero-padded to 64 bytes or hashed first.
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        let digest = Sha256::digest(key);
        for (slot, byte) in block.iter_mut().zip(digest.iter()) {
            *slot = *byte;
        }
    } else {
        for (slot, byte) in block.iter_mut().zip(key.iter()) {
            *slot = *byte;
        }
    }
    let mut inner = Sha256::new();
    for byte in &block {
        inner.update([byte ^ 0x36]);
    }
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    for byte in &block {
        outer.update([byte ^ 0x5c]);
    }
    outer.update(inner);
    outer.finalize().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in left.iter().zip(right) {
        diff |= left ^ right;
    }
    std::hint::black_box(diff) == 0
}
