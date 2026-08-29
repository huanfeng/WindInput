//! 定制版身份（`data_custom/custom.toml` 的 `[custom]` 段）的**对外暴露**：
//! 启动日志摘要与 RPC 字段共用这一处实现。
//!
//! # 为什么两者同源
//!
//! 这一项存在的全部理由是**报障**：定制版用户报「打不出字」时，第一步要能判断他装的
//! 到底是不是定制版、是哪一版。线索有两条——他贴上来的日志、他截图的关于页。两条各写
//! 一份格式化就会分叉（日志说 1.2、关于页说 1.3 是最难查的一类不一致），而这恰好发生在
//! 最需要可信的场合。故 [`startup_summary`] 与 [`identity_json`] 取同一个 manifest、
//! 走同一套空值处置。
//!
//! # 为什么住在 wind-rpc 而不是 wind-config
//!
//! `CustomManifest` 本身住在 `wind-config`，把摘要文案做成它的固有方法在层次上更自然。
//! 本次实施受改动边界所限（另有并行改动在 `wind-config` 侧），故先落在这里——两个消费者
//! （service 的启动日志、dispatch 的 `system.info`）都在 wind-rpc 的下游，取不到更下层的
//! 实现也不会有第三份副本。日后若要下沉到 `wind-config`，把这两个函数整体挪走即可，
//! 调用点各一处。
//!
//! # 日志隐私
//!
//! `id` / `name` / `version` / `base_version` 是**定制者声明的元信息**（随定制包分发，
//! 对所有该定制版用户相同），不是用户数据，故可进 INFO。定制层的**路径**属于用户机器
//! 信息，不进这一行（`load_custom_manifest` 已在 DEBUG 打过）。

use serde_json::{Value, json};
use wind_config::Config;

/// 空字段的占位：宁可打一个显眼的占位符，也不要在日志/关于页上留一段空白——
/// 空白既看不出「定制者没填」还是「程序没读到」，也让报障截图失去价值。
fn or_placeholder<'a>(s: &'a str, placeholder: &'a str) -> &'a str {
    if s.trim().is_empty() { placeholder } else { s }
}

/// 启动摘要行：非定制版返回 `None`（**不打任何行**——绝大多数装机走这一条，
/// 一行「本机不是定制版」乘以每次启动就是纯噪音）。
///
/// 形如：`定制版 huma-edition「虎码定制版」1.2（基于 0.9.30）`。
pub fn startup_summary() -> Option<String> {
    let m = Config::custom_manifest()?;
    let c = &m.custom;
    Some(format!(
        "定制版 {}「{}」{}（基于 {}）",
        or_placeholder(&c.id, "<未命名>"),
        or_placeholder(&c.name, "<无显示名>"),
        or_placeholder(&c.version, "<未标版本>"),
        or_placeholder(&c.base_version, "<未声明>"),
    ))
}

/// RPC 形态的定制版身份：非定制版返回 `Value::Null`。
///
/// ★ **不是「缺字段」而是显式 `null`**：字段恒在，值域是「对象 | null」。跨仓契约无
/// 编译期约束，「字段不存在」与「我这版 core 还没实现这个字段」在客户端看来完全一样，
/// 于是关于页只能靠猜；显式 `null` 则明确表示「问过了，本机不是定制版」。
///
/// 字段原样透出、**不做占位替换**：这里的消费者是 UI 与报障脚本，占位符是给人读的日志
/// 用的，塞进结构化字段会让「定制者没填 version」变成一个看起来像版本号的字符串。
pub fn identity_json() -> Value {
    match Config::custom_manifest() {
        None => Value::Null,
        Some(m) => json!({
            "id": m.custom.id,
            "name": m.custom.name,
            "version": m.custom.version,
            "baseVersion": m.custom.base_version,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空字段的占位处置：两个消费者共用，故只测这一层纯函数。
    /// 有清单/无清单两态由 `tests/custom_edition_present.rs` 与
    /// `tests/custom_edition_absent.rs` 两个独立进程覆盖（`custom_manifest()` 是 OnceLock）。
    #[test]
    fn placeholder_only_for_blank() {
        assert_eq!(or_placeholder("huma", "<未命名>"), "huma");
        assert_eq!(or_placeholder("", "<未命名>"), "<未命名>");
        // 全空白也算没填：定制者写了个空格，日志上与没填毫无区别，别让它蒙混过去。
        assert_eq!(or_placeholder("   ", "<未命名>"), "<未命名>");
    }
}
