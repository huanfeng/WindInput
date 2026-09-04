//! 装配：把 `charsets/*.yaml` 的解析结果与内置区块类编译成 [`CharsetRegistry`]。
//!
//! # 为什么装配点在 wind-engine
//!
//! `wind-config` 出 [`MergedClass`]（配置形态），`wind-candidate` 收 [`ClassSpec`]
//! （判定形态），两个 crate **互不依赖**——这道边界是刻意的，判定层不该认识任何配置
//! 格式（`19d8c80a` 把容器从 TOML 单文件换成 YAML 目录扫描时，判定层一行没动，靠的
//! 就是它）。转换因此必须落在同时看得见两边的地方，本 crate 是其中最底层的一个。
//!
//! # 合并顺序
//!
//! ```text
//! 内置区块类（50 块 + 「其它」+ 「符号」）
//!   ← charsets/*.yaml 按 key **字段级叠加**（不是替换）
//!   ← enabled: false 的整类丢弃
//! ```
//!
//! ★ 叠加而非替换，是为了让「给某个内置块配一条属性」这件事可行：用户写
//! `key: 表情符号` + `no_freq: true`，得到的应当是**那个块加上免词频**，而不是一个
//! 没有 `ranges` 的空类——后者会让配置看着生效、实际一个字符都命中不了。

use std::collections::BTreeMap;
use std::path::Path;

use tracing::warn;
use wind_candidate::{CharsetRegistry, ClassSpec, Scope, builtin_block_specs};
use wind_config::charset_def::{Commonality, DEFAULT_ORDER, MergedClass, ScopeKind, parse_ranges};

