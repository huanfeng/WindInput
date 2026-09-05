//! 守门元测试：**用户手写 TOML 里的任何一个字符串值写错，都不许让整份配置失效**。
//!
//! # 这条测试防的是什么
//!
//! `toml::Value::try_into::<T>()` 是整份一次性反序列化。面向用户的字符串枚举
//! （`text_orientation` / `punct.mode` / `smart_method` …）一旦写了值域外的字符串，
//! serde 报 `unknown variant`，整个 `T` 就 `Err`——而调用方通常把 `Err` 处理成
//! 「这份文件/这个方案/这一层不存在」，只留一行 `warn!`。用户看到的是
//! **完全没反应，但也不工作**，这是本仓最贵的失败形态。
//!
//! 已经为此付过三次学费：`compat.toml` 的 `initial_mode`（`de_initial_mode` 的文档）、
//! `config.toml` 的重复键（`parse_toml_lenient`）、`schema_overrides/<id>.toml` 的
//! `text_orientation = ""`（2026-09-04，整个方案读不出来）。
//!
//! # ★ 为什么必须是**遍历式**的元测试，而不是逐个字段的用例
//!
//! 前两次修复都是「给这个字段挂 `deserialize_with`」，靠人记得。结果就在 `app_compat.rs`
//! 同一个结构体里，`initial_mode` / `initial_punct` / `first_show_mode` 三个挂了，
//! 紧挨着的 `smart_method` **漏了**——同一个文件、同一段教训，隔几行就漏一个。
//!
//! 逐个字段的用例只覆盖已知的坑；本测试遍历 `T::default()` 序列化出的**每一个**字符串
//! 叶子，新增字段自动进入覆盖范围。忘了容错的字段会让这里红，不需要任何人记得。
//!
//! # 两档合格线
//!
//! | 档 | 判据 | 适用 |
//! |---|---|---|
//! | 字段级容错（严） | 换成非法值后整份仍反序列化成功 | 调用方没有段级降级的类型 |
//! | 段级降级可救（宽） | 整份失败，但 `probe_and_patch` 探得出毒且回落后成功 | 已接段级降级的 `Config` / `Schema` |
//!
//! ⚠️ 宽档**不是**及格线而是兜底：靠它救回意味着**同段的其它设置一起被回落成出厂值**
//! （用户只写错一个键，却丢了一整段）。2026-09-04 起这个名单已清零，
//! [`assert_no_regression`] 把它钉成了硬断言——名单再次非空时的修法是给那个字段挂容错，
//! 不是把它登记进豁免表。

use crate::app_compat::AppCompatFile;
use crate::config::Config;
use crate::schema::Schema;
use crate::section_fallback::probe_and_patch;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// 探针值：一个绝不会落在任何枚举值域里、也不像路径/字体名的字符串。
const POISON: &str = "__wind_invalid_value__";

/// 收集所有**字符串叶子**的点分路径。
///
/// 数组要下钻（`apps[0].smart_method` 这类枚举只住在数组元素里，`Vec::default()` 是空的、
/// 扫不到），路径里用 `[i]` 表示下标；改写时按同一套路径回填。
fn string_leaf_paths(v: &toml::Value, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match v {
        toml::Value::Table(t) => {
            for (k, sub) in t {
                prefix.push(k.clone());
                string_leaf_paths(sub, prefix, out);
                prefix.pop();
            }
        }
        toml::Value::Array(a) => {
            for (i, sub) in a.iter().enumerate() {
                prefix.push(format!("[{i}]"));
                string_leaf_paths(sub, prefix, out);
                prefix.pop();
            }
        }
        toml::Value::String(_) => out.push(prefix.clone()),
        _ => {}
    }
}

/// 按 [`string_leaf_paths`] 产出的路径把该处的值换成 `POISON`。
fn poison_at(root: &mut toml::Value, path: &[String]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut cur = root;
    for seg in parents {
        cur = match seg.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            Some(idx) => match idx
                .parse::<usize>()
                .ok()
                .and_then(|i| cur.as_array_mut().and_then(|a| a.get_mut(i)))
            {
                Some(v) => v,
                None => return false,
            },
            None => match cur.get_mut(seg) {
                Some(v) => v,
                None => return false,
            },
        };
    }
    match last.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(idx) => match idx
            .parse::<usize>()
            .ok()
            .and_then(|i| cur.as_array_mut().and_then(|a| a.get_mut(i)))
        {
            Some(slot) => {
                *slot = toml::Value::String(POISON.into());
                true
            }
            None => false,
        },
        None => match cur.as_table_mut() {
            Some(t) => {
                t.insert(last.clone(), toml::Value::String(POISON.into()));
                true
            }
            None => false,
        },
    }
}

