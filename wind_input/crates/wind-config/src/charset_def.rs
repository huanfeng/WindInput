//! 字符类定义（`charsets/*.yaml`）的解析与多层合并。
//!
//! 设计与取舍见 `docs/design/charset-classification.md`。本模块只把配置读成结构，不做判定；
//! 判定结构在 `wind-candidate` 的 `charset_registry` 编译。
//!
//! 别和 [`crate::code_charset`] 混淆：那个是码元字符集（按键算不算输入码），本模块是字符类
//! （字符属于哪一类），两者无关。
//!
//! # 文件格式：meta 头 + 列表体，与 `.dict.yaml` 同构
//!
//! ```yaml
//! ---
//! key: my_symbols          # 唯一标识；覆盖按它匹配，不是按文件名
//! name: 我不要的符号
//! order: 5
//! default: rare
//! ...                      # 头体分隔符（与 librime .dict.yaml 一致）
//! ★
//! ⌘
//! -☯                       # `-` 前缀 = 从本类移除
//! ```
//!
//! 三层：`data/charsets/`（出厂）→ `data_custom/charsets/`（定制）→ 用户层在 redb
//! （`wind_store::charsets`，一个类一条，value 是本格式文本），靠后者覆盖靠前者。前两层是目录，
//! 文件名任意。用户层不用目录，因为程序和人不能写同一个文件：人要改走外部编辑，
//! [`render_edit_view`] 导出完整视图，改完用 [`diff_against_factory`] 折回稀疏 diff 存库。
//!
//! # 覆盖是「字段级 + 列表增量」，不是整文件替换
//!
//! 整文件覆盖会让只想改一个字段的用户复制整份字表，从此出厂字表更新对他静默失效。故 meta 头
//! 逐字段合并，列表体默认追加、`-` 前缀删除，只有显式 `replace: true` 才整份替换。用户层通常
//! 只有几行：
//!
//! ```yaml
//! ---
//! key: emoji
//! default: common
//! ...
//! -☯
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tracing::warn;

/// 字符类定义所在的目录名（三层同名）。
pub const CHARSETS_DIR_NAME: &str = "charsets";

/// 只扫这个扩展名。目录里难免有 `.bak` / `README.md`，静默忽略它们比报错好。
const CHARSET_EXT: &str = "yaml";

/// meta 头与列表体的分隔符，与 librime `.dict.yaml` 一致。
const HEAD_BODY_SEP: &str = "...";

/// 列表体里表示「移除」的行前缀。
const REMOVE_PREFIX: char = '-';

/// 常用性。三态由 `Option<Commonality>` 表达：`None` = 不表态，不参与仲裁。
///
/// 「不表态」必须可表达：内置区块类只为给类型列打标签，强迫它们表态会改变现有行为。
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

/// 类的作用域来源。
///
/// 值域是闭集，不接受用户写的码位段：作用域背后是判定域（`is_han ∪ is_pua`），漏一段就是
/// 那批字恒判常用、过滤静默失效，其完整性必须由代码保证。用户能自定义的只有 `ranges` 与列表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    /// 汉字 ∪ 私用区——即现有的 `is_common_scope`，默认字表的管辖域。
    Han,
    /// 私用区。
    Pua,
}

/// 一个字符类的 meta 头。
///
/// 除 `key` 外所有字段都是 `Option`，这是字段级合并的前提：`None` = 本层没说话，沿用下层。
/// 改成非 Option + `#[serde(default)]` 会让「没写」把下层的值覆盖成默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharsetDef {
    /// 唯一标识。覆盖按它匹配，不是按文件名。
    pub key: String,

    /// 显示名。缺省 = `key`（内置类的 key 就是中文名，故通常不必写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// 码位段，如 `['U+2600-U+26FF']`。语法见 [`parse_range`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<String>>,

    /// 成员来自外部字表文件（相对 `schemas/`，走 `resolve_schema_resource` 的整体替换语义），
    /// 与内嵌列表取并集。
    ///
    /// 出厂不用它，只给用户自己的类引用大字表。别拿它做出厂数据的落点：两个数据源并存，
    /// 用户改了一个，另一个仍在生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// 作用域：本类「管得着」谁。配了它，`outside` 才有意义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeKind>,

    /// 成员的常用性默认判定。`None` = 不表态，不参与仲裁。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Commonality>,

    /// 作用域内、成员外的常用性判定。只在配了 `scope` 时有意义。
    ///
    /// 它承载「生僻字是补集」：`common_han` 是白名单，「是汉字却不在名单里 ⇒ 生僻」用成员关系
    /// 表达不了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside: Option<Commonality>,

    /// 仲裁顺序，小的优先。缺省见 [`DEFAULT_ORDER`]。用显式数字而非扫描顺序：后者在稀疏覆盖里
    /// 表达不了，`read_dir` 也不保证顺序。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,

    /// 本类成员不参与词频学习与重排。
    ///
    /// 并集语义（任一命中的类为真即真），与 `default` 的仲裁语义不同（见设计文档 §2.2）：
    /// 多免一个字符是安全方向。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_freq: Option<bool>,

    /// 本类成员额外纳入生僻字模式。并集语义，同 `no_freq`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_rare: Option<bool>,

    /// 关掉这个类。
    ///
    /// 多层合并表达不了删除，去掉一个内置类只能靠它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// 本层的列表体整份替换下层，而不是增量叠加。
    ///
    /// 写了它就接管整份字表、不再跟随出厂更新，所以必须显式声明，不能从「写了列表」推断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<bool>,
}

