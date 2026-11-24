//! 装配：把 `charsets/*.yaml` 的解析结果编译成 [`CharsetRegistry`]。
//!
//! # 为什么装配点在 wind-engine
//!
//! `wind-config` 出 [`MergedClass`]（配置形态），`wind-candidate` 收 [`ClassSpec`]
//! （判定形态），两个 crate 刻意互不依赖：判定层不该认识任何配置格式。转换因此落在
//! 同时看得见两边的最底层 crate，即本 crate。
//!
//! # 合并
//!
//! 全部类都来自 `charsets/*.yaml`（含生成的 Unicode 区块 `blocks.yaml`），按 key 字段级叠加
//! 而非替换：用户写 `key: 表情符号` + `no_freq: true` 应得到那个块加上免词频，而不是一个
//! 没有 `ranges`、一个字符都命中不了的空类。`enabled: false` 的整类丢弃。

use std::collections::BTreeMap;
use std::path::Path;

use tracing::warn;
use wind_candidate::{CharsetRegistry, ClassSpec, Scope};
use wind_config::charset_def::{Commonality, DEFAULT_ORDER, MergedClass, ScopeKind, parse_ranges};

/// 外部引用：配置里按名字指向字符类的那两个键。
///
/// 它们与类上的 `no_freq` / `in_rare` 是同一件事的两个入口（设计文档 §2.2），
/// 在装配期合并成同一份类属性——判定层因此只有一套语义，不必知道某个类的免词频
/// 是「类自己声明的」还是「被配置点名的」。
#[derive(Debug, Clone, Copy, Default)]
pub struct ExternalRefs<'a> {
    /// `schema.frequency.exclude_blocks`。
    pub no_freq: &'a [String],
    /// `input.rare_char.include_blocks`。
    pub in_rare: &'a [String],
}

/// 从 store 读用户层，解析成 docs（坏的一条跳过，见 `parse_stored_docs`）。
///
/// 放在这里而不是 `wind-config`：那个 crate 不依赖 `wind-store`，而本 crate 两边都看得见
/// ——与 `assemble` 落在这里是同一个理由。读失败只 warn 并当作空：store 打不开时其它
/// 用户数据也都是空的，字符类没理由例外。
pub fn user_docs_from_store(
    store: &wind_store::Store,
) -> Vec<wind_config::charset_def::CharsetDoc> {
    match store.list_charset_docs() {
        Ok(rows) => wind_config::charset_def::parse_stored_docs(
            rows.iter().map(|(k, t)| (k.as_str(), t.as_str())),
        ),
        Err(e) => {
            warn!("读字符类用户层失败，本次按无调整装配：{e}");
            Vec::new()
        }
    }
}