/// 一个字符串叶子被写成非法值后的结局。
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// 整份仍反序列化成功——字段级容错（或该字段本就接受任意字符串）。
    Tolerated,
    /// 整份失败，但段级降级探得出毒并回落成功。
    SalvagedBySection,
    /// 整份失败且段级降级也救不回来——用户会看到「整个文件/方案静默失效」。
    Fatal,
}

fn verdict_for<T>(seed: &toml::Value, path: &[String]) -> Verdict
where
    T: DeserializeOwned + Serialize + Default,
{
    let mut poisoned = seed.clone();
    if !poison_at(&mut poisoned, path) {
        // 回填不到的路径不参与判定（不该发生；发生了说明 `string_leaf_paths` 与
        // `poison_at` 对路径的理解不一致，宁可当作合格也不要给出假的红）。
        return Verdict::Tolerated;
    }
    if poisoned.clone().try_into::<T>().is_ok() {
        return Verdict::Tolerated;
    }
    let Ok(default_v) = toml::Value::try_from(T::default()) else {
        return Verdict::Fatal;
    };
    let patched = probe_and_patch::<T>(&poisoned, &default_v);
    if !patched.bad.is_empty() && patched.value.try_into::<T>().is_ok() {
        Verdict::SalvagedBySection
    } else {
        Verdict::Fatal
    }
}

/// 跑一遍某个类型的全部字符串叶子，返回 `(靠段级降级救回的路径, 致命路径)`。
fn scan<T>(seed: toml::Value) -> (Vec<String>, Vec<String>)
where
    T: DeserializeOwned + Serialize + Default,
{
    let mut paths = Vec::new();
    string_leaf_paths(&seed, &mut Vec::new(), &mut paths);
    let mut salvaged = Vec::new();
    let mut fatal = Vec::new();
    for p in &paths {
        let dotted = p.join(".").replace(".[", "[");
        match verdict_for::<T>(&seed, p) {
            Verdict::Tolerated => {}
            Verdict::SalvagedBySection => salvaged.push(dotted),
            Verdict::Fatal => fatal.push(dotted),
        }
    }
    salvaged.sort();
    fatal.sort();
    (salvaged, fatal)
}

/// `Config` 的种子：默认值即可（`Config::default()` 已含 `mix_modes` 等数组元素）。
fn config_seed() -> toml::Value {
    toml::Value::try_from(Config::default()).expect("Config::default 必须可序列化")
}

/// `Schema` 的种子：默认值即可——`[candidate]` / `[punct]` 在 `Schema::default()` 里是
/// 全默认的**子表**，序列化后带着键，扫得到。
///
/// ⚠️ **已知未覆盖**：`dictionaries` / `encoder` 这类默认为空数组或 `None` 的字段，其内部
/// 结构一个都扫不到（同 [`app_compat_seed`] 的第 1 个盲区）。那些结构目前没有字符串枚举；
/// 一旦往里加，得照 `app_compat_seed` 的办法在这里显式造一个元素，否则这条测试对它们**恒绿**。
fn schema_seed() -> toml::Value {
    toml::Value::try_from(Schema::default()).expect("Schema::default 必须可序列化")
}

/// `AppCompatFile` 的种子。
///
/// # ⚠️ 两个盲区，都必须在这里显式补掉
///
/// 1. `apps` 默认是**空数组**，直接 `default()` 一个规则字段都扫不到。
/// 2. 规则里的枚举字段全是 `Option` + `skip_serializing_if = "Option::is_none"`，
///    `None` 时序列化后**那个键根本不出现**——而 tri-state 覆盖字段恰恰全是这个形状，
///    正是最容易漏容错的一类（`smart_method` 就是这么漏的）。
///
/// 故这里逐个把 `Option` 字段填成 `Some`。**新增 `Option` 字段时必须在这里补一笔**——
/// 忘了补会被 [`app_compat_seed_covers_every_optional_field`] 拦下，不靠人记得。
fn app_compat_seed() -> toml::Value {
    use crate::app_compat::{AppCompatRule, FirstShowMode, InitialMode};
    use crate::config::SmartMethod;
    let rule = AppCompatRule {
        first_show_mode: Some(FirstShowMode::default()),
        initial_mode: Some(InitialMode::Chinese),
        initial_punct: Some(InitialMode::Chinese),
        smart_method: Some(SmartMethod::default()),
        auto_pair: Some(true),
        composition_start_pair_guard: Some(true),
        pin_anchor_when_start_drifts: Some(true),
        ..Default::default()
    };
    let file = AppCompatFile {
        apps: vec![rule],
        ..Default::default()
    };
    toml::Value::try_from(file).expect("AppCompatFile 必须可序列化")
}

