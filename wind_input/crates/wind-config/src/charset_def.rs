//! 字符类定义（`charset.toml`）的解析与三层合并。
//!
//! 设计与全部取舍见 `docs/design/charset-classification.md`。本模块只负责
//! **把配置读成结构**，不做判定——判定结构的编译（分段表、仲裁）在 `wind-candidate`。
//!
//! # ⛔ 不要和 [`crate::code_charset`] 混淆
//!
//! 那个是**码元**字符集（`[engine.codetable].input_chars`，回答「这一次按键算不算输入码」，
//! 值域是 ASCII 按键）。本模块是**字符类**（回答「这个字符属于哪一类」，值域是全 Unicode）。
//! 两者除了名字都带 charset 之外没有任何关系，故本模块的类型一律叫 `Charset*` 而不是
//! `CharSet`。
//!
//! # 与 `compat.toml` 同构，但合并语义**必须**不同
//!
//! 三层加载（`data/` → `data_custom/` → `user_config/`）、用户层由 GUI 整份重写、系统层
//! 程序绝不改写——这几条照抄 [`crate::app_compat`]。**唯一的偏离是合并粒度**：
//!
//! | | 合并语义 | 为什么 |
//! |---|---|---|
//! | `compat.toml` | 同进程名**整条覆盖** | 规则字段少且全是用户可设的 |
//! | 本模块 | **字段级合并** | `ranges` 是出厂数据，整条覆盖会把它丢掉 |
//!
//! ★★★ 用户在 GUI 里只改 emoji 的 `default`，若按整条覆盖，用户层那条 `[charset.emoji]`
//! 就只有一个 `default` 字段，系统层的 `ranges` / `name` 全部丢失——**这个类当场变成空集**。
//! 更隐蔽的是反方向：若 GUI 为了避免丢失而把整条定义（含 `ranges`）都写回用户层，那么
//! 出厂 `ranges` 升级（Unicode 出新版）对这个用户**永久失效**，且界面上看不出任何异常。
//! 字段级合并同时避开这两个坑：用户层只存他改过的字段。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tracing::warn;

/// 字符类定义文件名。
pub const CHARSET_FILE_NAME: &str = "charset.toml";

/// 写回用户层 `charset.toml` 时的固定文件头。
///
/// 与 `compat.toml` 同一条纪律：用户层由设置页整份重写（TOML 序列化不保留注释），
/// 故必须在文件里就把这件事讲明白，否则用户手写的说明被吞掉时无从得知原因。
const USER_CHARSET_HEADER: &str = "\
# 用户层字符类定义（覆盖 / 追加系统层 data/charset.toml）
#
# ⚠ 本文件由输入法设置页自动管理，每次在界面上改动都会整份重写，
#   手写的注释与排版不会保留。需要长期留存的说明请写在系统层 charset.toml。
#
# 合并语义：**按字段**覆盖系统层同名类，未写的字段沿用系统层。
#   故这里通常只有你改过的那几个字段，看不到 ranges 是正常的。
# 字段说明见系统层 data/charset.toml 顶部注释。

";

/// 常用性。三态由 `Option<Commonality>` 表达：`None` = **不表态**，不参与仲裁。
///
/// ★ 「不表态」必须是可表达的：那 50 个内置区块类存在的理由只是给类型列一个标签，
/// 强迫它们在常用性上表态，等于让用户在一个他从没想过的问题上做选择，而任一选择
/// 都会改变现有行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Commonality {
    /// 判为常用（进「常用字」「智能」两档检索范围）。
    Common,
    /// 判为生僻。
    Rare,
}

impl Commonality {
    /// `true` = 常用。判定链内部用 bool，与既有的 `is_string_common` 同域。
    pub fn is_common(self) -> bool {
        matches!(self, Self::Common)
    }
}

/// 类的**作用域**来源。
///
/// # ⛔ 值域是闭集，不接受用户写的码位段
///
/// 作用域回答「这个类管得着谁」，它背后是**判定域**（`is_han ∪ is_pua` 那张表）。
/// 判定域漏一段的后果是**那批字恒判常用、过滤静默失效**（issue #83 就是差一个码位），
/// 而显示域（`ranges`）漏一段只是标签显示「其它」。
///
/// ⇒ 判定域的完整性必须由代码保证。用户能自定义的只有 `ranges`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum ScopeKind {
    /// 汉字 ∪ 私用区——即现有的 `is_common_scope`，默认字表的管辖域。
    Han,
    /// 私用区。
    Pua,
}