/// 同 [`user_docs_from_store`]，另按出厂校验并把问题登记到该文件（`storageStatus.issues` /
/// `data.endEdit` 带回）：解析不了 / key 与文件名不一致 ⇒ 整份忽略；出厂类改 `ranges`、写
/// `replace: true` ⇒ 去掉这两项、其余照用。没问题的文件清掉旧的不合规问题（别的问题不动）。
pub fn validated_user_docs(
    store: &wind_store::Store,
    factory: &BTreeMap<String, MergedClass>,
) -> Vec<wind_config::charset_def::CharsetDoc> {
    use wind_store::issues::{self, IssueKind};
    let rows = match store.list_charset_docs() {
        Ok(rows) => rows,
        Err(e) => {
            warn!("读字符类用户层失败，本次按无调整装配：{e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (key, text) in rows {
        let Ok(path) = store.charset_path(&key) else {
            continue;
        };
        let invalid = |message: String| issues::report(&path, IssueKind::Invalid { message });
        let mut doc = match wind_config::charset_def::parse_doc(&text) {
            Ok(d) => d,
            Err(e) => {
                warn!("字符类用户层「{key}」解析失败，已跳过：{e}");
                invalid(format!("字符分类文件格式有误，整份未生效：{e}"));
                continue;
            }
        };
        if doc.def.key != key {
            warn!("字符类用户层：文件名「{key}」与文本里的 key 不一致，已跳过");
            invalid(format!(
                "文件里的 key「{}」与文件名不一致，整份未生效",
                doc.def.key
            ));
            continue;
        }
        let mut ignored = Vec::new();
        if let Some(f) = factory.get(&key) {
            if doc.def.ranges.is_some() && f.def.ranges.is_some() && doc.def.ranges != f.def.ranges
            {
                doc.def.ranges = None;
                ignored.push("出厂字符分类的范围不能改，已忽略");
            }
            if doc.def.replace == Some(true) {
                doc.def.replace = None;
                ignored.push("出厂字符分类不能整份替换（replace），已忽略");
            }
        }
        if ignored.is_empty() {
            issues::clear_invalid(&path);
        } else {
            invalid(ignored.join("；"));
        }
        out.push(doc);
    }
    out
}

/// 装配出判定用的 registry。
///
/// `data_dir` 供 `file:` 字段走 `resolve_schema_resource` 做三层解析；`None` 时
/// 外部字表一律读不到（只 warn，不影响其余类）。
pub fn assemble(
    defs: &BTreeMap<String, MergedClass>,
    data_dir: Option<&Path>,
    refs: ExternalRefs<'_>,
) -> CharsetRegistry {
    // 代码里没有内置清单：区块在 `data/charsets/blocks.yaml`（生成物），emoji 与常用汉字各有文件。
    // 别在这里塞「配置缺失时的兜底类」，那是第二个数据源；缺文件由 `apply_ref` 的未识别名 warn 暴露。
    let mut specs: BTreeMap<String, ClassSpec> = BTreeMap::new();

    for (key, mc) in defs {
        // `enabled: false` 是删除的表达（字段级合并里 `merge_value` 表达不了删除，
        // 见设计文档 §4.1）。内置块类同样可以被这样关掉。
        if mc.def.enabled == Some(false) {
            specs.remove(key);
            continue;
        }
        let spec = specs.entry(key.clone()).or_insert_with(|| ClassSpec {
            key: key.clone(),
            order: DEFAULT_ORDER,
            ..Default::default()
        });
        apply_onto(spec, mc, data_dir);
    }

    disarm_empty_scoped_classes(&mut specs);

    apply_ref(
        &mut specs,
        refs.no_freq,
        "schema.frequency.exclude_blocks",
        |c| c.no_freq = true,
    );
    apply_ref(
        &mut specs,
        refs.in_rare,
        "input.rare_char.include_blocks",
        |c| c.in_rare = true,
    );

    let (reg, dropped) = CharsetRegistry::compile(specs.into_values().collect());
    if !dropped.is_empty() {
        warn!(
            "字符类超出上限（{} 个），以下类被丢弃、配了不生效：{}",
            wind_candidate::MAX_CLASSES,
            dropped.join(", ")
        );
    }
    // 没有任何类表态常用性 ⇒ 常用/生僻判定整个不生效（退化为不过滤）。最常见成因是部署时
    // `data/charsets/` 没跟上新的可执行文件，用户只会看到生僻字模式与检索范围过滤没反应。
    if reg
        .classes()
        .iter()
        .all(|c| c.default_common.is_none() && c.outside_common.is_none())
    {
        warn!(
            "没有任何字符类表态常用性，常用/生僻判定不生效——检查数据目录下的 charsets/ 是否随程序一起更新"
        );
    }
    let shadowed = reg.shadowed_keys();
    if !shadowed.is_empty() {
        warn!(
            "以下字符类被 order 更靠前的类完全遮住、永远轮不到：{}（调小它们的 order）",
            shadowed.join(", ")
        );
    }
    reg
}

/// 安全阀：一个类配了作用域和「域外」判定、却一个成员都没有时，撤销它的表态。
///
/// 如 `common_han`（作用域 = 汉字，名单内常用、域内名单外生僻）的字表读不到时，名单为空会让
/// 每个汉字都判生僻、候选几乎被滤光。失效方向必须是「不表态」而不是「反过来判」，同
/// `CommonChars::is_empty()`。纯段类没有补集这一半，成员为空本就无害，故只管同时有
/// scope 和 outside 的类。
fn disarm_empty_scoped_classes(specs: &mut BTreeMap<String, ClassSpec>) {
    for c in specs.values_mut() {
        let is_complement_class = c.scope.is_some() && c.outside_common.is_some();
        if is_complement_class && c.members.is_empty() && c.ranges.is_empty() {
            warn!(
                "字符类「{}」配了作用域与「域外」判定却没有任何成员（字表没读到？），\
                 本类的常用性判定已撤销——否则作用域内的字会**全部**落进域外那一档",
                c.key
            );
            c.default_common = None;
            c.outside_common = None;
        }
    }
}

/// 按名字点名一批类，给它们打上并集属性。
///
/// 必须 warn 未识别的名字：`表情符號`（繁体「號」）这种错法肉眼极难分辨，
/// 静默跳过只会表现为「配了没反应」。
fn apply_ref(
    specs: &mut BTreeMap<String, ClassSpec>,
    names: &[String],
    what: &str,
    set: impl Fn(&mut ClassSpec),
) {
    let mut unknown = Vec::new();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        match specs.get_mut(name) {
            Some(c) => set(c),
            None => unknown.push(name.to_string()),
        }
    }
    if !unknown.is_empty() {
        warn!(
            "{what} 有 {} 个名字不认识、已跳过: {}（应为字符类的 key：区块名、\"其它\"、\"符号\" 或 \"emoji\"）",
            unknown.len(),
            unknown.join("、")
        );
    }
}

/// 把一份配置**字段级叠加**到 spec 上：`Some` 的字段才写下去。
///
/// 与 `CharsetDef::merge_from` 同一条纪律，但那边合的是配置层之间的三层，这边是
/// 「代码给的内置骨架 ← 配置」。两处都逐字段 `if is_some`，漏一行是静默的。
fn apply_onto(spec: &mut ClassSpec, mc: &MergedClass, data_dir: Option<&Path>) {
    let def = &mc.def;

    if let Some(n) = &def.name {
        spec.name = n.clone();
    }
    if let Some(r) = &def.ranges {
        spec.ranges = parse_ranges(&def.key, r);
    }
    if let Some(o) = def.order {
        spec.order = o;
    }
    if let Some(s) = def.scope {
        spec.scope = Some(match s {
            ScopeKind::Han => Scope::Han,
            ScopeKind::Pua => Scope::Pua,
        });
    }
    if let Some(c) = def.default {
        spec.default_common = Some(is_common(c));
    }
    if let Some(c) = def.outside {
        spec.outside_common = Some(is_common(c));
    }
    if let Some(v) = def.no_freq {
        spec.no_freq = v;
    }
    if let Some(v) = def.in_rare {
        spec.in_rare = v;
    }

    // 内嵌列表体（一行连写多字，按字素簇切分；见 `charset_def::split_members`）。
    for m in &mc.member_order {
        if spec.members.insert(m.clone()) {
            spec.member_order.push(m.clone());
        }
    }
    // 外部字表（逐 `char`）——两种形态的差别见 `load_member_file`。
    if let Some(f) = &def.file {
        for m in load_member_file(&def.key, f, data_dir) {
            if spec.members.insert(m.clone()) {
                spec.member_order.push(m);
            }
        }
    }
    spec.excluded.extend(mc.removed.iter().cloned());
}

fn is_common(c: Commonality) -> bool {
    matches!(c, Commonality::Common)
}

/// 读 `file:` 指向的外部字表：`#` 注释行跳过，其余逐 `char` 收录。
///
/// 与内嵌列表体的「每行一个字素簇」刻意不同：`file:` 是为兼容一行几十字连写的
/// `schemas/common_chars.txt`，按行读会一个字都命中不了。多码位簇（`👨‍👩‍👧`）请用内嵌列表体。
fn load_member_file(key: &str, rel: &str, data_dir: Option<&Path>) -> Vec<String> {
    let Some(path) = wind_config::Config::resolve_schema_resource(data_dir, rel) else {
        warn!("字符类「{key}」的字表 {rel} 找不到，本类的外部成员为空");
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        warn!("字符类「{key}」的字表 {} 读不出来", path.display());
        return Vec::new();
    };
    // 行尾规整：孤立 \r 会让整份文件算成一行，`#` 开头的表就被整个当成注释
    // 跳过——与 `CommonChars::load` 读同一种文件，症状也一样（上面那句「全程无
    // 报错」正是它）
    let content = wind_utils::text::normalize_input(&content);
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .flat_map(|l| l.chars().map(|c| c.to_string()).collect::<Vec<_>>())
        .collect()
}

#[cfg(test)]
#[path = "charset_assembly_tests.rs"]
mod tests;
