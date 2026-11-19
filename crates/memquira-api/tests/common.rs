#![allow(dead_code)]

pub const DEV_API_KEY: &str = "memcore_dev_key";

pub fn authorization_header() -> (&'static str, String) {
    ("Authorization", format!("Bearer {DEV_API_KEY}"))
}