/// 守 [`app_compat_seed`] 的覆盖率：`AppCompatRule` 的每个 `Option` 字段都必须出现在
/// 种子序列化的产物里，否则 [`compat_toml_survives_any_single_bad_string`] 会**假绿**
/// ——扫不到的字段永远不会被投毒，测试照常通过。
///
/// 判据取自源码而不是类型系统：Rust 没有字段反射，而这类「漏一个」正是本测试要防的。
#[test]
fn app_compat_seed_covers_every_optional_field() {
    let src = include_str!("app_compat.rs");
    let seed = app_compat_seed();
    let rule = seed
        .get("apps")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|r| r.as_table())
        .expect("种子里必须有一条规则");

    // `AppCompatRule` 的字段区间：从结构体定义开始到它的右花括号。
    let start = src
        .find("pub struct AppCompatRule {")
        .expect("找不到 AppCompatRule 定义");
    let body = &src[start..];
    let end = body.find("\n}").expect("找不到 AppCompatRule 的结尾");
    let body = &body[..end];

    let mut missing = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub ") else {
            continue;
        };
        let Some((name, ty)) = rest.split_once(": ") else {
            continue;
        };
        if ty.starts_with("Option<") && !rule.contains_key(name) {
            missing.push(name.to_string());
        }
    }
    assert!(
        missing.is_empty(),
        "AppCompatRule 新增了 Option 字段但没进 app_compat_seed，\
         这些字段不会被投毒、守门测试会假绿：{missing:?}"
    );
}

/// ⚠️ 两条断言的强度**刻意不同**，别顺手合并成一条。
///
/// `fatal` 是「用户看到整份/整个方案静默失效」，永远不许有。
/// `salvaged` 是「靠段级降级救回」——它能跑，但代价是用户只写错一个键、
/// 同段其它设置一起被回落成出厂值。2026-09-04 起这个名单已清零（11 个字段全部挂上
/// [`crate::tolerant_de`]），故这里钉成硬断言：**新增字段漏挂容错会在这里红**。
///
/// 名单不为空时的修法是给那个字段挂 `deserialize_with`，不是把它加进豁免表。
fn assert_no_regression(kind: &str, salvaged: &[String], fatal: &[String]) {
    assert!(
        fatal.is_empty(),
        "{kind} 里这些键写错会让整份配置/整个方案静默失效（段级降级也救不回）：{fatal:#?}"
    );
    assert!(
        salvaged.is_empty(),
        "{kind} 里这些键只能靠段级降级救回——写错一个键会连累同段其它设置一起回落出厂值。\
         给它们挂 `#[serde(deserialize_with = \"crate::tolerant_de::tolerant\")]`：{salvaged:#?}"
    );
}

#[test]
fn config_toml_survives_any_single_bad_string() {
    let (salvaged, fatal) = scan::<Config>(config_seed());
    assert_no_regression("config.toml", &salvaged, &fatal);
}

#[test]
fn schema_toml_survives_any_single_bad_string() {
    let (salvaged, fatal) = scan::<Schema>(schema_seed());
    assert_no_regression("方案文件", &salvaged, &fatal);
}

#[test]
fn compat_toml_survives_any_single_bad_string() {
    let (salvaged, fatal) = scan::<AppCompatFile>(app_compat_seed());
    // ⚠️ `app_compat::load_file` 目前**没有**段级降级：解析失败就整份 compat.toml 丢掉。
    // 故这里两类都不许有——必须是字段级容错。
    assert!(
        fatal.is_empty() && salvaged.is_empty(),
        "compat.toml 没有段级降级，这些键写错会让**整份文件**失效，必须加字段级容错：\
         fatal={fatal:#?} salvaged={salvaged:#?}"
    );
}