/// 一个字符类的定义。
///
/// **所有字段都是 `Option`**，这是字段级合并的前提：`None` = 本层没说话，沿用下层。
/// 若某字段用非 Option 类型 + `#[serde(default)]`，「用户没写」与「用户写了默认值」
/// 在合并时无法区分，前者会把下层的值覆盖成默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharsetDef {
    /// 显示名。缺省 = key 本身（内置类的 key 就是中文名，故通常不必写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// 码位段，如 `['U+2600-U+26FF']`。语法见 [`parse_range`]。
    ///
    /// ⚠️ **GUI 不写这个字段**（内置类只读）。整体替换语义对它是危险的——见模块头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<String>>,

    /// 成员来自字表文件（相对 `schemas/`，走 `resolve_schema_resource` 的整体替换语义，
    /// 故用户把同名文件放进用户配置目录即可换掉整本字表）。
    ///
    /// ★★ **emoji 也走这一支**（`emoji_chars.txt`），不是内置判定函数。初稿曾因
    /// 「keycap 基字符扣不掉、序列级判定表达不了」把它定成内置，但那两条只否得掉
    /// **区间**——列表逐条列举，不列 `0-9 # *` 即可，键又是字素簇、装得下序列。
    /// 而内置是唯一用户改不了的形态，与「让判据可自定义」这个改造目的直接冲突。
    /// 论证见设计文档 §5.2。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// 作用域：本类「管得着」谁。配了它，`outside` 才有意义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeKind>,

    /// GUI 写的稀疏增补：这些字素簇也算本类成员。
    ///
    /// ★ 与 `ranges` 分开是刻意的：`ranges` 是用户手写的骨架、GUI 不碰；本字段是 GUI
    /// 写的调整、出厂恒空。**整体替换对本字段是安全的**（没有出厂值可丢），对 `ranges`
    /// 则不然。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<String>>,

    /// GUI 写的稀疏排除：这些字素簇不算本类成员（优先于 `ranges` / `add`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>,

    /// 成员的常用性默认判定。`None` = 不表态，不参与仲裁。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Commonality>,

    /// **作用域内、成员外**的常用性判定。只在配了 `scope` 时有意义。
    ///
    /// ★★ 这个字段承载的是「生僻字是补集」那件事：`common_han` 的成员是 8104 字的白名单，
    /// 而「是汉字、却不在名单里 ⇒ 生僻」用成员关系表达不了——那个字压根不是成员。
    /// 缺了它，「换一本常用字表」这个需求做不到。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside: Option<Commonality>,

    /// 仲裁顺序，**小的优先**。缺省见 [`DEFAULT_ORDER`]。
    ///
    /// ★ 用显式数字而不是配置里的书写顺序：书写顺序在**稀疏覆盖**里表达不了——用户层
    /// 只写一个 `[charset.emoji]`，它该插在系统层那串类的哪个位置？改成一个数就可以只写
    /// 这个数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,

    /// 本类成员不参与词频学习与重排。
    ///
    /// ⚠️ **并集语义**（任一命中的类为真即真），与 `default` 的仲裁语义不同——见设计文档
    /// §2.2。多免一个字符是安全方向；改成仲裁会让一个本该免的字符不免。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_freq: Option<bool>,

    /// 本类成员额外纳入生僻字模式。并集语义，同 `no_freq`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_rare: Option<bool>,

    /// 关掉这个类。
    ///
    /// ★ 存在的理由：三层合并只能新增/覆盖，**表达不了删除**。用户想去掉一个内置类时，
    /// 没有这个字段就只能靠「把 ranges 覆盖成空数组」这类偏方，而那会连带改变
    /// 类型列的显示。本仓已在 `key_actions` 物化与扩展词库 override 上踩过同一个坑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// `order` 缺省值。取中间值而不是 0，让用户既能插到内置类前面（负数/小数值）
/// 也能插到后面，不必给所有内置类重新编号。
pub const DEFAULT_ORDER: i32 = 100;

impl CharsetDef {
    /// 字段级合并：`overlay` 里 `Some` 的字段覆盖 `self`，`None` 的保留。
    fn merge_from(&mut self, overlay: CharsetDef) {
        // 逐字段 `if let Some` 而不是 `unwrap_or(self.x)`：后者对 `Vec` 要克隆，
        // 且新增字段时漏写一行不会有任何提示。这里漏写同样静默，但至少形态一致、
        // 一眼能看出少了哪个字段——由 `merge_covers_every_field` 钉住。
        macro_rules! take {
            ($($f:ident),+ $(,)?) => { $( if overlay.$f.is_some() { self.$f = overlay.$f; } )+ };
        }
        take!(
            name, ranges, file, scope, add, remove, default, outside, order, no_freq, in_rare,
            enabled
        );
    }