/// `order` 缺省值。取中间值，让用户既能插到内置类前面也能插到后面。
///
/// 汉字上缺省值轮不到：出厂 `common_han` 是 10 且对整个汉字域表态，故新建类由
/// `handle_charset::TEMPLATE_ORDER` 显式给更小的值。别为此改这个常量，否则已有自建类的
/// 仲裁结果会静默翻转。
pub const DEFAULT_ORDER: i32 = 100;

impl CharsetDef {
    /// 字段级合并：`overlay` 里 `Some` 的字段覆盖 `self`，`None` 的保留。`key` 不动。
    fn merge_from(&mut self, overlay: CharsetDef) {
        // 逐字段 `if ... .is_some()`：新增字段时漏写一行是静默的，但形态一致、一眼能
        // 看出少了哪个——由 `merge_covers_every_field` 钉住。
        macro_rules! take {
            ($($f:ident),+ $(,)?) => { $( if overlay.$f.is_some() { self.$f = overlay.$f; } )+ };
        }
        take!(
            name, ranges, file, scope, default, outside, order, no_freq, in_rare, enabled, replace
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

    /// 显示名，缺省回落 `key`。
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.key)
    }
}

/// 一个 `.yaml` 文件解析出的内容：meta 头 + 列表体。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharsetDoc {
    /// meta 头。
    pub def: CharsetDef,
    /// 列表体里**不带**前缀的行——要加入本类的字素簇。
    pub added: Vec<String>,
    /// 列表体里 `-` 前缀的行——要从本类移除的字素簇。
    pub removed: Vec<String>,
}

/// 多层合并后的一个类。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergedClass {
    /// 字段级合并后的 meta。
    pub def: CharsetDef,
    /// 合并后的成员（内嵌列表部分；`file` 指向的外部字表由消费方另行加载）。
    pub members: BTreeSet<String>,
    /// 成员的配置顺序（首次出现的先后），与 [`Self::members`] 同集合。设置页按它列全表并分页；
    /// `BTreeSet` 是码位序，会丢掉字表的分级顺序。
    pub member_order: Vec<String>,
    /// 被移除的字素簇。单独留一份交给判定层当排除集：`file` 外部字表在更后面加载，
    /// 移除也要作用到它。
    pub removed: BTreeSet<String>,
}

/// 把列表体的一行拆成 `(要不要删, 成员)` 序列。推荐一行一个成员，多码位簇整体算一个。
///
/// 一行多个字时按空白分词 → 字素簇两级切开（调用方 warn），不能静默当成一个成员，否则它
/// 永远匹配不上。切分口径须与 [`wind_candidate::split_markable_clusters`] 一致（`graphemes(true)`）。
/// 空白还用来隔开相邻区域指示符：`🇨🇳` 连写是一面旗，两个字母要写 `🇨 🇳`。
///
/// `-` 前缀作用于整个词：`-的一是` 删三个。
fn split_members(line: &str) -> impl Iterator<Item = (bool, String)> + '_ {
    use unicode_segmentation::UnicodeSegmentation;
    line.split_whitespace().flat_map(|word| {
        let (remove, body) = match word.strip_prefix(REMOVE_PREFIX) {
            Some(rest) if !rest.is_empty() => (true, rest),
            _ => (false, word),
        };
        body.graphemes(true).map(move |g| (remove, g.to_string()))
    })
}

