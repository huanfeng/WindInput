//! 值域层容错：面向用户的字符串枚举写错了值，只让**这一个字段**回落，不牵连别的。
//!
//! # 与段级降级的分工
//!
//! [`crate::section_fallback`] 是结构层兜底：探不出别的办法时把**整段**回落出厂值。
//! 它保证「不会整份失效」，但代价是用户只写错一个键、却丢了同段的其它设置。
//!
//! 本模块是值域层：`text_orientation = "uprigth"`（拼错）只让这一个字段回落，
//! `[candidate]` 段里的 `font_family`、注释模板等照常生效。**两层都要有**——
//! 值域层管得准，结构层管得全（覆盖将来新增的、没人记得挂容错的字段）。
//!
//! # ★ 为什么是泛型适配器，而不是给每个枚举手写映射表
//!
//! 本仓 6 个面向用户的字符串枚举里，只有 `TextOrientation` 有 `as_str()`；其余 5 个
//! （`LayoutIntent` / `PunctIntent` / `TopCommitMode` / `FreeInputMode` / `SmartMethod`）
//! 的值域**只存在于 `#[serde(rename_all)]` 生成的实现里**。手写一张 `"normal" => Normal`
//! 的映射表就等于给值域造第二个真相源——改了枚举忘了改表，症状是「配置写对了却不生效」，
//! 比现在这个 bug 更难查。
//!
//! 这里的做法是把字符串重新喂给 **derive 生成的那个实现**（`T::deserialize` +
//! [`serde::de::value::StrDeserializer`]），失败才回落。值域始终只有一份。
//!
//! # 只治「字符串写错」，不治「类型写错」
//!
//! `text_orientation = 3` 这类**类型**错误照旧返回 `Err`，交给段级降级处理。
//! 例外是只服务 `compat.toml` 的几个函数（`tolerant_opt` / `tolerant_opt_bool` /
//! `tolerant_bool` / `tolerant_i32`）：那个载体没有段级降级，类型错也只能在字段上吞。
//! 分工清晰：值域层只回答「这个字符串在不在值域里」，把它扩张成「什么都吞」会连
//! 真正的配置结构错误一起掩盖掉。

use serde::Deserialize;
use serde::de::value::StrDeserializer;
use serde::de::{Deserializer, IntoDeserializer};
use std::cell::RefCell;
use tracing::warn;

thread_local! {
    /// 本线程**当前这次反序列化**里被回落的原值。
    ///
    /// # 为什么必须收集，而不是只打日志
    ///
    /// 字段级容错把爆炸半径缩到了一个字段，但也因此**不再触发段级降级**——于是用户的
    /// 处境从「整个方案不工作」变成「这一项写了没反应」，日志里有一行 WARN，界面上
    /// 什么都没有。那仍然是「完全没反应，但又不工作」，只是更安静。
    /// 收集起来交给调用方（`read_schema` → `Schema::degraded_sections` → toast）才算闭环。
    ///
    /// # 为什么是 thread_local 而不是参数
    ///
    /// `deserialize_with` 的签名由 serde 定死，塞不进上下文；而一次 `try_into` 是同一个
    /// 线程内同步跑完的，thread_local 的生命周期正好覆盖它。⚠️ 调用方必须
    /// **先 [`clear_fallbacks`] 再反序列化，随后 [`take_fallbacks`]**——段级降级的探针会
    /// 反复反序列化同一份数据，不清就会把探针产生的记录算进最终结果。
    static FALLBACKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// 清空本线程的回落记录。反序列化**之前**调用，见 `FALLBACKS` 的文档。
pub fn clear_fallbacks() {
    FALLBACKS.with(|f| f.borrow_mut().clear());
}

/// 取走本线程的回落记录（原值列表，已去重、保序）。
pub fn take_fallbacks() -> Vec<String> {
    FALLBACKS.with(|f| {
        let mut v = std::mem::take(&mut *f.borrow_mut());
        // 同一个坏值可能被多个字段共用（如两处都写了同样的错拼），报一次就够。
        let mut seen = std::collections::HashSet::new();
        v.retain(|s| seen.insert(s.clone()));
        v
    })
}

/// `pub(crate)`：自定义 `Deserialize` 的字段（如 `de_raw_candidate`，它要兼容老的 bool
/// 形态、无法直接套 [`tolerant`]）同样要把回落记进来，否则设置端的 toast 漏报这一项。
pub(crate) fn record_fallback(raw: &str) {
    FALLBACKS.with(|f| f.borrow_mut().push(raw.to_string()));
}

/// 容错反序列化一个字符串枚举：值域外的字符串回落 `T::default()` 并 WARN。
///
/// 用在**非** `Option` 的枚举字段上：
/// ```ignore
/// #[serde(default, deserialize_with = "crate::tolerant_de::tolerant")]
/// pub text_orientation: TextOrientation,
/// ```
pub fn tolerant<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let raw = String::deserialize(d)?;
    let sd: StrDeserializer<'_, D::Error> = raw.as_str().into_deserializer();
    match T::deserialize(sd) {
        Ok(v) => Ok(v),
        Err(e) => {
            // WARN 而非 INFO：这是用户的配置**没有生效**，不是正常降级。
            // 带上原值，否则用户只知道「不生效」却不知道自己写错了哪个字。
            warn!(
                "配置值 \"{raw}\" 不在取值范围内（{}），本项回落出厂默认值：{e}",
                std::any::type_name::<T>()
            );
            record_fallback(&raw);
            Ok(T::default())
        }
    }
}

