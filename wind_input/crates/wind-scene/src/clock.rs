//! 取时间的垫片。本 crate 一律经 [`now`] 取当前时刻、用 [`Instant`] 存时刻，不直接用
//! `std::time::Instant`：它的 `now()` 在 wasm32 上运行即 panic，wasm 上换成 `web-time`
//! （浏览器 `performance.now()`）。原生目标上 [`Instant`] 就是 `std::time::Instant`。

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;
#[cfg(target_arch = "wasm32")]
pub use web_time::Instant;

/// 当前时刻。
pub fn now() -> Instant {
    Instant::now()
}
