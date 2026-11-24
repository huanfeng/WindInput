//! 值域层容错：面向用户的字符串枚举写错了值，只让**这一个字段**回落，不牵连别的。
//!
//! # 与段级降级的分工
//!
//! [`crate::section_fallback`] 是结构层兜底，把整段回落出厂值，代价是丢了同段其它设置。
//! 本模块是值域层，只让写错的那个字段回落。两层都要有：值域层管得准，结构层管得全
//! （覆盖没人记得挂容错的新字段）。
//!
//! # 泛型适配器，不手写映射表
//!
//! 把字符串重新喂给 derive 生成的 `T::deserialize`（经 [`serde::de::value::StrDeserializer`]），
//! 失败才回落。手写映射表会给值域造第二个真相源，改了枚举忘了改表就「写对了却不生效」。
//!
//! # 只治「字符串写错」，不治「类型写错」
//!
//! 类型错误照旧返回 `Err`，交给段级降级，以免掩盖真正的结构错误。例外是只服务
//! `compat.toml` 的 `tolerant_opt` / `tolerant_opt_bool` / `tolerant_bool` / `tolerant_i32`：
//! 那个载体没有段级降级，类型错也只能在字段上吞。

use serde::Deserialize;
use serde::de::value::StrDeserializer;
use serde::de::{Deserializer, IntoDeserializer};
use std::cell::RefCell;
use tracing::warn;

thread_local! {
    /// 本线程**当前这次反序列化**里被回落的原值。
    ///
    /// 必须收集而不只打日志：字段级容错不再触发段级降级，只有交给调用方
    /// （`read_schema` → `Schema::degraded_sections` → toast）用户才看得到。
    ///
    /// 用 thread_local 是因为 `deserialize_with` 的签名塞不进上下文。调用方必须先
    /// [`clear_fallbacks`] 再反序列化、随后 [`take_fallbacks`]：段级降级的探针会反复反序列化
    /// 同一份数据，不清就会混进探针的记录。
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
/// `Option` 在 tri-state 字段里表示「有没有配过」，回落成 `Some(默认)` 会凭空长出 per-app
/// 覆盖，用户改全局默认时这些应用不跟着变。
///
/// 本函数连类型错也吞：它只用在 `compat.toml` 上，理由同 [`tolerant_opt_bool`]。
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
/// 这是「不治类型写错」的例外（连同 [`tolerant_bool`]、[`tolerant_i32`]、[`tolerant_opt`]），
/// 只给 `compat.toml` 用：那里 `load_file` 解析失败即整份丢弃，不在字段上吞就是所有规则一起
/// 静默失效。回落 `None` 而非 `Some(false)`：认不出 = 没配过。
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
