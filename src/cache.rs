use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use axum::body::Bytes;
use http::{HeaderMap, StatusCode};
use moka::{future::Cache, Expiry};

pub struct MyExpiry;

impl Expiry<String, (u64, StatusCode, HeaderMap, Bytes)> for MyExpiry {
    /// Returns the duration of the expiration of the value that was just
    /// created.
    fn expire_after_create(
        &self,
        _key: &String,
        value: &(u64, StatusCode, HeaderMap, Bytes),
        _current_time: Instant,
    ) -> Option<Duration> {
        Some(Duration::from_secs(value.0))
    }
}

pub static CACHE: LazyLock<Cache<String, (u64, StatusCode, HeaderMap, Bytes)>> =
    LazyLock::new(|| {
        let expiry = MyExpiry;

        Cache::builder()
            .max_capacity(10000)
            .expire_after(expiry)
            .build()
    });
