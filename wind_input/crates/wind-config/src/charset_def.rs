//! 字符类定义（`charsets/*.yaml`）的解析与多层合并。
//!
//! 设计与全部取舍见 `docs/design/charset-classification.md`。本模块只负责
//! **把配置读成结构**，不做判定——判定结构的编译在 `wind-candidate` 的
//! `charset_registry`，由 coordinator 把这里的产物转成 `ClassSpec` 喂过去。
//!
//! # ⛔ 不要和 [`crate::code_charset`] 混淆
//!
//! 那个是**码元**字符集（`[engine.codetable].input_chars`，回答「这一次按键算不算输入码」，
//! 值域是 ASCII 按键）。本模块是**字符类**（回答「这个字符属于哪一类」，值域是全 Unicode）。
//! 两者除了名字都带 charset 之外没有任何关系，故本模块的类型一律叫 `Charset*`。
//!
//! # 文件格式：meta 头 + 列表体，与 `.dict.yaml` 同构
//!
//! ```yaml
//! ---
//! key: my_symbols          # 唯一标识；覆盖按它匹配，**不是**按文件名
//! name: 我不要的符号
//! order: 5
//! default: rare
//! ...                      # 头体分隔符（与 librime .dict.yaml 一致）
//! ★
//! ⌘
//! -☯                       # `-` 前缀 = 从本类移除
//! ```
//!
//! 三层：`data/charsets/`（出厂）→ `data_custom/charsets/`（定制）→ **用户层在 redb**
//! （`wind_store::charsets`，一个类一条、value 就是本格式的文本）。靠后者覆盖靠前者。
//! 前两层是目录，**文件名任意**，一个类一个文件，新增类只需丢一个文件进去。
//!
//! 用户层不是目录，是因为**程序和人不能写同一个文件**：设置页改一个开关要重写文件，
//! 用户此时在编辑器里开着它，谁后保存谁赢。于是程序只写库；人要改就「外部编辑」——
//! [`render_edit_view`] 导出一份**完整视图**到临时文件，改完用 [`diff_against_factory`]
//! 折回稀疏 diff 存库。备份包里的 `charsets/<key>.yaml` 是库文本原样，同一格式。
//!
//! # ★★★ 覆盖是「字段级 + 列表增量」，不是整文件替换
//!
//! 这是本模块最容易做错、错了又最难发现的一处。用户想把 emoji 的 `default` 改成
//! `common`，若按整文件覆盖，他必须把 1427 行字表整份复制进用户目录再改一个字段 ⇒
//! **Unicode 出新版时出厂字表更新对这个用户永久失效**，而界面上看不出任何异常。
//!
//! 本仓已在 `key_actions` 物化、扩展词库 override、`compat.toml` 空壳规则上踩过三次
//! 同一个坑。共同判据：**稀疏 diff 的合并算子里，「删除」和「顺序」有没有表达**。
//!
//! ⇒ 于是：meta 头逐字段合并（未写的字段沿用下层），列表体默认**追加**、`-` 前缀
//! **删除**、只有显式 `replace: true` 才整份替换。用户层通常只有几行：
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

/// 常用性。三态由 `Option<Commonality>` 表达：`None` = **不表态**，不参与仲裁。
///
/// ★ 「不表态」必须是可表达的：那些内置区块类存在的理由只是给类型列一个标签，
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
/// ⇒ 判定域的完整性必须由代码保证。用户能自定义的只有 `ranges` 与列表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    /// 汉字 ∪ 私用区——即现有的 `is_common_scope`，默认字表的管辖域。
    Han,
    /// 私用区。
    Pua,
}

/// 一个字符类的 **meta 头**。
///
/// **除 `key` 外所有字段都是 `Option`**，这是字段级合并的前提：`None` = 本层没说话，
/// 沿用下层。若某字段用非 Option 类型 + `#[serde(default)]`，「用户没写」与「用户写了
/// 默认值」在合并时无法区分，前者会把下层的值覆盖成默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharsetDef {
    /// 唯一标识。**覆盖按它匹配，不是按文件名**——所以用户可以把文件叫
    /// 「我的emoji调整.yaml」，一样能覆盖出厂的 emoji 类。
    pub key: String,

    /// 显示名。缺省 = `key`（内置类的 key 就是中文名，故通常不必写）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// 码位段，如 `['U+2600-U+26FF']`。语法见 [`parse_range`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<String>>,

    /// 成员来自**外部字表文件**（相对 `schemas/`，走 `resolve_schema_resource` 的整体
    /// 替换语义）。与内嵌列表并存，两者取并集。
    ///
    /// ⚠️ **出厂不再用它**：常用字表已搬进 `charsets/common_han.yaml` 的列表体
    /// （2026-09-04，用户拍板）。留着本字段是给「自己的类要引用一个大字表」那种用法。
    ///
    /// ⛔ 别拿它做出厂数据的落点：两个文件就是两个数据源，用户改了其中一个，
    /// 另一个还在生效——而他看不出哪一半是哪一半。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// 作用域：本类「管得着」谁。配了它，`outside` 才有意义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeKind>,

    /// 成员的常用性默认判定。`None` = 不表态，不参与仲裁。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Commonality>,

    /// **作用域内、成员外**的常用性判定。只在配了 `scope` 时有意义。
    ///
    /// ★★ 这个字段承载的是「生僻字是补集」那件事：`common_han` 的成员是 8104 字的
    /// 白名单，而「是汉字、却不在名单里 ⇒ 生僻」用成员关系表达不了——那个字压根不是
    /// 成员。缺了它，「换一本常用字表」这个需求做不到。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside: Option<Commonality>,

    /// 仲裁顺序，**小的优先**。缺省见 [`DEFAULT_ORDER`]。
    ///
    /// ★ 用显式数字而不是文件的扫描顺序：目录扫描序在**稀疏覆盖**里表达不了，而且
    /// `read_dir` 本身不保证顺序。改成一个数就可以只写这个数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,

    /// 本类成员不参与词频学习与重排。
    ///
    /// ⚠️ **并集语义**（任一命中的类为真即真），与 `default` 的仲裁语义不同——见设计
    /// 文档 §2.2。多免一个字符是安全方向；改成仲裁会让一个本该免的字符不免。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_freq: Option<bool>,

    /// 本类成员额外纳入生僻字模式。并集语义，同 `no_freq`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_rare: Option<bool>,

    /// 关掉这个类。
    ///
    /// ★ 存在的理由：多层合并只能新增/覆盖，**表达不了删除**。用户想去掉一个内置类，
    /// 没有这个字段就只能靠偏方（把 ranges 覆盖成空），而那会连带改变类型列的显示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// 本层的列表体**整份替换**下层，而不是增量叠加。
    ///
    /// ⚠️ 用户层写了它就等于接管整份字表，**从此不再跟随出厂更新**——这正是「我只想
    /// 改一个字段」与「我要接管整个定义」的分界，必须显式声明，不能靠「用户写了列表」
    /// 隐式推断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<bool>,
}