/// 解析一份文件，可能含多个类。按 meta 头第一个非注释非空行分辨形态：
///
/// | 形态 | meta 头 | 列表体 | 用途 |
/// |---|---|---|---|
/// | 单类 | 一个映射（`key: xxx`） | 支持 | 带成员名单的类（emoji、常用汉字） |
/// | 多类 | 一个数组（`- key: xxx`） | 不支持 | 一批纯区间类（Unicode 区块） |
///
/// 多类文件不带列表体（归哪个类说不清）；要给其中某类加成员，另起同 `key` 的单类文件
/// （见设计文档 §3.5）。多类形态让几十个内置区块既可配置又不淹没 `charsets/` 目录。
pub fn parse_docs(text: &str) -> anyhow::Result<Vec<CharsetDoc>> {
    let (head, body) = split_head_body(text);
    // 判据要跳过注释与空行：出厂多类文件以说明注释开头，否则会被误判成单类而整份解析失败。
    let first = head
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'));
    if first.is_some_and(|l| l.starts_with('-')) {
        anyhow::ensure!(
            body.trim().is_empty(),
            "多类文件（meta 头是数组）不支持列表体——一份列表归哪个类说不清；\
             要给其中某个类加成员，另起一个同 key 的单类文件"
        );
        let defs: Vec<CharsetDef> =
            serde_yaml::from_str(&head).map_err(|e| anyhow::anyhow!("meta 头解析失败：{e}"))?;
        return defs
            .into_iter()
            .map(|def| {
                anyhow::ensure!(!def.key.trim().is_empty(), "数组里有一项缺 key");
                Ok(CharsetDoc {
                    def,
                    added: Vec::new(),
                    removed: Vec::new(),
                })
            })
            .collect();
    }
    Ok(vec![parse_doc(text)?])
}

/// 按 `...` 把文件切成 meta 头与列表体（`---` 行跳过）。
fn split_head_body(text: &str) -> (String, String) {
    // 行尾规整放在两个 pub 入口的共同下游，全链路只做一次。不归一化的话孤立 \r 会让整份
    // 文件算成一行、找不到 `...` 分隔，一个类都解析不出来。
    let normalized = wind_utils::text::normalize_input(text);
    let text = normalized.as_ref();
    let (mut head, mut body) = (String::new(), String::new());
    let mut in_head = true;
    for line in text.lines() {
        if in_head {
            if line.trim() == HEAD_BODY_SEP {
                in_head = false;
                continue;
            }
            if line.trim() == "---" {
                continue;
            }
            head.push_str(line);
            head.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    (head, body)
}

/// 解析一个单类 `.yaml` 文件的内容。
///
/// 头体以 `...` 分隔；没有分隔符则整份都是 meta 头（只调字段的覆盖文件，用户层最常见）。
/// `---` 起始行可有可无，与 `.dict.yaml` 惯例一致。
pub fn parse_doc(text: &str) -> anyhow::Result<CharsetDoc> {
    let (head, body_text) = split_head_body(text);
    let body: Vec<&str> = body_text.lines().collect();

    let def: CharsetDef =
        serde_yaml::from_str(&head).map_err(|e| anyhow::anyhow!("meta 头解析失败：{e}"))?;
    anyhow::ensure!(!def.key.trim().is_empty(), "meta 头缺少 key");

    let (mut added, mut removed) = (Vec::new(), Vec::new());
    for (i, line) in body.iter().enumerate() {
        let line_no = i + 1;
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let items: Vec<(bool, String)> = split_members(t).collect();
        // 一行多个成员：已切开，但提醒一句（见 `split_members`）。
        if items.len() > 1 {
            warn!(
                "字符类「{}」列表体第 {} 行有 {} 个成员：{}。一行写一个更好读，                 也与其余字表一致；本行已按字素簇切开",
                def.key,
                line_no,
                items.len(),
                t
            );
        }
        for (remove, m) in items {
            if remove {
                removed.push(m);
            } else {
                added.push(m);
            }
        }
    }
    Ok(CharsetDoc {
        def,
        added,
        removed,
    })
}

/// 扫描一层目录，返回该层的全部文档。
///
/// 按文件名排序后处理：`read_dir` 不保证顺序，同层两个文件撞 key 时结果会随机。
/// 单个文件解析失败只 warn 并跳过，不让整层失效。
pub fn load_layer(dir: &Path) -> Vec<CharsetDoc> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case(CHARSET_EXT))
        })
        .collect();
    paths.sort();

    let mut out = Vec::new();
    let mut seen: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else {
            warn!("读取字符类文件失败：{}", p.display());
            continue;
        };
        // 一份文件可能出多个类（meta 头是数组时），见 `parse_docs`。
        match parse_docs(&text) {
            Ok(docs) => {
                for doc in docs {
                    // 同层 key 撞车要报出来，否则用户改了一个文件却看不到效果。
                    if let Some(prev) = seen.get(&doc.def.key) {
                        warn!(
                            "字符类 key「{}」在同一层重复：{} 与 {}，两者将按文件名序叠加",
                            doc.def.key,
                            prev.display(),
                            p.display()
                        );
                    } else {
                        seen.insert(doc.def.key.clone(), p.clone());
                    }
                    out.push(doc);
                }
            }
            Err(e) => warn!("解析 {} 失败，已跳过：{e}", p.display()),
        }
    }
    out
}

