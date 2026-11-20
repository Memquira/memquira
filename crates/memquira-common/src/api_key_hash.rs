use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Returns a hex-encoded HMAC-SHA256 of `raw_key` using `pepper`.
///
/// Raw API keys must never be stored; only this hash is persisted.
pub fn hash_api_key(pepper: &str, raw_key: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(pepper.as_bytes()).expect("HMAC accepts arbitrary key length");
    mac.update(raw_key.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