/// `order` 缺省值。取中间值而不是 0，让用户既能插到内置类前面也能插到后面，
/// 不必给所有内置类重新编号。
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
    /// 成员的**配置顺序**（首次出现的先后），与 [`Self::members`] 同集合。
    ///
    /// ⚠️ 顺序有意义：设置页按它列全表并分页。`BTreeSet` 给的是**码位序**——拿它去列
    /// 常用字表，「一级 3500 → 二级 3000 → 三级」的分级顺序就没了，用户翻到第 2 页
    /// 看到的东西也会随字表内容变动而整体漂移。
    pub member_order: Vec<String>,
    /// 被移除的字素簇。
    ///
    /// ★ 合并时不能直接从 `members` 里扣掉就算完：`file` 指向的外部字表是在更后面
    /// 加载的，用户写的 `-☯` 必须能作用到它上面。故单独留一份，交给判定层当排除集。
    pub removed: BTreeSet<String>,
}

/// 解析一个 `.yaml` 文件的内容。
///
/// 头体以 `...` 分隔；没有分隔符则**整份都是 meta 头**（只调字段、不动列表的覆盖文件
/// 就长这样，是用户层最常见的形态）。`---` 起始行可有可无，与 `.dict.yaml` 惯例一致。
/// 把列表体的一行拆成 `(要不要删, 成员)` 序列。
///
/// # 一行一个成员，是**推荐也是自带文件的**写法
///
/// | 写法 | 得到 |
/// |---|---|
/// | `我` | 一个成员 |
/// | `⚽️` `👨‍👩‍👧` `1️⃣` | 各**一个**成员（多码位簇整体，不会被拆散） |
///
/// # 一行写了多个字怎么办：切开，并 warn
///
/// ⚠️ **不能静默当成一个成员**——那样它永远匹配不上任何候选，用户看到的是「我明明加了
/// 这些字，一个都没生效」，而文件、日志、界面三处都不报错。旧的 `common_chars.txt`
/// 读端就是逐 `char` 收的，用户照那个习惯写完全可能。
///
/// ⇒ 按**空白分词 → 字素簇**两级切开，并由调用方 warn 一句。切分口径与判定层的
/// [`wind_candidate::split_markable_clusters`] 一致（都是 `graphemes(true)`），
/// 不一致的后果是配置里写的成员匹配不上候选里的簇，而两边各自看都对。
///
/// ⚠️ 空白分隔在这里还有一个不可替代的用途：相邻的区域指示符**会合成国旗**，`🇨🇳`
/// 连写是一个成员（那面旗）而不是两个字母。要表达两个字母只能写 `🇨 🇳`——字素簇切分
/// 解决不了「两个成员恰好能拼成第三个」。
///
/// `-` 前缀作用于**整个词**：`-的一是` 删三个。
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

