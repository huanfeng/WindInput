//! 取时间的垫片。本 crate 一律经 [`now`] 取当前时刻，不直接调 `Instant::now()`：
//! `std::time::Instant::now` 在 wasm32 上运行即 panic，到时只换这一处（如 `web-time`）。

/// 当前时刻；现在只是转调 `std::time::Instant::now()`。
pub fn now() -> std::time::Instant {
    std::time::Instant::now()
}