/// 出厂两层（`data/charsets` → `data_custom/charsets`）的合并结果。
///
/// 本模块里「出厂」恒指这两层之和，编辑视图与 diff 都以它为基准；把定制层算进用户层，
/// 「恢复默认」会把定制层一并丢掉。
pub fn load_factory(
    data_dir: Option<&Path>,
    custom_dir: Option<&Path>,
) -> BTreeMap<String, MergedClass> {
    let mut merged: BTreeMap<String, MergedClass> = BTreeMap::new();
    for dir in [data_dir, custom_dir].into_iter().flatten() {
        for doc in load_layer(&dir.join(CHARSETS_DIR_NAME)) {
            apply_doc(&mut merged, doc);
        }
    }
    merged
}

/// 三层加载：出厂两层目录 + 用户层 docs（来自库，见 [`parse_stored_docs`]）。
pub fn load_layered(
    data_dir: Option<&Path>,
    custom_dir: Option<&Path>,
    user_docs: Vec<CharsetDoc>,
) -> BTreeMap<String, MergedClass> {
    let mut merged = load_factory(data_dir, custom_dir);
    for doc in user_docs {
        apply_doc(&mut merged, doc);
    }
    merged
}

/// 把库里的 `(key, 文本)` 解析成 docs。坏的一条 warn 并跳过，不影响其余。
///
/// 库键与文本里的 `key` 不一致则跳过：只可能是手工改库或程序 bug，按哪个算都是猜。
pub fn parse_stored_docs<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<CharsetDoc> {
    let mut out = Vec::new();
    for (key, text) in rows {
        match parse_doc(text) {
            Ok(doc) if doc.def.key == key => out.push(doc),
            Ok(doc) => warn!(
                "字符类用户层：库键「{key}」与文本里的 key「{}」不一致，已跳过",
                doc.def.key
            ),
            Err(e) => warn!("字符类用户层「{key}」解析失败，已跳过：{e}"),
        }
    }
    out
}

/// 把一份文档叠加到合并结果上。
pub fn apply_doc(merged: &mut BTreeMap<String, MergedClass>, doc: CharsetDoc) {
    let entry = merged
        .entry(doc.def.key.clone())
        .or_insert_with(|| MergedClass {
            def: CharsetDef {
                key: doc.def.key.clone(),
                ..Default::default()
            },
            ..Default::default()
        });

    // `replace` 只影响列表，不影响 meta：接管字表与接管字段是两件事。
    if doc.def.replace == Some(true) {
        entry.members.clear();
        entry.member_order.clear();
        entry.removed.clear();
    }
    entry.def.merge_from(doc.def);

    for m in doc.added {
        entry.removed.remove(&m);
        // 只在**首次**出现时记顺序：重复的成员不该在列表里出现两次。
        if entry.members.insert(m.clone()) {
            entry.member_order.push(m);
        }
    }
    for r in doc.removed {
        entry.members.remove(&r);
        entry.member_order.retain(|x| x != &r);
        entry.removed.insert(r);
    }
}

/// 一个码位段（闭区间）。
pub type CodeRange = (u32, u32);

/// 存进库 / 进备份包的文本首行说明（设计 `user-dict-libraries-and-external-edit.md` §5；
/// 其余数据文件的文案在 `wind_userdata::text::banner`）。
pub const STORED_DOC_BANNER: &str = "# 清风输入法 · 字符类的个人调整。可手动编辑。";

/// 文本头注释。用户在备份包里会直接看到，要说明「只含改过的字段」，否则会奇怪 emoji 类的
/// 备份里为什么没有 emoji。
const STORED_DOC_HEADER: &str = "\
# 只含你改过的字段与增删的成员；没写的沿用下层（定制层 → 出厂层）。
# 列表体在 `...` 之后：裸行 = 加进本类，`-` 开头 = 从本类移除。
";