/// 解析一份文件，可能含**多个**类。
///
/// # 两种形态，按 meta 头的第一个非空字符分辨
///
/// | 形态 | meta 头 | 列表体 | 用途 |
/// |---|---|---|---|
/// | 单类 | 一个映射（`key: xxx`） | ✅ 支持 | 带成员名单的类（emoji、常用汉字） |
/// | 多类 | 一个**数组**（`- key: xxx`） | ⛔ 不支持 | 一批纯区间类（50 个 Unicode 区块） |
///
/// ⚠️ 多类文件不带列表体：一份列表归哪个类说不清。要给其中某个类加成员，另起一个同
/// `key` 的单类文件即可（覆盖按 key 匹配，见设计文档 §3.5）。
///
/// ★ 存在的理由：50 个内置区块若一类一文件，`charsets/` 目录会被淹掉、用户看不见真正
/// 该配的那两三个；而全塞进代码里又等于「这套东西唯独这部分不可配置」——那正是本次
/// 改造要消灭的形态。
pub fn parse_docs(text: &str) -> anyhow::Result<Vec<CharsetDoc>> {
    let (head, body) = split_head_body(text);
    // ⚠️ 判据要**跳过注释与空行**：出厂的多类文件开头是一段说明注释，按「第一个字符」
    // 判会把它当成单类文件，于是整份解析失败、52 个类一个都不进 registry——而日志里
    // 只有一行「解析失败，已跳过」，看不出是判错了形态。
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
        // ⚠️ 一行多个成员：解析得了，但提醒一句。自带文件一行一个，用户照旧
        // `common_chars.txt` 的习惯连写完全可能——而静默当成一个成员的话，
        // 那一行永远匹配不上任何候选，三处都不报错。
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
/// ⚠️ **按文件名排序后处理**：`read_dir` 不保证顺序，不排序的话同层内两个文件涉及
/// 同一个 key 时行为随机、极难复现。
///
/// 单个文件解析失败只 warn 并跳过该文件——一个写坏的文件不该让整层配置失效
/// （同 `compat.toml` 的容错纪律）。
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
                    // ⚠️ 同层内 key 撞车必须报出来：静默取其一会让用户改了一个文件却
                    // 看不到效果，而两个文件看上去都是对的。
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
/// ★ 本模块里「出厂」恒指**这两层之和**：定制层是发行方替用户做的决定，对用户而言
/// 与出厂无异。编辑视图与 diff 都以它为基准——把定制层算进「用户层」的话，diff 会把
/// 定制方加的成员全记成用户新增，用户「恢复默认」就把定制层一并丢了。
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
/// ⚠️ 库键与文本里的 `key` 必须一致，否则跳过：不一致只可能是手工改库或程序 bug，
/// 按哪一个算都是猜——而猜错的表现是「改了 A 类，B 类变了」。
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

/// 存进库 / 进备份包的文本头注释。
///
/// 备份包里用户会直接看到这份文本，所以要把「只含改过的字段」这件事说出来——否则他
/// 会奇怪为什么 emoji 类的备份里一个 emoji 都没有。
const STORED_DOC_HEADER: &str = "\
# WindInput 字符类的用户层调整（设置页维护）。
# 只含你改过的字段与增删的成员；没写的沿用下层（定制层 → 出厂层）。
# 列表体在 `...` 之后：裸行 = 加进本类，`-` 开头 = 从本类移除。
";

/// 把一份用户层调整渲染成 yaml 文本（存库 / 进备份包）。
///
/// ⚠️ 只写 `Some` 的字段（`CharsetDef` 全字段 `skip_serializing_if`）。写全量会把当前
/// 的出厂值**固化**进用户层——出厂改了 `order` 或补了 `ranges`，这个用户永远拿不到。
/// 同一个坑见设计文档 §4.1。
pub fn render_doc(doc: &CharsetDoc) -> anyhow::Result<String> {
    anyhow::ensure!(
        is_valid_key(&doc.def.key),
        "字符类的 key「{}」不合法",
        doc.def.key
    );
    let head = serde_yaml::to_string(&doc.def)?;
    let mut text = String::with_capacity(head.len() + 256);
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

/// 外部编辑文件的头注释。`{name}` / `{key}` 由 [`render_edit_view`] 填。
///
/// ★ 首行「此文件不会被自动读取」是用户拍板的措辞：这份文件是**导出的副本**，改完
/// 必须回设置页导入。不说这句，用户会改完等着生效，然后判定功能坏了。
const EDIT_VIEW_HEADER: &str = "\
# ⚠️ 此文件不会被自动读取。改完保存后，回到设置页「字符集分类」用「从文件加载」导入。
#
# 这是「{name}」（key: {key}）当前生效的完整定义 = 出厂 + 你的调整。
# 直接增删 `...` 之后的字符行即可；导入时只记下与出厂的差异，出厂更新仍会跟随。
# 字段：name、default（common / rare，删掉这行 = 沿用出厂）、no_freq、in_rare、
#       order（小的优先）、enabled。
";

/// 出厂类的 `ranges` 在编辑视图里写成注释时，跟在后面的说明。
const RANGES_READONLY_NOTE: &str = "  # 出厂类的范围只读；要改范围请新建自己的类";

/// 渲染**完整视图**供外部编辑：生效字段 + 生效成员（出厂 ∪ 新增 − 移除）。
///
/// # 为什么导出完整视图而不是用户层 diff
///
/// diff 只有 `-龘` 几行，用户看不到出厂有什么，写不出正确的 `-字`。完整视图所见即所得，
/// 直接增删；回读时 [`diff_against_factory`] 与**当前**出厂比较，改一个字库里就只多一条
/// ——稀疏性不丢，且与导出时刻无关，出厂升版也不会错乱。
///
/// ⚠️ `file:` 指向的外部字表**不展开**（本层读不到它，也不该读）：那些成员既不在视图
/// 里也不在 diff 的基准里，回读不会把它们误记成「移除」。代价是用户在视图里看不到、
/// 也删不掉它们——出厂没有带 `file:` 的类，可接受。
///
/// `factory` 为 `None` 即自建类，视图就是用户层本身。
pub fn render_edit_view(
    factory: Option<&MergedClass>,
    user: &CharsetDoc,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        is_valid_key(&user.def.key),
        "字符类的 key「{}」不合法",
        user.def.key
    );
    // 生效字段 = 出厂字段被用户层逐字段覆盖。
    let mut def = factory.map(|f| f.def.clone()).unwrap_or_default();
    def.key = user.def.key.clone();
    def.merge_from(user.def.clone());
    // `replace` 是「本层接管字表」的开关，完整视图导入本来就等价于给出全部，导出它只会
    // 让用户以为要写这个字段。
    def.replace = None;

    let mut text = String::new();
    text.push_str(
        &EDIT_VIEW_HEADER
            .replace("{name}", def.display_name())
            .replace("{key}", &def.key),
    );
    text.push_str("---\n");

    // 出厂类的 ranges 只读：写成注释让用户看得见，但改了也导不进去（diff 时拒绝）。
    let readonly_ranges = factory.is_some_and(|f| f.def.ranges.is_some());
    let ranges_note = if readonly_ranges {
        def.ranges.take()
    } else {
        None
    };
    text.push_str(&serde_yaml::to_string(&def)?);
    if let Some(r) = ranges_note {
        text.push_str("# ranges: [");
        text.push_str(&r.join(", "));
        text.push(']');
        text.push_str(RANGES_READONLY_NOTE);
        text.push('\n');
    }

    // 生效成员，保出厂顺序（设置页按它分页），用户新增的排在后面。
    let removed: std::collections::HashSet<&str> =
        user.removed.iter().map(String::as_str).collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut members: Vec<&str> = Vec::new();
    let base = factory.map(|f| f.member_order.as_slice()).unwrap_or(&[]);
    for m in base.iter().chain(user.added.iter()) {
        if !removed.contains(m.as_str()) && seen.insert(m.as_str()) {
            members.push(m.as_str());
        }
    }
    text.push_str(HEAD_BODY_SEP);
    text.push('\n');
    for m in members {
        text.push_str(m);
        text.push('\n');
    }
    Ok(text)
}

/// 把外部编辑回来的**完整视图**折成用户层的稀疏 diff。
///
/// - 字段：与出厂相同的置 `None`（不固化）；不同的保留；**删掉的行 = 沿用出厂**
///   （字段级合并表达不了「把出厂的 Some 变 None」，这是已知限制，头注释里有说）。
/// - 成员：`added = 视图 − 出厂`，`removed = 出厂 − 视图`（按出厂顺序）。视图里手写的
///   `-字` 也并入 removed。
/// - 出厂类的 `ranges` 改了 ⇒ **报错**，不静默丢弃：用户花时间改了范围，静默丢掉会让
///   他以为导入坏了。
///
/// `factory` 为 `None` 即自建类：原样存，只清掉无意义的 `replace`。
pub fn diff_against_factory(
    mut parsed: CharsetDoc,
    factory: Option<&MergedClass>,
) -> anyhow::Result<CharsetDoc> {
    anyhow::ensure!(
        is_valid_key(&parsed.def.key),
        "字符类的 key「{}」不合法",
        parsed.def.key
    );
    let Some(f) = factory else {
        parsed.def.replace = None;
        return Ok(parsed);
    };

    if let Some(r) = &parsed.def.ranges
        && f.def.ranges.is_some()
        && Some(r) != f.def.ranges.as_ref()
    {
        anyhow::bail!(
            "「{}」是出厂类，范围只读；要用别的范围请新建自己的类",
            f.def.display_name()
        );
    }

    let mut def = CharsetDef {
        key: f.def.key.clone(),
        ..Default::default()
    };
    // 逐字段：与出厂相同 ⇒ None。与 `merge_from` 同款宏，漏一个字段一眼能看出。
    macro_rules! keep_if_differs {
        ($($fld:ident),+ $(,)?) => { $(
            if parsed.def.$fld.is_some() && parsed.def.$fld != f.def.$fld {
                def.$fld = parsed.def.$fld.take();
            }
        )+ };
    }
    keep_if_differs!(
        name, file, scope, default, outside, order, no_freq, in_rare, enabled
    );
    // ranges 到这里只可能等于出厂或为 None，两者都不该进用户层。replace 同理。

    let view: BTreeSet<&str> = parsed.added.iter().map(String::as_str).collect();
    let added: Vec<String> = parsed
        .added
        .iter()
        .filter(|m| !f.members.contains(m.as_str()))
        .cloned()
        .collect();
    let mut removed: Vec<String> = f
        .member_order
        .iter()
        .filter(|m| !view.contains(m.as_str()))
        .cloned()
        .collect();
    for r in parsed.removed {
        if !removed.contains(&r) {
            removed.push(r);
        }
    }
    Ok(CharsetDoc {
        def,
        added,
        removed,
    })
}

/// key 能不能当标识用（也是备份包里的文件名）。
///
/// # ⚠️ 为什么要校验而不是「安全化」
///
/// 安全化（把非法字符换成 `_`）会让两个不同的 key 落到同一个文件上，后写的**静默**覆盖
/// 先写的——用户看到的是「改了 A 类，B 类的设置没了」。校验则在入口就拒绝，代价是用户
/// 得换个名字，而那正是他能理解并处理的。
///
/// ⛔ 明确拒绝的：路径分隔符、`.`（含 `..`）、`:`（Windows 的 `C:name` 数据流语法，
/// 见仓内防穿越守卫的同款约定）、控制字符、首尾空白。
pub fn is_valid_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 64 || key.trim() != key {
        return false;
    }
    !key.chars().any(|c| {
        std::path::is_separator(c)
            || c.is_control()
            || matches!(c, '.' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    })
}

/// 这份调整是不是**空壳**（除 `key` 外什么都没说）。
///
/// ★ 空壳必须从库里删掉而不是留一条只有 key 的记录：留着的话，「恢复默认」之后列表里
/// 仍标着「已调整」，下次读还会走一遍合并——`compat.toml` 的 `update_user_rule` 是同一条。
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
/// # 为什么用 `U+XXXX` 而不是字面字符或 `\u` 转义
///
/// 1. YAML/TOML 的双引号串里 `\u` 必须跟满 4 位十六进制，`\u00-\uFF` 直接解析报错；
/// 2. `\u` 转义长度定死 4 位（或 8 位 `\U`），写不了 `U+1F600` 这种 5 位码位；
/// 3. `U+XXXX` 是 Unicode 自己的标准写法（UAX #42），用户在任何 Unicode 资料里见到的
///    都是这个形态。
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
    // 上界按 Unicode 码位空间判，而不是 `char::from_u32`：代理区 D800-DFFF 不是合法
    // `char`，但作为**区间端点**出现在配置里是合理的（写 `U+0000-U+FFFF` 圈整个 BMP
    // 没有错）。逐字符判定时代理区本就不可能命中。
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

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> CharsetDoc {
        parse_doc(text).expect("解析")
    }

    #[test]
    fn parses_head_and_body() {
        let d = doc("---\nkey: mine\nname: 我的\norder: 5\ndefault: rare\n...\n★\n⌘\n-☯\n");
        assert_eq!(d.def.key, "mine");
        assert_eq!(d.def.display_name(), "我的");
        assert_eq!(d.def.order_or_default(), 5);
        assert_eq!(d.def.default, Some(Commonality::Rare));
        assert_eq!(d.added, vec!["★", "⌘"]);
        assert_eq!(d.removed, vec!["☯"]);
    }

    /// 只调字段、不动列表的覆盖文件没有 `...`，整份都是 meta 头——用户层最常见的形态。
    #[test]
    fn a_head_only_file_is_valid() {
        let d = doc("key: emoji\ndefault: common\n");
        assert_eq!(d.def.key, "emoji");
        assert!(d.added.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn body_skips_comments_and_blanks() {
        let d = doc("key: k\n...\n# 注释\n\n★\n");
        assert_eq!(d.added, vec!["★"]);
    }

    #[test]
    fn missing_key_is_rejected() {
        assert!(parse_doc("name: 没有 key\n").is_err());
    }

    /// 拼错字段名要报错而不是静默吞掉，否则得到一个「配了没反应」且毫无线索的类。
    #[test]
    fn unknown_field_is_rejected() {
        assert!(parse_doc("key: k\ndefualt: rare\n").is_err());
    }

    /// ★★★ 字段级合并：用户层只写 `default`，出厂的 `ranges` 与列表必须留下。
    /// 整文件覆盖会让这个类当场变成空集，且用户从此脱离出厂更新。
    #[test]
    fn overlay_keeps_what_it_did_not_mention() {
        let mut m = BTreeMap::new();
        apply_doc(
            &mut m,
            doc(
                "key: emoji\nname: 表情\nranges: ['U+1F300-U+1FAFF']\ndefault: rare\n...\n😀\n😁\n",
            ),
        );
        apply_doc(&mut m, doc("key: emoji\ndefault: common\n"));

        let e = &m["emoji"];
        assert_eq!(e.def.default, Some(Commonality::Common), "改过的字段生效");
        assert_eq!(
            e.def.ranges.as_deref(),
            Some(&["U+1F300-U+1FAFF".to_string()][..]),
            "★ 没提到的 ranges 必须留着"
        );
        assert_eq!(e.members.len(), 2, "★ 没提到的列表必须留着");
        assert_eq!(e.def.name.as_deref(), Some("表情"));
    }

    #[test]
    fn overlay_list_is_incremental_by_default() {
        let mut m = BTreeMap::new();
        apply_doc(&mut m, doc("key: k\n...\n😀\n😁\n"));
        apply_doc(&mut m, doc("key: k\n...\n★\n-😀\n"));

        let e = &m["k"];
        assert!(e.members.contains("★") && e.members.contains("😁"));
        assert!(!e.members.contains("😀"), "被 - 前缀移除");
        assert!(
            e.removed.contains("😀"),
            "★ 移除记录要留着——它还要作用于 file: 引用的外部字表"
        );
    }

    /// `replace: true` 是「我要接管整份字表」的显式声明，从此不跟随出厂更新。
    #[test]
    fn replace_wipes_the_lower_layers_list() {
        let mut m = BTreeMap::new();
        apply_doc(&mut m, doc("key: k\nname: 出厂\n...\n😀\n😁\n"));
        apply_doc(&mut m, doc("key: k\nreplace: true\n...\n★\n"));

        let e = &m["k"];
        assert_eq!(e.members.iter().cloned().collect::<Vec<_>>(), vec!["★"]);
        assert_eq!(
            e.def.name.as_deref(),
            Some("出厂"),
            "replace 只管列表，不动 meta"
        );
    }

    /// 先移除、后重新加入 ⇒ 该条重新生效，且不再留在排除集里
    /// （否则 `file:` 引用的那份外部字表里的同一条会被误删）。
    #[test]
    fn re_adding_a_removed_entry_clears_the_removal() {
        let mut m = BTreeMap::new();
        apply_doc(&mut m, doc("key: k\n...\n-☯\n"));
        apply_doc(&mut m, doc("key: k\n...\n☯\n"));
        let e = &m["k"];
        assert!(e.members.contains("☯"));
        assert!(!e.removed.contains("☯"));
    }

    /// 合并宏漏写字段是静默的，用「全字段非 None 的 overlay 应整体生效」把它变成会红的测试。
    #[test]
    fn merge_covers_every_field() {
        let full = CharsetDef {
            key: "k".into(),
            name: Some("n".into()),
            ranges: Some(vec!["U+1-U+2".into()]),
            file: Some("f.txt".into()),
            scope: Some(ScopeKind::Han),
            default: Some(Commonality::Rare),
            outside: Some(Commonality::Common),
            order: Some(7),
            no_freq: Some(true),
            in_rare: Some(true),
            enabled: Some(false),
            replace: Some(true),
        };
        let mut base = CharsetDef {
            key: "k".into(),
            ..Default::default()
        };
        base.merge_from(full.clone());
        assert_eq!(base, full, "有字段没被 merge_from 覆盖到");
    }

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
        for bad in ["4E00-9FFF", "U+9FFF-U+4E00", "U+ZZZZ", "U+110000", "U+"] {
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

    #[test]
    fn scans_only_yaml_and_in_a_stable_order() {
        let dir = std::env::temp_dir().join(format!("charsets_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.yaml"), "key: b\n").unwrap();
        std::fs::write(dir.join("a.yaml"), "key: a\n").unwrap();
        std::fs::write(dir.join("note.md"), "not a charset").unwrap();
        std::fs::write(dir.join("c.yaml.bak"), "key: nope\n").unwrap();

        let docs = load_layer(&dir);
        let keys: Vec<&str> = docs.iter().map(|d| d.def.key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b"], "只扫 .yaml，且按文件名定序");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一个写坏的文件不该让整层失效。
    #[test]
    fn a_broken_file_does_not_kill_the_layer() {
        let dir = std::env::temp_dir().join(format!("charsets_broken_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ok.yaml"), "key: ok\n").unwrap();
        std::fs::write(dir.join("bad.yaml"), "key: [不是字符串\n").unwrap();

        let docs = load_layer(&dir);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].def.key, "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 列表体的两级切分 ───────────────────────────────────────────────────

    fn members_of(body: &str) -> Vec<String> {
        let doc = parse_doc(&format!("---\nkey: t\n...\n{body}")).unwrap();
        doc.added
    }

    /// ★ 一行写了多个字：**切开**，而不是当成一个永远匹配不上的成员。
    ///
    /// 自带文件一行一个，但用户照旧 `common_chars.txt` 的习惯连写完全可能——静默当成
    /// 一个成员的话，那一行一个字都不生效，而文件、日志、界面三处都不报错。
    #[test]
    fn a_line_with_several_chars_is_split_rather_than_taken_whole() {
        assert_eq!(members_of("的一是"), vec!["的", "一", "是"]);
        // 空白也能分隔，两种写法可以混用。
        assert_eq!(members_of("的 一是\n不"), vec!["的", "一", "是", "不"]);
    }

    /// ★★ 多码位簇**整体**算一个成员，不会被拆散。
    ///
    /// 拆散的后果是配置里写 `⚽️` 却匹配不上候选里那个簇——两边各自看都对。
    #[test]
    fn a_multi_code_point_cluster_stays_whole() {
        assert_eq!(members_of("⚽\u{FE0F}"), vec!["⚽\u{FE0F}"]);
        assert_eq!(
            members_of("👨\u{200D}👩\u{200D}👧"),
            vec!["👨\u{200D}👩\u{200D}👧"]
        );
        assert_eq!(members_of("1\u{FE0F}\u{20E3}"), vec!["1\u{FE0F}\u{20E3}"]);
    }

    /// ⚠️ **空白分隔存在的唯一理由**：相邻的区域指示符会合成国旗。
    ///
    /// `🇨🇳` 连写是一面旗（一个成员），要表达「两个区域指示符字母」只能用空格分开。
    /// 字素簇切分解决不了「两个成员恰好能拼成第三个」。
    #[test]
    fn adjacent_regional_indicators_need_a_space_to_stay_apart() {
        assert_eq!(members_of("🇨🇳").len(), 1, "连写合成国旗，是一个成员");
        assert_eq!(members_of("🇨 🇳").len(), 2, "要分开就得用空格");
    }

    /// `-` 前缀作用于**整个词**。
    #[test]
    fn the_remove_prefix_covers_the_whole_word() {
        let doc = parse_doc("---\nkey: t\n...\n-的一是 好\n").unwrap();
        assert_eq!(doc.removed, vec!["的", "一", "是"]);
        assert_eq!(doc.added, vec!["好"]);
    }

    // ── 用户层：渲染 / 编辑视图 / diff ─────────────────────────────────────

    fn doc_of(key: &str) -> CharsetDoc {
        CharsetDoc {
            def: CharsetDef {
                key: key.to_string(),
                ..Default::default()
            },
            added: Vec::new(),
            removed: Vec::new(),
        }
    }

    /// 一个像出厂 emoji 那样的类：带 ranges、带成员、不表态。
    fn factory_emoji() -> MergedClass {
        let mut m = BTreeMap::new();
        apply_doc(
            &mut m,
            parse_doc("key: emoji\nname: Emoji 表情\nranges: [U+1F600-U+1F64F]\norder: 20\n...\n😀\n😃\n😄\n").unwrap(),
        );
        m.remove("emoji").unwrap()
    }

    /// ★★ 渲染**只写用户改过的字段**，不带出厂值。
    ///
    /// 写全量会把当前的 `ranges` / `order` 固化进用户层 ⇒ 出厂补了新 emoji 或调了顺序，
    /// 这个用户永远拿不到。这是本仓踩过三次的坑（设计文档 §4.1）。
    #[test]
    fn rendering_writes_only_what_the_user_changed() {
        let mut doc = doc_of("emoji");
        doc.def.default = Some(Commonality::Rare);
        let text = render_doc(&doc).unwrap();
        assert!(text.contains("key: emoji"));
        assert!(text.contains("default: rare"));
        for absent in [
            "ranges", "order", "file", "scope", "outside", "no_freq", "in_rare",
        ] {
            assert!(
                !text.contains(&format!("\n{absent}:")),
                "没改过的 {absent} 不该被写进用户层（会固化出厂值）"
            );
        }
        assert!(!text.contains("\n...\n"), "没有成员增删就不该有列表体");
    }

    /// 渲染出来的东西读得回来——写端与读端用的是同一套语法。
    #[test]
    fn a_rendered_doc_round_trips() {
        let mut doc = doc_of("mine");
        doc.def.name = Some("我的类".into());
        doc.def.order = Some(7);
        doc.def.no_freq = Some(true);
        doc.added = vec!["★".into(), "☆".into()];
        doc.removed = vec!["☀".into()];
        let back = parse_doc(&render_doc(&doc).unwrap()).unwrap();
        assert_eq!(back, doc);
    }

    /// 渲染出来的文本带头注释，说明「只含改过的字段」——备份包里用户会直接看到它。
    #[test]
    fn the_rendered_text_explains_it_is_sparse() {
        let mut doc = doc_of("emoji");
        doc.def.order = Some(1);
        let text = render_doc(&doc).unwrap();
        assert!(text.starts_with('#'), "开头得是注释");
        assert!(text.contains("只含你改过的字段"));
    }

    /// ⛔ 危险的 key 在**入口**就被拒，不做「安全化」。
    ///
    /// 安全化会让两个不同的 key 落到同一个文件上，后写的静默覆盖先写的——用户看到的是
    /// 「改了 A 类，B 类的设置没了」。
    #[test]
    fn dangerous_keys_are_rejected_not_sanitized() {
        for bad in [
            "../escape",
            "a/b",
            "a\\b",
            "C:stream",
            ".",
            "..",
            "",
            " lead",
            "trail ",
            "wi*ld",
        ] {
            assert!(!is_valid_key(bad), "「{bad}」不该被当作合法 key");
        }
        for good in ["emoji", "common_han", "表情符号", "my-set_1"] {
            assert!(is_valid_key(good), "「{good}」该是合法 key");
        }
        assert!(
            render_doc(&doc_of("../escape")).is_err(),
            "非法 key 必须渲染失败"
        );
        assert!(
            diff_against_factory(doc_of("a/b"), None).is_err(),
            "导入一份 key 非法的文件也必须拒绝"
        );
    }

    /// 库里的文本按条解析：坏的一条跳过、库键与文本 key 不一致的跳过，其余照常。
    #[test]
    fn stored_docs_skip_the_broken_and_the_mismatched() {
        let docs = parse_stored_docs([
            ("emoji", "key: emoji\ndefault: rare\n"),
            ("broken", "key: [\n"),
            ("mismatch", "key: something_else\n"),
            ("mine", "key: mine\n...\n★\n"),
        ]);
        let keys: Vec<&str> = docs.iter().map(|d| d.def.key.as_str()).collect();
        assert_eq!(keys, ["emoji", "mine"]);
        assert_eq!(docs[1].added, vec!["★".to_string()]);
    }

    // ── 编辑视图 ───────────────────────────────────────────────────────────

    /// ★ 首行必须说「此文件不会被自动读取」——用户拍板的措辞，也是这份文件最容易被
    /// 误解的地方。
    #[test]
    fn the_edit_view_says_it_is_not_read_automatically() {
        let text = render_edit_view(Some(&factory_emoji()), &doc_of("emoji")).unwrap();
        let first = text.lines().next().unwrap();
        assert!(first.contains("不会被自动读取"), "首行：{first}");
        assert!(text.contains("从文件加载"), "得告诉用户回哪里导入");
        assert!(
            text.contains("「Emoji 表情」（key: emoji）"),
            "得写明是哪个类"
        );
    }

    /// 视图 = 出厂 + 用户调整的**生效**形态：字段合并、成员并集减移除、出厂顺序在前。
    #[test]
    fn the_edit_view_shows_the_effective_definition() {
        let mut user = doc_of("emoji");
        user.def.default = Some(Commonality::Rare);
        user.added = vec!["🫠".into()];
        user.removed = vec!["😃".into()];
        let text = render_edit_view(Some(&factory_emoji()), &user).unwrap();

        assert!(text.contains("\nname: Emoji 表情\n"), "出厂字段要在");
        assert!(text.contains("\norder: 20\n"));
        assert!(text.contains("\ndefault: rare\n"), "用户覆盖的字段要生效");
        let body: Vec<&str> = text.split("\n...\n").nth(1).unwrap().lines().collect();
        assert_eq!(
            body,
            ["😀", "😄", "🫠"],
            "移除的不在、新增的在末尾、出厂顺序保留"
        );
    }

    /// 出厂类的 ranges 写成**注释**：看得见、改不动（改了 diff 时拒绝）。
    #[test]
    fn factory_ranges_are_shown_as_a_comment() {
        let text = render_edit_view(Some(&factory_emoji()), &doc_of("emoji")).unwrap();
        assert!(
            !text.contains("\nranges:"),
            "不能作为字段出现，否则用户改了会以为能导入"
        );
        assert!(text.contains("# ranges: [U+1F600-U+1F64F]"), "但要能看见");
        assert!(text.contains("范围只读"));
    }

    /// 自建类（没有出厂对应）的视图就是它自己，ranges 是正常字段。
    #[test]
    fn a_custom_class_edit_view_is_itself() {
        let mut user = doc_of("mine");
        user.def.name = Some("我的符号".into());
        user.def.ranges = Some(vec!["U+2600-U+26FF".into()]);
        user.added = vec!["★".into()];
        let text = render_edit_view(None, &user).unwrap();
        assert!(
            text.contains("\nranges:\n- U+2600-U+26FF\n")
                || text.contains("ranges: [U+2600-U+26FF]")
        );
        assert!(text.ends_with("...\n★\n"));
    }

    // ── diff ───────────────────────────────────────────────────────────────

    /// ★★ 导出 → 原样导入 ⇒ 用户层**不变**。这是「完整视图不固化出厂值」的凭据。
    #[test]
    fn export_then_import_unchanged_is_a_no_op() {
        let f = factory_emoji();
        let mut user = doc_of("emoji");
        user.def.default = Some(Commonality::Rare);
        user.added = vec!["🫠".into()];
        user.removed = vec!["😃".into()];

        let text = render_edit_view(Some(&f), &user).unwrap();
        let back = diff_against_factory(parse_doc(&text).unwrap(), Some(&f)).unwrap();
        assert_eq!(back, user);
    }

    /// 一个没调整过的类，导出再导入，用户层仍是空壳——不能因为导出过就多出一条记录。
    #[test]
    fn export_then_import_a_pristine_class_yields_an_empty_override() {
        let f = factory_emoji();
        let text = render_edit_view(Some(&f), &doc_of("emoji")).unwrap();
        let back = diff_against_factory(parse_doc(&text).unwrap(), Some(&f)).unwrap();
        assert!(is_empty_override(&back), "实得 {back:?}");
    }

    /// 视图里改一个字段、删一个字、加一个字 ⇒ diff 里恰好三条。
    #[test]
    fn diff_records_exactly_what_changed() {
        let f = factory_emoji();
        let mut text = render_edit_view(Some(&f), &doc_of("emoji")).unwrap();
        text = text.replace("\norder: 20\n", "\norder: 5\n");
        text = text.replace("\n😃\n", "\n");
        text.push_str("🫠\n");

        let back = diff_against_factory(parse_doc(&text).unwrap(), Some(&f)).unwrap();
        assert_eq!(back.def.order, Some(5));
        assert_eq!(back.def.name, None, "没改的字段不能被固化");
        assert_eq!(back.added, vec!["🫠".to_string()]);
        assert_eq!(back.removed, vec!["😃".to_string()]);
    }

    /// 删掉字段那一行 = 沿用出厂，不是「不表态」——字段级合并表达不了后者。
    #[test]
    fn deleting_a_field_line_means_follow_factory() {
        let f = factory_emoji();
        let mut user = doc_of("emoji");
        user.def.order = Some(5);
        let text = render_edit_view(Some(&f), &user)
            .unwrap()
            .replace("\norder: 5\n", "\n");
        let back = diff_against_factory(parse_doc(&text).unwrap(), Some(&f)).unwrap();
        assert_eq!(
            back.def.order, None,
            "用户层不再覆盖 order ⇒ 生效值回到出厂 20"
        );
    }

    /// ⛔ 出厂类的 ranges 改了要**报错**，不静默丢弃。
    #[test]
    fn changing_factory_ranges_is_rejected_loudly() {
        let f = factory_emoji();
        let mut parsed = doc_of("emoji");
        parsed.def.ranges = Some(vec!["U+0000-U+FFFF".into()]);
        let e = diff_against_factory(parsed, Some(&f)).unwrap_err();
        assert!(e.to_string().contains("范围只读"), "{e}");

        // 写回与出厂相同的 ranges 则无事：用户可能把注释解开又没改。
        let mut same = doc_of("emoji");
        same.def.ranges = f.def.ranges.clone();
        let back = diff_against_factory(same, Some(&f)).unwrap();
        assert_eq!(back.def.ranges, None, "等于出厂就不该进用户层");
    }

    /// 自建类原样存；`replace` 对它无意义，清掉。
    #[test]
    fn a_custom_class_is_stored_as_is() {
        let mut parsed = doc_of("mine");
        parsed.def.ranges = Some(vec!["U+2600-U+26FF".into()]);
        parsed.def.replace = Some(true);
        parsed.added = vec!["★".into()];
        let back = diff_against_factory(parsed.clone(), None).unwrap();
        assert_eq!(back.def.ranges, parsed.def.ranges);
        assert_eq!(back.added, parsed.added);
        assert_eq!(back.def.replace, None);
    }
}