/// 装配出判定用的 registry。
///
/// `data_dir` 供 `file:` 字段走 `resolve_schema_resource` 做三层解析；`None` 时
/// 外部字表一律读不到（只 warn，不影响其余类）。
pub fn assemble(defs: &BTreeMap<String, MergedClass>, data_dir: Option<&Path>) -> CharsetRegistry {
    let mut specs: BTreeMap<String, ClassSpec> = builtin_block_specs()
        .into_iter()
        .map(|s| (s.key.clone(), s))
        .collect();

    for (key, mc) in defs {
        // `enabled: false` 是**删除**的表达（字段级合并里 `merge_value` 表达不了删除，
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

    let (reg, dropped) = CharsetRegistry::compile(specs.into_values().collect());
    if !dropped.is_empty() {
        warn!(
            "字符类超出上限（{} 个），以下类被丢弃、配了不生效：{}",
            wind_candidate::MAX_CLASSES,
            dropped.join(", ")
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

    // 内嵌列表体（每行一个**字素簇**）。
    spec.members.extend(mc.members.iter().cloned());
    // 外部字表（逐 `char`）——两种形态的差别见 `load_member_file`。
    if let Some(f) = &def.file {
        spec.members.extend(load_member_file(&def.key, f, data_dir));
    }
    spec.excluded.extend(mc.removed.iter().cloned());
}

fn is_common(c: Commonality) -> bool {
    matches!(c, Commonality::Common)
}

/// 读 `file:` 指向的外部字表：`#` 注释行跳过，其余**逐 `char`** 收录。
///
/// ⚠️ **与内嵌列表体的「每行一个字素簇」刻意不同**，因为 `file:` 存在的唯一理由就是
/// 兼容 `schemas/common_chars.txt`——那个文件是一行几十个字连写的，按「每行一簇」读
/// 会把整行当成一个成员，结果是**一个字都命中不了**，而全程无报错。
///
/// ⇒ 要表达多码位簇（`👨‍👩‍👧`、`1️⃣`）就用内嵌列表体，`file:` 给不了这个表达力。
fn load_member_file(key: &str, rel: &str, data_dir: Option<&Path>) -> Vec<String> {
    let Some(path) = wind_config::Config::resolve_schema_resource(data_dir, rel) else {
        warn!("字符类「{key}」的字表 {rel} 找不到，本类的外部成员为空");
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        warn!("字符类「{key}」的字表 {} 读不出来", path.display());
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .flat_map(|l| l.chars().map(|c| c.to_string()).collect::<Vec<_>>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wind_config::charset_def::{CharsetDef, parse_doc};

    fn merged(text: &str) -> BTreeMap<String, MergedClass> {
        let mut m = BTreeMap::new();
        wind_config::charset_def::apply_doc(&mut m, parse_doc(text).expect("解析失败"));
        m
    }

    /// 出厂零配置：装配出来的 registry **一个字符都不表态**，行为与本机制出现之前一致。
    #[test]
    fn no_config_means_no_opinion() {
        let reg = assemble(&BTreeMap::new(), None);
        for probe in ["我", "龘", "😀", "★", "a", "、"] {
            assert_eq!(reg.verdict_of(probe), None, "{probe} 不该有人表态");
            assert!(!reg.no_freq(probe), "{probe} 不该免词频");
            assert!(!reg.in_rare(probe), "{probe} 不该进生僻模式");
        }
    }

    /// ★★ 给内置块配一条属性时，块的 `ranges` 必须留着——这是「叠加而非替换」那条。
    ///
    /// 替换语义下这个类会没有 `ranges`、一个字符都命中不了，而用户看到的是
    /// 「我配了 no_freq，没反应」。
    #[test]
    fn overlaying_a_builtin_block_keeps_its_ranges() {
        let defs = merged("---\nkey: 表情符号\nno_freq: true\n");
        let reg = assemble(&defs, None);
        assert!(reg.no_freq("😀"), "块的 ranges 被覆盖掉了");
        assert!(!reg.no_freq("我"), "免词频不该外溢到块外");
    }

    /// `enabled: false` 能把内置块类整个关掉。
    #[test]
    fn disabled_class_is_dropped() {
        let defs = merged("---\nkey: 表情符号\nenabled: false\n");
        let reg = assemble(&defs, None);
        assert!(reg.class_by_key("表情符号").is_none());
        // 关掉一个块不该动到别的块。
        assert!(reg.class_by_key("基本汉字").is_some());
    }

    /// 自定义类：yaml 里没有的 key 就新建，`order` 缺省走 `DEFAULT_ORDER`。
    #[test]
    fn a_custom_class_is_created_with_default_order() {
        let defs = merged("---\nkey: mine\nranges:\n  - U+2600-U+26FF\ndefault: rare\n");
        let reg = assemble(&defs, None);
        let c = reg.class_by_key("mine").expect("自定义类没进去");
        assert_eq!(c.order, DEFAULT_ORDER);
        assert_eq!(reg.verdict_of("☀"), Some(false));
    }

    /// ★ 自定义类的 `order`（缺省 100）比内置块类（900）靠前 ⇒ 它表的态说了算。
    ///
    /// 反过来的话，用户新建一个类怎么配都不会生效——而内置块类全都不表态，
    /// 这种「被不表态的类挡住」是最难查的一种没反应。
    #[test]
    fn a_custom_class_outranks_the_builtin_blocks() {
        let defs = merged("---\nkey: mine\nranges:\n  - U+4E00-U+4EFF\ndefault: rare\n");
        let reg = assemble(&defs, None);
        assert_eq!(reg.verdict_of("一"), Some(false));
        assert_eq!(
            reg.class_of("一").map(|c| c.key.as_str()),
            Some("mine"),
            "仲裁赢家应当是表了态的那个"
        );
    }

    /// 列表体的 `-` 删除对内置块的 `ranges` 也生效（`excluded` 优先于一切）。
    #[test]
    fn removal_beats_a_builtin_range() {
        let defs = merged("---\nkey: 表情符号\nno_freq: true\n...\n-😀\n");
        let reg = assemble(&defs, None);
        assert!(!reg.no_freq("😀"), "被删掉的成员不该还命中");
        assert!(reg.no_freq("😁"), "同块的其余字符不受影响");
    }

    /// 缺 `file:` 只让该类的外部成员为空，不影响它已有的其余定义。
    #[test]
    fn a_missing_member_file_does_not_kill_the_class() {
        let defs =
            merged("---\nkey: mine\nfile: nope.txt\nranges:\n  - U+2600-U+26FF\ndefault: rare\n");
        let reg = assemble(&defs, None);
        assert_eq!(reg.verdict_of("☀"), Some(false), "ranges 那半边该照常工作");
    }

    /// 装配对每个可叠加字段都真的写了一行——漏一行是静默的（配了没反应）。
    ///
    /// 与 `CharsetDef::merge_covers_every_field` 同款：那条钉配置三层之间的合并，
    /// 这条钉「配置 → 判定形态」这一跳。
    #[test]
    fn apply_covers_every_field() {
        let defs = merged(concat!(
            "---\n",
            "key: mine\n",
            "name: 我的类\n",
            "ranges:\n  - U+2600-U+26FF\n",
            "scope: pua\n",
            "default: rare\n",
            "outside: common\n",
            "order: 7\n",
            "no_freq: true\n",
            "in_rare: true\n",
            "...\n",
            "★\n",
            "-☀\n",
        ));
        let reg = assemble(&defs, None);
        let c = reg.class_by_key("mine").expect("类没进去");
        assert_eq!(c.name, "我的类");
        assert_eq!(c.ranges, vec![(0x2600, 0x26FF)]);
        assert_eq!(c.scope, Some(Scope::Pua));
        assert_eq!(c.default_common, Some(false));
        assert_eq!(c.outside_common, Some(true));
        assert_eq!(c.order, 7);
        assert!(c.no_freq);
        assert!(c.in_rare);
        assert!(c.members.contains("★"));
        assert!(c.excluded.contains("☀"));
        // `CharsetDef` 剩下的字段（key / file / enabled / replace）都不是「叠加到
        // ClassSpec 上的属性」：key 是主键，file 与列表体同归 members，enabled 在
        // 本函数之前就决定了这个类进不进来，replace 由配置层消化。
        let _ = CharsetDef::default();
    }
}
