//! 段级降级：一段有毒只丢那一段，而不是让整份文件失败。
//!
//! # 这套机制回答的问题
//!
//! `toml::Value::try_into::<T>()` 是**整份一次性**反序列化：一个字段的值不在值域里，
//! 整个 `T` 就 `Err`。落到调用方手里通常是 `None`，于是「一个字段写错」放大成
//! 「这份文件/这个方案/这一层整个不存在」，且往往只剩一行 `warn!`——用户看到的是
//! **完全没反应，但也不工作**。
//!
//! 本模块把爆炸半径收敛到「顶层段」，能细化时进一步收敛到「段里的一个直接子键」：
//! 有毒的路径回落出厂默认值，其余部分照常生效，并把降级清单交回调用方去报给用户。
//!
//! # ★ 为什么是泛型，而不是留在 `Config` 里
//!
//! 这套逻辑最初只为 `config.toml` 写（`Config::deserialize_with_section_fallback`），
//! 而同样的病在别处一模一样地复发过：`schema_overrides/<id>.toml` 里一个
//! `text_orientation = ""` 让**整个方案**读不出来（2026-09-04）。判据与修法完全相同，
//! 差别只在 `T`。故这里只依赖 `T: DeserializeOwned`，由调用方提供「全默认骨架」。
//!
//! ⚠️ **降级不是迁移的替代品**。字段类型/值域收缩（如 `layout` 曾接受 `upright`）
//! 必须写显式迁移；靠降级「自愈」会把迁移缺失变成静默回落出厂值，
//! 见 `Config::deserialize_with_section_fallback` 里那条 WARN 的理由。

use serde::de::DeserializeOwned;

/// 段级降级的**单次探针**：把 `value` 贴到全默认骨架 `default_v` 的 `path` 处再整体
/// 反序列化。失败即说明毒在这条路径底下，返回该路径**自己的**错误文本。
///
/// 骨架用默认值而不是用户值，是这套机制正确性的来源：其余部分恒定合法，于是失败只可能
/// 来自贴上去的那一段，判定互不干扰。
pub(crate) fn probe_section<T: DeserializeOwned>(
    default_v: &toml::Value,
    path: &[&str],
    value: &toml::Value,
) -> Option<String> {
    let mut probe = default_v.clone();
    let (last, parents) = path.split_last()?;
    let mut cur = &mut probe;
    for seg in parents {
        // 骨架里没有这条路径 = 未登记键。serde 会忽略它，探不出毒，也不该降级任何东西。
        cur = cur.get_mut(*seg)?;
    }
    cur.as_table_mut()?
        .insert((*last).to_string(), value.clone());
    probe.try_into::<T>().err().map(|e| e.to_string())
}

/// 对**已判定为坏**的顶层段再探一层：逐个直接子键做探针，返回 `(段.子键, 该子键的错误)`。
///
/// 返回空表示无法细化（该段在用户值或默认值里不是表、或毒不在任何单个子键上），调用方
/// 退回整段降级。**只探这一层**，不再往下递归。
pub(crate) fn narrow_bad_section<T: DeserializeOwned>(
    default_v: &toml::Value,
    section: &str,
    section_value: &toml::Value,
) -> Vec<(String, String)> {
    let (Some(sub), Some(_)) = (
        section_value.as_table(),
        default_v.get(section).and_then(|v| v.as_table()),
    ) else {
        return Vec::new();
    };
    sub.iter()
        .filter_map(|(key, value)| {
            probe_section::<T>(default_v, &[section, key], value)
                .map(|err| (format!("{section}.{key}"), err))
        })
        .collect()
}

/// 把 `root` 里 `path`（点分）处的值换成 `default_v` 同路径的默认值；默认值里没有则删除。
///
/// 删除而非保留：路径在默认值里不存在意味着它不是配置键，而它又被探针判成了毒——
/// 带进最终值只会让 `try_into` 再失败一次。
fn reset_path_to_default(root: &mut toml::Value, default_v: &toml::Value, path: &str) {
    let segs: Vec<&str> = path.split('.').collect();
    let Some((last, parents)) = segs.split_last() else {
        return;
    };
    let mut cur = root;
    for seg in parents {
        let Some(next) = cur.get_mut(*seg) else {
            return;
        };
        cur = next;
    }
    let Some(table) = cur.as_table_mut() else {
        return;
    };
    let mut def = default_v;
    for seg in &segs {
        match def.get(*seg) {
            Some(v) => def = v,
            None => {
                table.remove(*last);
                return;
            }
        }
    }
    table.insert((*last).to_string(), def.clone());
}

/// 一次段级降级的产物。
pub struct Patched {
    /// 有毒路径已回落出厂默认值的 TOML 值。调用方仍需自己 `try_into::<T>()`——
    /// 探针探不出毒时它与输入相同，那时反序列化**仍会失败**，须有整体回落分支。
    pub value: toml::Value,
    /// `(点分路径, 该路径自己的反序列化错误)`，已排序。空 = 没探出任何有毒的段。
    ///
    /// ⚠️ 每条都带**自己的**错误：多段同时有毒时，整份 `try_into` 的错误只讲得清其中
    /// 一个段，拿它给每一行 WARN 用会把排查的人直接带到无关的段上。
    pub bad: Vec<(String, String)>,
}

/// 逐顶层段探毒并回落：对 `merged` 的每个顶层段做探针，坏段能细化到子键就细化，
/// 然后把所有坏路径换成 `default_v` 的同路径默认值。
///
/// `default_v` 必须是 `T::default()` 序列化出的**全默认骨架**（见 `probe_section`）。
/// 调用方须先自行确认 `merged` 确实反序列化失败——本函数不做这道判断，健康输入进来
/// 只会白跑一遍探针并返回空 `bad`。
pub fn probe_and_patch<T: DeserializeOwned>(
    merged: &toml::Value,
    default_v: &toml::Value,
) -> Patched {
    let mut bad: Vec<(String, String)> = Vec::new();
    if let Some(sections) = merged.as_table() {
        for (section, value) in sections {
            let Some(section_err) = probe_section::<T>(default_v, &[section], value) else {
                continue;
            };
            let narrowed = narrow_bad_section::<T>(default_v, section, value);
            if narrowed.is_empty() {
                bad.push((section.clone(), section_err));
            } else {
                bad.extend(narrowed);
            }
        }
    }
    // 顶层不是表时上面一条都收不到，调用方会落到自己的整体回落分支。
    //
    // 显式排序而不是依赖 `toml::Table` 的遍历序：后者是否有序取决于 `preserve_order`
    // 特性，而特性可能被任何一个传递依赖打开——那种翻车只在别人的依赖树里复现。
    bad.sort();

    let mut value = merged.clone();
    for (path, _) in &bad {
        reset_path_to_default(&mut value, default_v, path);
    }
    Patched { value, bad }
}