    /// 这个类是否被关掉。缺省为开。
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// 仲裁顺序，缺省 [`DEFAULT_ORDER`]。
    pub fn order_or_default(&self) -> i32 {
        self.order.unwrap_or(DEFAULT_ORDER)
    }
}

/// `charset.toml` 的文件形态。
///
/// ⛔ 用**表** `[charset.<key>]` 而不是数组表 `[[charset]]`：数组在合并里是整体替换，
/// 用户改一个字段就要重写整个数组 ⇒ 内置类的完整定义（含 `ranges`）被固化进用户层 ⇒
/// 出厂更新对该用户永久失效。表形式还顺带让 key 天然唯一。论证见设计文档 §4.1。
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CharsetFile {
    /// key → 定义。`BTreeMap` 而非 `HashMap`：写回时键序稳定，否则每次整份重写都会
    /// 打乱行序，用户拿 diff 看自己改了什么会看到一片无关变动。
    #[serde(default)]
    pub charset: BTreeMap<String, CharsetDef>,
}

/// 一个码位段（闭区间）。
pub type CodeRange = (u32, u32);

/// 解析一条码位段：`U+4E00-U+9FFF`（闭区间）或 `U+4E00`（单点）。
///
/// # 为什么不是 `一-￿`
///
/// 1. TOML 的基本字符串里 `\u` 必须跟满 4 位十六进制，`\u00-\uFF` 直接**解析报错**；
/// 2. `\u` 转义长度定死 4 位（或 8 位 `\U`），写不了 `U+1F600` 这种 5 位码位；
/// 3. `U+XXXX` 是 Unicode 自己的标准写法（UAX #42），用户在任何 Unicode 资料里见到的
///    都是这个形态。
///
/// 配置里请用 TOML 的**字面量字符串**（单引号）包裹，虽然本语法不含反斜杠、
/// 用双引号也不会出错，但单引号能让「这里面的东西不经转义」这件事一眼可见。
pub fn parse_range(s: &str) -> Result<CodeRange, String> {
    let t = s.trim();
    // 从右边找分隔符：码位本身不含 `-`，但将来若支持负数 order 之类的语法，
    // 从右找更不容易撞上。空串会在 `split_at` 前被下面的解析拦住。
    let (lo_s, hi_s) = match t.split_once('-') {
        Some((a, b)) => (a, b),
        None => (t, t), // 单点
    };
    let lo = parse_code_point(lo_s)?;
    let hi = parse_code_point(hi_s)?;
    if lo > hi {
        return Err(format!("段起点大于终点：{t}"));
    }
    Ok((lo, hi))
}

/// 解析单个码位 `U+XXXX`。前缀大小写不敏感；允许 4~6 位十六进制。
fn parse_code_point(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let hex = t
        .strip_prefix("U+")
        .or_else(|| t.strip_prefix("u+"))
        .ok_or_else(|| format!("缺少 U+ 前缀：{t}"))?;
    if hex.is_empty() || hex.len() > 6 {
        return Err(format!("码位长度不合法（应为 1~6 位十六进制）：{t}"));
    }
    let v = u32::from_str_radix(hex, 16).map_err(|_| format!("不是十六进制码位：{t}"))?;
    // 上界按 Unicode 码位空间判，而不是 `char::from_u32`：代理区 D800-DFFF 不是合法
    // `char`，但作为**区间端点**出现在配置里是合理的（用户写 `U+0000-U+FFFF` 圈整个 BMP
    // 没有错）。逐字符判定时代理区本就不可能命中——`char` 里不存在这些值。
    if v > 0x10FFFF {
        return Err(format!("码位超出 Unicode 上界 10FFFF：{t}"));
    }
    Ok(v)
}

/// 解析一组码位段，**逐条容错**：返回解析成功的段，失败的逐条 warn。
///
/// ⛔ 不得因为一条写错就丢掉整个类（`.ok().unwrap_or(空)` = 丢数据）：那会让用户的
/// 一个笔误静默地把整类字符从判定里抹掉，而配置文件看上去完全正常。
pub fn parse_ranges(key: &str, raw: &[String]) -> Vec<CodeRange> {
    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        match parse_range(r) {
            Ok(v) => out.push(v),
            Err(e) => warn!("字符类「{key}」的码位段被忽略：{e}"),
        }
    }
    out.sort_unstable();
    out
}