/// 把一份用户层调整渲染成 yaml 文本（存库 / 进备份包）。
///
/// 只写 `Some` 的字段：写全量会把当前出厂值固化进用户层（见设计文档 §4.1）。
pub fn render_doc(doc: &CharsetDoc) -> anyhow::Result<String> {
    anyhow::ensure!(
        is_valid_key(&doc.def.key),
        "字符分类的 key「{}」不合法",
        doc.def.key
    );
    let head = serde_yaml::to_string(&doc.def)?;
    let mut text = String::with_capacity(head.len() + 256);
    text.push_str(STORED_DOC_BANNER);
    text.push('\n');
    text.push_str(STORED_DOC_HEADER);
    text.push_str("---\n");
    text.push_str(&head);
    if !doc.added.is_empty() || !doc.removed.is_empty() {
        text.push_str(HEAD_BODY_SEP);
        text.push('\n');
        for a in &doc.added {
            text.push_str(a);
            text.push('\n');
        }
        for r in &doc.removed {
            text.push(REMOVE_PREFIX);
            text.push_str(r);
            text.push('\n');
        }
    }
    Ok(text)
}

/// key 能不能当标识用（也是备份包里的文件名）。
///
/// 校验而不是安全化：把非法字符换成 `_` 会让两个 key 落到同一个文件上、后写的静默覆盖先写的。
///
/// 拒绝路径分隔符（`/` 与 `\` 都拦）、`.`、`:`（Windows `C:name` 数据流语法）、控制字符、首尾
/// 空白。别用 `std::path::is_separator`：它随运行平台变，而备份包跨平台流转，判据须与平台无关。
pub fn is_valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 64 || key.trim() != key {
        return false;
    }
    // Windows 保留的设备名当不了文件名（`con.yaml` 在部分系统上写不进去）。
    const RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];
    let lower = key.to_ascii_lowercase();
    let b = lower.as_bytes();
    if RESERVED.contains(&lower.as_str())
        || ((lower.starts_with("com") || lower.starts_with("lpt"))
            && b.len() == 4
            && (b'1'..=b'9').contains(&b[3]))
    {
        return false;
    }
    !key.chars().any(|c| {
        matches!(
            c,
            '/' | '\\' | '.' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        ) || c.is_control()
    })
}

/// 这份调整是不是空壳（除 `key` 外什么都没说）。空壳须从库里删掉，否则「恢复默认」后
/// 列表仍标着「已调整」。
pub fn is_empty_override(doc: &CharsetDoc) -> bool {
    let d = &doc.def;
    doc.added.is_empty()
        && doc.removed.is_empty()
        && d.name.is_none()
        && d.ranges.is_none()
        && d.file.is_none()
        && d.scope.is_none()
        && d.default.is_none()
        && d.outside.is_none()
        && d.order.is_none()
        && d.no_freq.is_none()
        && d.in_rare.is_none()
        && d.enabled.is_none()
        && d.replace.is_none()
}

/// 解析一条码位段：`U+4E00-U+9FFF`（闭区间）或 `U+4E00`（单点）。
///
/// 用 `U+XXXX` 而不是 `\u` 转义：后者长度定死 4 位、写不满就解析报错，且这是 Unicode 资料
/// 里的标准写法。
pub fn parse_range(s: &str) -> Result<CodeRange, String> {
    let t = s.trim();
    let (lo_s, hi_s) = match t.split_once('-') {
        Some((a, b)) => (a, b),
        None => (t, t),
    };
    let lo = parse_code_point(lo_s)?;
    let hi = parse_code_point(hi_s)?;
    if lo > hi {
        return Err(format!("段起点大于终点：{t}"));
    }
    Ok((lo, hi))
}

/// 解析单个码位 `U+XXXX`。前缀大小写不敏感；允许 1~6 位十六进制。
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
    // 上界按码位空间判而不是 `char::from_u32`：代理区作为区间端点是合理的（`U+0000-U+FFFF`）。
    if v > 0x10FFFF {
        return Err(format!("码位超出 Unicode 上界 10FFFF：{t}"));
    }
    Ok(v)
}

/// 解析一组码位段，逐条容错：返回成功的段，失败的逐条 warn。不得因一条写错丢掉整个类。
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

#[cfg(test)]
#[path = "charset_def_tests.rs"]
mod tests;
