use crate::web_auth::{init_data_from_authorization, verify, AuthError};
use crate::*;

const TOKEN: &str = "123456:test-token";
const OWNER: UserId = UserId(7);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_owner(data: &str) -> Result<u64, AuthError> {
    verify(data, TOKEN, OWNER, 86_400)
}

#[test]
fn hmac_matches_rfc4231_vectors() {
    let case1 = crate::web_auth::hmac_sha256(&[0x0b; 20], b"Hi There");
    assert_eq!(
        hex(&case1),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );

    let case2 = crate::web_auth::hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        hex(&case2),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );

    // Key longer than the 64-byte block: the key must be hashed first.
    let case6 = crate::web_auth::hmac_sha256(
        &[0xaa; 131],
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(
        hex(&case6),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn accepts_a_fresh_payload_for_the_owner() {
    let data = crate::test_support::signed_init_data(TOKEN, OWNER.0, now());
    assert_eq!(verify_owner(&data), Ok(OWNER.0));
}

#[test]
fn accepts_uppercase_hash() {
    let data = crate::test_support::signed_init_data(TOKEN, OWNER.0, now());
    let (prefix, hash) = data.rsplit_once("hash=").unwrap();
    let upper = format!("{prefix}hash={}", hash.to_uppercase());
    assert_eq!(verify_owner(&upper), Ok(OWNER.0));
}

#[test]
fn rejects_another_bot_token() {
    let data = crate::test_support::signed_init_data(TOKEN, OWNER.0, now());
    assert_eq!(
        verify(&data, "999:other", OWNER, 86_400),
        Err(AuthError::BadSignature)
    );
}

#[test]
fn rejects_a_tampered_user() {
    let data = crate::test_support::signed_init_data_with(TOKEN, OWNER.0, 8, now());
    assert_eq!(verify_owner(&data), Err(AuthError::BadSignature));
}

#[test]
fn rejects_another_user_with_a_valid_signature() {
    let data = crate::test_support::signed_init_data(TOKEN, 8, now());
    assert_eq!(verify_owner(&data), Err(AuthError::NotOwner { user_id: 8 }));
}

#[test]
fn rejects_stale_and_future_payloads() {
    let stale = crate::test_support::signed_init_data(TOKEN, OWNER.0, now() - 2 * 86_400);
    assert_eq!(verify_owner(&stale), Err(AuthError::Expired));

    let future = crate::test_support::signed_init_data(TOKEN, OWNER.0, now() + 3_600);
    assert_eq!(verify_owner(&future), Err(AuthError::Expired));
}

#[test]
fn rejects_missing_or_broken_fields() {
    let stamp = now();
    let missing_hash = format!("auth_date={stamp}&user=%7B%22id%22%3A7%7D");
    assert_eq!(verify_owner(&missing_hash), Err(AuthError::Malformed));

    let bad_hex = format!(
        "auth_date={stamp}&user=%7B%22id%22%3A7%7D&hash={}",
        "z".repeat(64)
    );
    assert_eq!(verify_owner(&bad_hex), Err(AuthError::Malformed));

    let short_hex = format!(
        "auth_date={stamp}&user=%7B%22id%22%3A7%7D&hash={}",
        "0".repeat(63)
    );
    assert_eq!(verify_owner(&short_hex), Err(AuthError::Malformed));

    let duplicate = format!(
        "auth_date={stamp}&auth_date={stamp}&user=%7B%22id%22%3A7%7D&hash={}",
        "0".repeat(64)
    );
    assert_eq!(verify_owner(&duplicate), Err(AuthError::Malformed));

    // A correctly signed payload without `user` must fail as malformed, not
    // reach the owner comparison.
    let signed_without_user = format!("auth_date={stamp}");
    let secret = crate::web_auth::hmac_sha256(b"WebAppData", TOKEN.as_bytes());
    let digest = crate::web_auth::hmac_sha256(&secret, signed_without_user.as_bytes());
    let no_user = format!("{signed_without_user}&hash={}", hex(&digest));
    assert_eq!(verify_owner(&no_user), Err(AuthError::Malformed));
}

#[test]
fn rejects_empty_and_oversized_payloads() {
    assert_eq!(verify_owner("   "), Err(AuthError::Missing));
    assert_eq!(verify_owner(&"a".repeat(9_000)), Err(AuthError::Malformed));
}

#[test]
fn parses_only_the_tma_scheme() {
    assert_eq!(
        init_data_from_authorization(Some("tma abc=1")),
        Some("abc=1")
    );
    assert_eq!(
        init_data_from_authorization(Some("TMA abc=1")),
        Some("abc=1")
    );
    assert_eq!(init_data_from_authorization(Some("Bearer abc=1")), None);
    assert_eq!(init_data_from_authorization(Some("tma")), None);
    assert_eq!(init_data_from_authorization(Some("tma   ")), None);
    assert_eq!(init_data_from_authorization(None), None);
}