/// 读一层文件。不存在或解析失败返回 `None`（调用方按「本层没说话」处理）。
///
/// 解析失败只 warn 不中断：与 `compat.toml` 同一条纪律——用户手改坏了 TOML 时，
/// 设置页仍要能打开并把它改回来，宁可这一层不生效也不要卡死。
fn load_file(path: &Path) -> Option<CharsetFile> {
    let text = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<CharsetFile>(&text) {
        Ok(f) => Some(f),
        Err(e) => {
            warn!("解析 {} 失败，本层字符类定义被忽略：{e}", path.display());
            None
        }
    }
}

/// 三层加载，层序 `data < data_custom < user`，靠后者**按字段**覆盖靠前者。
pub fn load_layered(
    data_dir: Option<&Path>,
    custom_dir: Option<&Path>,
    user_dir: Option<&Path>,
) -> CharsetFile {
    let mut merged = CharsetFile::default();
    for dir in [data_dir, custom_dir, user_dir].into_iter().flatten() {
        if let Some(layer) = load_file(&dir.join(CHARSET_FILE_NAME)) {
            merge_into(&mut merged, layer);
        }
    }
    merged
}

/// 把 `overlay` 合进 `base`：同 key 按字段合并，新 key 直接加入。
pub fn merge_into(base: &mut CharsetFile, overlay: CharsetFile) {
    for (key, def) in overlay.charset {
        base.charset.entry(key).or_default().merge_from(def);
    }
}

