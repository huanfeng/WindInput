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

/// 外部引用：配置里**按名字**指向字符类的那两个键。
///
/// ★ 它们与类上的 `no_freq` / `in_rare` 是同一件事的两个入口（设计文档 §2.2），
/// 在装配期合并成同一份类属性——判定层因此只有一套语义，不必知道某个类的免词频
/// 是「类自己声明的」还是「被配置点名的」。
#[derive(Debug, Clone, Copy, Default)]
pub struct ExternalRefs<'a> {
    /// `schema.frequency.exclude_blocks`。
    pub no_freq: &'a [String],
    /// `input.rare_char.include_blocks`。
    pub in_rare: &'a [String],
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
    // ⚠️ 没有任何类表态常用性 ⇒ 常用/生僻判定整个不生效（一切兜底判常用）。
    //
    // 最常见的成因是**部署时 `data/charsets/` 没跟上**：旧的 data 目录配新的可执行文件。
    // 失效方向是安全的（退化为不过滤，同 `CommonChars::is_empty()`），但用户看到的是
    // 「生僻字模式什么都出不来」「检索范围过滤没反应」，而不会想到是少了个目录。
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

/// ★★ 安全阀：一个类**配了作用域和「域外」判定、却一个成员都没有**时，撤销它的表态。
///
/// # 为什么必须有这一条
///
/// `common_han` 的形态是「作用域 = 汉字，名单 = common_chars.txt，名单内常用、
/// **域内名单外生僻**」。字表读不到时（路径变了、文件被删、定制包漏打），名单为空而
/// 作用域还在 ⇒ **每一个汉字都落进「域内、名单外」⇒ 全部判生僻**，用户的候选被过滤到
/// 几乎不剩。而全程只有一条 warn。
///
/// 失效方向必须是「这个类不表态」（回到没有它的样子），不是「反过来判」。同款防线是
/// `CommonChars::is_empty()`——那里也是拿「出厂表没装进来」当作**退化为不过滤**的信号，
/// 而不是拿空表去过滤。
///
/// ⚠️ 只对**同时有 scope 和 outside** 的类生效。纯段类成员为空只是它不命中任何字符，
/// 没有「补集」这一半，本来就无害。
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
/// ⚠️ **必须 warn 未识别的名字**：这些名字是中文串，`表情符號`（繁体「號」）这种错法
/// 肉眼极难分辨，而静默跳过的表现就是「配了没反应」，用户无从判断是名字写错还是功能
/// 没生效。这条职责从 `Manager::parse_freq_exclude` 原样搬过来。
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
        let reg = assemble(&BTreeMap::new(), None, ExternalRefs::default());
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
        let reg = assemble(&defs, None, ExternalRefs::default());
        assert!(reg.no_freq("😀"), "块的 ranges 被覆盖掉了");
        assert!(!reg.no_freq("我"), "免词频不该外溢到块外");
    }

    /// `enabled: false` 能把内置块类整个关掉。
    #[test]
    fn disabled_class_is_dropped() {
        let defs = merged("---\nkey: 表情符号\nenabled: false\n");
        let reg = assemble(&defs, None, ExternalRefs::default());
        assert!(reg.class_by_key("表情符号").is_none());
        // 关掉一个块不该动到别的块。
        assert!(reg.class_by_key("基本汉字").is_some());
    }

    /// 自定义类：yaml 里没有的 key 就新建，`order` 缺省走 `DEFAULT_ORDER`。
    #[test]
    fn a_custom_class_is_created_with_default_order() {
        let defs = merged("---\nkey: mine\nranges:\n  - U+2600-U+26FF\ndefault: rare\n");
        let reg = assemble(&defs, None, ExternalRefs::default());
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
        let reg = assemble(&defs, None, ExternalRefs::default());
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
        let reg = assemble(&defs, None, ExternalRefs::default());
        assert!(!reg.no_freq("😀"), "被删掉的成员不该还命中");
        assert!(reg.no_freq("😁"), "同块的其余字符不受影响");
    }

    /// ★★ 字表读不到时，「域内名单外 ⇒ 生僻」这一半必须**撤销**，不能拿空名单去判。
    ///
    /// 没有这条防线，`common_chars.txt` 一旦读不到，每个汉字都落进「域内、名单外」
    /// ⇒ 全部判生僻 ⇒ 用户候选被过滤到几乎不剩，而只有一条 warn。
    #[test]
    fn a_scoped_class_without_members_gives_up_its_verdict() {
        let defs = merged(concat!(
            "---
",
            "key: common_han
",
            "scope: han
",
            "file: 这个文件不存在.txt
",
            "default: common
",
            "outside: rare
",
        ));
        let reg = assemble(&defs, None, ExternalRefs::default());
        assert_eq!(reg.verdict_of("我"), None, "字表没读到就不该表态");
        assert_eq!(reg.verdict_of("龘"), None, "更不该把汉字全判成生僻");
    }

    /// 对照：名单读得到时，「域内名单外 ⇒ 生僻」照常工作——防线不该误伤正常情形。
    #[test]
    fn a_scoped_class_with_members_keeps_its_verdict() {
        let defs = merged(concat!(
            "---
",
            "key: common_han
",
            "scope: han
",
            "default: common
",
            "outside: rare
",
            "...
",
            "我
",
        ));
        let reg = assemble(&defs, None, ExternalRefs::default());
        assert_eq!(reg.verdict_of("我"), Some(true));
        assert_eq!(reg.verdict_of("龘"), Some(false), "域内名单外该判生僻");
    }

    /// 缺 `file:` 只让该类的外部成员为空，不影响它已有的其余定义。
    #[test]
    fn a_missing_member_file_does_not_kill_the_class() {
        let defs =
            merged("---\nkey: mine\nfile: nope.txt\nranges:\n  - U+2600-U+26FF\ndefault: rare\n");
        let reg = assemble(&defs, None, ExternalRefs::default());
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
        let reg = assemble(&defs, None, ExternalRefs::default());
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