/// 容错反序列化 `Option<枚举>`：值域外的字符串回落 **`None`**，不是 `Some(默认值)`。
///
/// ⚠️ 与 [`tolerant`] 的回落目标刻意不同，这个区别是语义性的。`Option` 在本仓的
/// tri-state 字段里表示「**有没有配过**」：`None` = 不干预/跟随上一层。认不出的值回落成
/// `Some(T::default())` 就等于**替用户显式配了一个默认档**——per-app 覆盖凭空长出来，
/// 用户改全局默认时这些应用不跟着变，且他从没配过、无从撤销。
/// 见 `AppCompatRule::first_show_mode` 的字段文档。
///
/// ⚠️ 与 [`tolerant`] 不同，本函数**连类型错也吞**（`smart_method = true` 回落 `None`
/// 并 WARN）：它只用在 `compat.toml` 上，那里没有段级降级可交，理由同 [`tolerant_opt_bool`]。
pub fn tolerant_opt<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let raw = match Option::<toml::Value>::deserialize(d)? {
        None => return Ok(None),
        Some(toml::Value::String(s)) => s,
        Some(other) => {
            let raw = other.to_string();
            warn!(
                "配置值 {raw} 不是字符串（{}），本项按「未设置」处理",
                std::any::type_name::<T>()
            );
            record_fallback(&raw);
            return Ok(None);
        }
    };
    let sd: StrDeserializer<'_, D::Error> = raw.as_str().into_deserializer();
    match T::deserialize(sd) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            warn!(
                "配置值 \"{raw}\" 不在取值范围内（{}），本项按「未设置」处理：{e}",
                std::any::type_name::<T>()
            );
            record_fallback(&raw);
            Ok(None)
        }
    }
}

/// 容错反序列化 `Option<bool>`：非布尔值（`"yes"`、`1`）回落 **`None`** 并 WARN。
///
/// ⚠️ 这是模块头部「只治字符串写错，不治类型写错」的**例外**（连同其裸 `bool` 版
/// [`tolerant_bool`]、整数版 [`tolerant_i32`]、[`tolerant_opt`]），只给没有段级降级的载体用（`compat.toml`：
/// `load_file` 解析失败即整份丢弃）。在那里，类型错并没有下一层兜底可交——不在字段上吞掉，就是所有应用的所有
/// 规则一起静默失效。
/// 回落 `None` 而非 `Some(false)`，理由同 [`tolerant_opt`]：认不出 = 没配过。
pub fn tolerant_opt_bool<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<toml::Value>::deserialize(d)? {
        None => Ok(None),
        Some(toml::Value::Boolean(b)) => Ok(Some(b)),
        Some(other) => {
            let raw = other.to_string();
            warn!("配置值 {raw} 不是布尔值（true / false），本项按「未设置」处理");
            record_fallback(&raw);
            Ok(None)
        }
    }
}

/// 容错反序列化裸 `bool`：非布尔值回落 **`false`**（这类字段的「未配置」）并 WARN。
///
/// 与 [`tolerant_opt_bool`] 同一个例外、同一个载体（`compat.toml`）：那里的裸 `bool` 字段
/// 都是 `default + skip_serializing_if = "is_false"`，`false` 就是「没配」，回落到它与
/// `Option` 版回落 `None` 同义。
pub fn tolerant_bool<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(tolerant_opt_bool(d)?.unwrap_or(false))
}

/// 容错反序列化裸 `i32`：非整数（`"12"`、`1.5`、`true`）或超出 `i32` 范围的整数回落 **`0`**
/// 并 WARN。
///
/// 与 [`tolerant_bool`] 同一个例外、同一个载体（`compat.toml`）：那里的 `i32` 字段（坐标校正、
/// 固定落点）都是 `default + skip_serializing_if = "is_zero_i32"`，`0` 就是「没配」。
pub fn tolerant_i32<'de, D>(d: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<toml::Value>::deserialize(d)? {
        None => Ok(0),
        Some(toml::Value::Integer(n)) if i32::try_from(n).is_ok() => Ok(n as i32),
        Some(other) => {
            let raw = other.to_string();
            warn!("配置值 {raw} 不是 32 位整数，本项按「未设置」（0）处理");
            record_fallback(&raw);
            Ok(0)
        }
    }
}

#[cfg(test)]
#[path = "tolerant_de_tests.rs"]
mod tests;