/// 渲染用户层 `charset.toml` 的完整内容（含固定头注释）。
pub fn render_user_charset(file: &CharsetFile) -> Result<String, toml::ser::Error> {
    Ok(format!("{USER_CHARSET_HEADER}{}", toml::to_string(file)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_range_forms() {
        assert_eq!(parse_range("U+4E00-U+9FFF"), Ok((0x4E00, 0x9FFF)));
        assert_eq!(parse_range("U+1F600"), Ok((0x1F600, 0x1F600)), "单点");
        assert_eq!(
            parse_range("  u+20 - u+7f  "),
            Ok((0x20, 0x7F)),
            "空白与小写"
        );
    }

    #[test]
    fn rejects_bad_ranges() {
        for bad in [
            "4E00-9FFF",     // 缺前缀
            "U+9FFF-U+4E00", // 倒序
            "U+ZZZZ",        // 非十六进制
            "U+110000",      // 超出上界
            "U+",            // 空码位
        ] {
            assert!(parse_range(bad).is_err(), "{bad} 应被拒绝");
        }
    }

    /// ⛔ 一条写错不得连累同类的其余段——否则一个笔误会静默抹掉整类字符。
    #[test]
    fn one_bad_range_does_not_drop_the_rest() {
        let raw = vec![
            "U+2600-U+26FF".to_string(),
            "垃圾".to_string(),
            "U+1F300".to_string(),
        ];
        assert_eq!(
            parse_ranges("t", &raw),
            vec![(0x2600, 0x26FF), (0x1F300, 0x1F300)]
        );
    }

    /// ★★★ 字段级合并：用户层只写 `default`，系统层的 `ranges` 必须留下。
    /// 整条覆盖会让这个类当场变成空集。
    #[test]
    fn overlay_keeps_base_fields_it_did_not_mention() {
        let mut base = CharsetFile::default();
        base.charset.insert(
            "emoji".into(),
            CharsetDef {
                name: Some("表情符号".into()),
                ranges: Some(vec!["U+1F300-U+1FAFF".into()]),
                default: Some(Commonality::Common),
                ..Default::default()
            },
        );
        let mut overlay = CharsetFile::default();
        overlay.charset.insert(
            "emoji".into(),
            CharsetDef {
                default: Some(Commonality::Rare),
                ..Default::default()
            },
        );

        merge_into(&mut base, overlay);
        let got = &base.charset["emoji"];
        assert_eq!(got.default, Some(Commonality::Rare), "改过的字段生效");
        assert_eq!(
            got.ranges.as_deref(),
            Some(&["U+1F300-U+1FAFF".to_string()][..]),
            "★ 没提到的 ranges 必须留着——丢了这个类就成空集"
        );
        assert_eq!(got.name.as_deref(), Some("表情符号"));
    }

    #[test]
    fn overlay_adds_new_keys() {
        let mut base = CharsetFile::default();
        let mut overlay = CharsetFile::default();
        overlay.charset.insert(
            "mine".into(),
            CharsetDef {
                ranges: Some(vec!["U+E000-U+E0FF".into()]),
                ..Default::default()
            },
        );
        merge_into(&mut base, overlay);
        assert!(base.charset.contains_key("mine"));
    }

    /// 合并宏漏写一个字段是静默的，用一条「全字段都非 None 的 overlay 应整体生效」
    /// 把它变成会失败的测试。新增字段时若忘了加进 `take!`，这里就会红。
    #[test]
    fn merge_covers_every_field() {
        let full = CharsetDef {
            name: Some("n".into()),
            ranges: Some(vec!["U+1-U+2".into()]),
            file: Some("f.txt".into()),
            scope: Some(ScopeKind::Han),
            add: Some(vec!["a".into()]),
            remove: Some(vec!["r".into()]),
            default: Some(Commonality::Rare),
            outside: Some(Commonality::Common),
            order: Some(7),
            no_freq: Some(true),
            in_rare: Some(true),
            enabled: Some(false),
        };
        let mut base = CharsetDef::default();
        base.merge_from(full.clone());
        assert_eq!(base, full, "有字段没被 merge_from 覆盖到");
    }

    /// 用户层是整份重写的，写回再读必须等价——否则设置页每存一次就丢一点东西。
    #[test]
    fn user_layer_round_trips() {
        let mut f = CharsetFile::default();
        f.charset.insert(
            "emoji".into(),
            CharsetDef {
                default: Some(Commonality::Rare),
                order: Some(10),
                no_freq: Some(true),
                ..Default::default()
            },
        );
        let text = render_user_charset(&f).expect("渲染");
        assert!(text.starts_with('#'), "固定头注释在最前");
        let back: CharsetFile = toml::from_str(&text).expect("回读");
        assert_eq!(back.charset["emoji"].default, Some(Commonality::Rare));
        assert_eq!(back.charset["emoji"].no_freq, Some(true));
        assert_eq!(
            back.charset["emoji"].ranges, None,
            "用户层不该出现 ranges——出现了就说明 GUI 把出厂值固化了"
        );
    }

    /// `skip_serializing_if` 必须把没表态的字段整个省掉，否则用户层会铺满
    /// `name = ""` 之类的空壳，下次合并时反而覆盖掉系统层。
    #[test]
    fn unset_fields_are_not_serialized() {
        let mut f = CharsetFile::default();
        f.charset.insert(
            "x".into(),
            CharsetDef {
                order: Some(1),
                ..Default::default()
            },
        );
        let text = toml::to_string(&f).expect("序列化");
        assert!(text.contains("order = 1"));
        for absent in ["name", "ranges", "default", "enabled"] {
            assert!(!text.contains(absent), "{absent} 不该出现在输出里：{text}");
        }
    }

    #[test]
    fn parses_a_realistic_file() {
        let text = r#"
[charset.emoji]
default = "rare"
order = 10
no_freq = true

[charset.common_han]
scope = "Han"
file = "common_chars.txt"
default = "common"
outside = "rare"

[charset.my_symbols]
name = "我不要的符号"
ranges = ['U+2600-U+26FF', 'U+2700-U+27BF']
add = ["★"]
remove = ["☀"]
default = "rare"
order = 5
"#;
        let f: CharsetFile = toml::from_str(text).expect("解析");
        assert_eq!(f.charset.len(), 3);
        assert_eq!(f.charset["emoji"].no_freq, Some(true));
        assert_eq!(f.charset["common_han"].scope, Some(ScopeKind::Han));
        assert_eq!(f.charset["common_han"].outside, Some(Commonality::Rare));
        let mine = &f.charset["my_symbols"];
        assert_eq!(mine.order_or_default(), 5);
        assert!(mine.is_enabled(), "没写 enabled 即为开");
        assert_eq!(
            parse_ranges("my_symbols", mine.ranges.as_deref().unwrap()),
            vec![(0x2600, 0x26FF), (0x2700, 0x27BF)]
        );
    }

    /// 拼错字段名要报错而不是被静默吞掉——`deny_unknown_fields` 的存在理由。
    /// 少了它，用户写 `defualt = "rare"` 会得到一个「配了没反应」且毫无线索的类。
    #[test]
    fn typo_in_field_name_is_rejected() {
        let text = "[charset.x]\ndefualt = \"rare\"\n";
        assert!(toml::from_str::<CharsetFile>(text).is_err());
    }
}
