//! 引擎告警收集：只收 WARN 级、消息以 `skin:` 开头的 tracing 事件（实机日志里的同一句话），
//! 随 `set_theme` / `render` 的回执带给编辑器。引擎侧不改。
//!
//! 不装全局订阅者（wasm 上全局只能设一次，同模块多个预览器会互相串）：每次调用期间把本模块的
//! 订阅者设成当前线程的默认（`dispatcher::with_default`），调用前清空缓冲、调用后取走。
//! `with_default` 是**替换**当前线程的默认订阅者、不是叠加：期间别的订阅者（若有）收不到任何事件。
//! wasm 里本无别的订阅者；原生宿主若要同时记日志，不该把预览调用包在这里面。
//!
//! 约定：只取事件的 `message` 字段（`tracing::warn!("skin: …")` 的格式化正文），其余结构化字段
//! 不看——引擎的皮肤告警都把全部信息写进正文。
//!
//! 资源层的告警（图集 / 动画建帧）只在建缓存那一刻报；资源没变时缓存跨 `set_theme` 沿用，由
//! `wind_scene::image_cache::replaying_skin_warnings` 让本帧命中的缓存条目重放（见 `Preview::render`）。

use std::cell::RefCell;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Level, Metadata, Subscriber};

/// 收集的前缀。
pub const PREFIX: &str = "skin:";
/// 一次调用至多收这么多条（不可信主题可让同一条告警按装饰数重复出现）。
pub const MAX_WARNINGS: usize = 64;
thread_local! {
    static BUF: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// 只建一次：每建一个 `Dispatch` 都会让 tracing 重算全部调用点的兴趣。
    static DISPATCH: Dispatch = Dispatch::new(SkinWarnings);
}

struct SkinWarnings;

fn wanted(m: &Metadata<'_>) -> bool {
    m.is_event() && *m.level() == Level::WARN
}

struct Message(Option<String>);

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl Subscriber for SkinWarnings {
    fn register_callsite(&self, m: &'static Metadata<'static>) -> Interest {
        if wanted(m) {
            Interest::sometimes()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, m: &Metadata<'_>) -> bool {
        wanted(m)
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, e: &Event<'_>) {
        let mut m = Message(None);
        e.record(&mut m);
        let Some(msg) = m.0.filter(|s| s.starts_with(PREFIX)) else {
            return;
        };
        BUF.with(|b| {
            let mut b = b.borrow_mut();
            if b.len() < MAX_WARNINGS && !b.contains(&msg) {
                b.push(msg);
            }
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// 跑 `f`，返回结果与期间的 `skin:` 告警（去重、按出现顺序，至多 [`MAX_WARNINGS`] 条）。
pub fn collect<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    BUF.with(|b| b.borrow_mut().clear());
    let r = DISPATCH.with(|d| tracing::dispatcher::with_default(d, f));
    (r, BUF.with(|b| std::mem::take(&mut *b.borrow_mut())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_skin_prefixed_warnings() {
        let ((), w) = collect(|| {
            tracing::warn!("skin: 甲 {}", 1);
            tracing::warn!("skin: 甲 {}", 1);
            tracing::warn!("主题图片读不出");
            tracing::info!("skin: 信息级不收");
            tracing::error!("skin: 错误级不收");
            tracing::warn!("skin: 乙");
        });
        assert_eq!(w, ["skin: 甲 1", "skin: 乙"]);
        // 调用期外的告警不进下一次。
        tracing::warn!("skin: 期外");
        let ((), w) = collect(|| ());
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn caps_the_count() {
        let ((), w) = collect(|| {
            for i in 0..MAX_WARNINGS + 10 {
                tracing::warn!("skin: 第 {i} 条");
            }
        });
        assert_eq!(w.len(), MAX_WARNINGS);
    }
}
