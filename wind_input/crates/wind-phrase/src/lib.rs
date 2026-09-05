//! 短语系统：静态/动态短语模板展开 + 命令栏（cmdbar）双路径。
//!
//! 与 Go 版本 `wind_input/internal/dict/phrase.go` + `internal/cmdbar` 对齐。
//! 加载 `system.phrases.toml`，输入码命中短语 code 时生成候选。
//!
//! **双路径**（对齐 Go design §7.2）：
//! - 短语 text 使用命令栏语法（含 `$CC(`/`$SS(`/`$AA(` marker 或顶层 `{expr}` 插值）→ 经
//!   `wind-cmdbar` 解析求值（`{date()}`/`{calc(code)}`/`{upper(code)}`/`$SS` 字符串组/`$AA` 字符组等）。
//! - 否则 → 旧的简单模板变量展开
//!   （$Y/$YYYY/$YY/$M/$MM/$D/$DD/$HH/$mm/$ss/$WC/$YC/$MC/$DC/$ts/$tsms）。
//!
//! 命令栏 display 侧只用纯函数（无需宿主服务）；`$CC` 的副作用动作需平台服务（按键/剪贴板/
//! 进程注入），Rust 端平台层尚缺，故当前仅显现 display 候选，动作执行待平台服务补齐。

use chrono::{DateTime, Datelike, Local, Timelike};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};
use wind_cmdbar::{
    Phrase, PhraseEval, Services, default_registry, evaluate, evaluate_phrase, is_cmdbar_grammar,
    parse,
};

/// 一条短语（同 code 下按 weight 降序、position 升序排列）
#[derive(Debug, Clone, Default)]
pub struct PhraseEntry {
    pub text: String,
    pub weight: i32,
    pub position: i32,
    /// 是否系统短语（来自 system.phrases.toml / store `is_system=true`）；false=用户短语。
    pub is_system: bool,
    /// 分类（`""` = 未分类）。方案级 `[phrases]` 按它做白/黑名单过滤，见 [`PhraseScope`]。
    pub category: String,
}

/// 方案级短语作用域：一次查询里「哪些短语算数」。
///
/// # ★ 为什么它是**必填参数**而不是 `Option` 或另开一族 `*_scoped` 方法
///
/// 短语查询有六个消费点（两处候选生成、两处「这个码位归短语管」的判据、临英两处）。
/// **只漏掉 `phrase_owns_code` 的表现是：短语候选不出现了，但顶码与自动上屏仍被短语层
/// 否决 ⇒ 打字卡住不上屏，且零日志。** 可选参数或并行方法族都把「别漏」变成自觉，
/// 而这里必须是编译期强制——新增查询方法时同样躲不过。
///
/// 见 `docs/design/schema-scoped-behavior.md` §6.3。
#[derive(Debug, Clone, Copy)]
pub struct PhraseScope<'a> {
    /// 本方案是否加载短语。`false` ⇒ 所有查询直接短路。
    pub enabled: bool,
    /// 白名单。**空 = 不施加这一项限制**（全部分类），不是「一条都不要」——
    /// 「一条都不要」由 `enabled = false` 表达。空串 `""` 匹配未分类短语。
    pub categories: &'a [String],
    /// 黑名单。空 = 不排除；在白名单之后再减。
    pub exclude: &'a [String],
}

impl PhraseScope<'_> {
    /// 不施加任何限制。测试与「无方案上下文」的调用点用。
    ///
    /// 它仍然是**显式写出来的**决定，与「忘了过滤」在代码里长得不一样——这正是必填参数
    /// 想要的效果。
    pub const ALL: PhraseScope<'static> = PhraseScope {
        enabled: true,
        categories: &[],
        exclude: &[],
    };

    /// 这条短语算不算数。
    pub fn admits(&self, e: &PhraseEntry) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.categories.is_empty() && !self.categories.iter().any(|c| c == &e.category) {
            return false;
        }
        !self.exclude.iter().any(|c| c == &e.category)
    }

    /// 整体关闭的快路径：`has_longer_code` 这类全表扫描据此直接返回，不必逐条问。
    pub fn is_closed(&self) -> bool {
        !self.enabled
    }

    /// 是否**完全不过滤**（只判 enabled，不看分类）——全表扫描的快路径判据。
    fn admits_everything(&self) -> bool {
        self.enabled && self.categories.is_empty() && self.exclude.is_empty()
    }
}

/// 一条短语命中：展开后的候选文本 + 权重 + 可选命令源 / 前缀导航目标。
/// - `command_src` 非空 → 这是 `$CC` 命令短语（选中时执行动作而非上屏 text），
///   其值为待重新求值/执行的命令源（如 `$CC("切简繁", ime.toggle("s2t"))`）。
/// - `nav_code` 非空 → 这是**前缀导航候选**（敲 `zz`/`co` 列出的 `zzbd`/`coen` 等），
///   `text` 为组名/命令显示名，`comment` 为码后缀（如 `bd`）。选中时补全输入到
///   `nav_code` 完整码并重查展开（见 coordinator commit_selected 的 is_group 臂）。
#[derive(Debug, Clone, PartialEq)]
pub struct PhraseHit {
    pub text: String,
    pub weight: i32,
    pub command_src: Option<String>,
    pub nav_code: Option<String>,
    pub comment: String,
    /// 原始记录文本（store 里的 `PhraseEntry.text`，模板/命令未展开）。
    /// 右键「禁用短语」按 (code, source_text) 定位 store 记录（display 可能是展开后文本）。
    pub source_text: String,
    /// 是否系统短语（`is_system=true`）；false=用户短语。调试提示区分来源用。
    pub is_system: bool,
}

impl PhraseHit {
    fn plain(text: String, weight: i32) -> Self {
        Self {
            text,
            weight,
            command_src: None,
            nav_code: None,
            comment: String::new(),
            source_text: String::new(),
            is_system: false,
        }
    }

    /// 附上原始记录文本（构造点统一经此填充，测试用 `plain` 直构不填）。
    fn with_source(mut self, src: &str) -> Self {
        self.source_text = src.to_string();
        self
    }

    /// 附上注释（前缀命中时＝**剩余编码**）。
    ///
    /// 只在 [`PhraseLayer::lookup_prefix_at`] 用：精确路径（`lookup_at`）没有剩余编码，
    /// 填了就是假提示。marker 短语（`$SS`/`$AA`/`$CC`）经 `nav`/`command_nav` 早就带上了
    /// 同一个 `suffix`，静态短语（Literal/Template）此前漏填——同一串码下，系统 `zz*` 分组
    /// 看得见还差几个字母、用户自己加的静态短语却看不见。
    fn with_comment(mut self, comment: String) -> Self {
        self.comment = comment;
        self
    }

    /// 前缀导航——**组**候选（`$SS`/`$AA`）：`code` 为补全目标完整码，选中后补全展开。
    fn nav(text: String, weight: i32, code: String, comment: String) -> Self {
        Self {
            text,
            weight,
            command_src: None,
            nav_code: Some(code),
            comment,
            source_text: String::new(),
            is_system: false,
        }
    }

    /// 前缀导航——**命令**候选（`$CC`）：选中后**直接执行** `src`（不二级展开），
    /// `code` 为完整码（执行时作输入上下文），`comment` 为码后缀。
    fn command_nav(text: String, weight: i32, src: String, code: String, comment: String) -> Self {
        Self {
            text,
            weight,
            command_src: Some(src),
            nav_code: Some(code),
            comment,
            source_text: String::new(),
            is_system: false,
        }
    }
}

/// TOML 系统短语原始条目（platform 过滤后），供上层同步入库。
#[derive(Debug, Clone)]
pub struct SystemPhraseEntry {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub position: i32,
    /// 分类（`""` = 未分类），来自 TOML 的 `category` 键。
    pub category: String,
}

/// [`PhraseLayer::from_records`] 的输入记录。
#[derive(Debug, Clone)]
pub struct PhraseSeed {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub position: i32,
    pub is_system: bool,
    /// 分类（`""` = 未分类）。
    pub category: String,
}

/// 短语层：code → 多条短语
#[derive(Debug, Default)]
pub struct PhraseLayer {
    map: HashMap<String, Vec<PhraseEntry>>,
}

#[derive(serde::Deserialize)]
struct PhrasesFile {
    #[serde(default)]
    phrases: Vec<RawPhrase>,
}

#[derive(serde::Deserialize)]
struct RawPhrase {
    code: String,
    text: String,
    #[serde(default)]
    weight: Option<i32>,
    #[serde(default)]
    position: Option<i32>,
    #[serde(default)]
    platform: Option<String>,
    /// 分类（缺省 = 未分类）。方案级 `[phrases] categories` 按它过滤。
    #[serde(default)]
    category: Option<String>,
}

impl PhraseLayer {
    /// 从 system.phrases.toml 加载（文件缺失/解析失败 → 空层）
    pub fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        let parsed: PhrasesFile = match toml::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                warn!("Parse phrases failed {}: {}", path.display(), e);
                return Self::default();
            }
        };
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        for r in parsed.phrases {
            // 平台过滤：空/"all"/"windows" 接受
            if let Some(p) = &r.platform {
                let p = p.to_lowercase();
                if !p.is_empty() && p != "all" && p != "windows" {
                    continue;
                }
            }
            map.entry(r.code).or_default().push(PhraseEntry {
                text: r.text,
                weight: r.weight.unwrap_or(1000),
                position: r.position.unwrap_or(0),
                // system.phrases.toml 内均为系统短语。
                is_system: true,
                category: r.category.unwrap_or_default(),
            });
        }
        for v in map.values_mut() {
            v.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.position.cmp(&b.position)));
        }
        Self { map }
    }

    /// 解析 system.phrases.toml 为原始条目（platform 过滤，默认 weight=1000/position=0）。
    /// 供 coordinator 同步进 store；文件缺失/解析失败 → 空。
    pub fn parse_system_entries(path: &std::path::Path) -> Vec<SystemPhraseEntry> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let parsed: PhrasesFile = match toml::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                warn!("Parse phrases failed {}: {}", path.display(), e);
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for r in parsed.phrases {
            if let Some(p) = &r.platform {
                let p = p.to_lowercase();
                if !p.is_empty() && p != "all" && p != "windows" {
                    continue;
                }
            }
            out.push(SystemPhraseEntry {
                code: r.code,
                text: r.text,
                weight: r.weight.unwrap_or(1000),
                position: r.position.unwrap_or(0),
                category: r.category.unwrap_or_default(),
            });
        }
        out
    }

    /// 从 [`PhraseSeed`] 记录构建短语层（调用方只传 enabled 项）。
    ///
    /// 用结构体而不是元组：字段已经六个，位置参数在调用点读不出谁是谁，
    /// 而 `(String, String, ...)` 里错位两个同类型字段编译器一声不吭。
    pub fn from_records(records: impl IntoIterator<Item = PhraseSeed>) -> Self {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        for r in records {
            map.entry(r.code).or_default().push(PhraseEntry {
                text: r.text,
                weight: r.weight,
                position: r.position,
                is_system: r.is_system,
                category: r.category,
            });
        }
        for v in map.values_mut() {
            v.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.position.cmp(&b.position)));
        }
        Self { map }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 查 code 对应的展开短语；跳过含不支持变量的项。
    /// `last` 为上屏历史快照（index 0 = 最近），供命令栏 display 侧的 `last(n)` 使用
    /// （如 `coll` 的 `$CC(last(), ...)` 候选需显示上一次上屏内容）。
    /// `host` 为宿主能力回调束（剪贴板 / 反查），见 [`PhraseHost`]。测试传 [`PhraseHost::empty`]。
    pub fn lookup(
        &self,
        code: &str,
        last: &[String],
        host: &PhraseHost<'_>,
        scope: &PhraseScope<'_>,
    ) -> Vec<PhraseHit> {
        self.lookup_at(code, Local::now(), last, host, scope)
    }

    /// 同 lookup，但显式传入时间（便于测试）。
    pub fn lookup_at(
        &self,
        code: &str,
        now: DateTime<Local>,
        last: &[String],
        host: &PhraseHost<'_>,
        scope: &PhraseScope<'_>,
    ) -> Vec<PhraseHit> {
        if scope.is_closed() {
            return Vec::new();
        }
        let entries = match self.map.get(code) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        for e in entries {
            if !scope.admits(e) {
                continue;
            }
            let sys_start = out.len();
            if is_cmdbar_grammar(&e.text) {
                // 命令栏路径：display 求值（纯函数 + last/clip 上下文，无副作用服务）。
                let ctx = PhraseCtx {
                    input: code.to_string(),
                    now,
                    last,
                    host,
                };
                match evaluate_phrase(&e.text, &ctx, default_registry()) {
                    // 无动作（literal/template，如 {date()}）→ 显示即上屏文本。
                    //
                    // 空展开不出空候选：与下方 `expand_template` 分支、以及 `lookup_prefix_at`
                    // 同处置。求值型 display 落空是**常态**而非异常——`{dict.rev(clip(),2)}`
                    // 在剪贴板只有 1 个字时就该什么都不出，而不是给一条点了没反应的空白候选。
                    Ok(PhraseEval::Single { display, actions })
                        if actions.is_empty() && !display.is_empty() =>
                    {
                        out.push(PhraseHit::plain(display, e.weight).with_source(&e.text))
                    }
                    // 无动作且 display 为空：整条丢弃（不落候选）。
                    Ok(PhraseEval::Single { actions, .. }) if actions.is_empty() => {}
                    // $CC 命令短语（有动作）：携带命令源，选中时由 coordinator 执行动作。
                    Ok(PhraseEval::Single { display, .. }) => out.push(PhraseHit {
                        text: display,
                        weight: e.weight,
                        command_src: Some(e.text.clone()),
                        nav_code: None,
                        comment: String::new(),
                        source_text: e.text.clone(),
                        is_system: false,
                    }),
                    Ok(PhraseEval::Array(arr)) => {
                        for el in arr.elements {
                            // 仅显现无动作的字面元素（符号等）；带动作的嵌入 $CC 需元素级源，后续补。
                            // 空元素同样丢弃：`$SS` 的元素是**运行时**求值的，词条固定写 N 个
                            // 元素、各查第 1..N 个字时，剪贴板不足 N 字必然让尾部几条落空。
                            if el.actions.is_empty() && !el.display.is_empty() {
                                out.push(
                                    PhraseHit::plain(el.display, e.weight).with_source(&e.text),
                                );
                            }
                        }
                    }
                    // 隐私红线（docs/logging-convention.md）：warn 不得含词条明文，源文降到 debug。
                    Err(err) => {
                        warn!(
                            "cmdbar phrase eval failed (chars={}): {}",
                            e.text.chars().count(),
                            err
                        );
                        debug!("cmdbar phrase eval failed text={:?}", e.text);
                    }
                }
            } else if let Some(text) = expand_template(&e.text, &now) {
                // 短语文本按原样出候选：库里存的就是真实文本（含真换行）。
                // 转义只在系统边界发生（文本文件读写、设置页 UI 进出），见
                // `wind_dict::store_layer::record_to_candidate` 的同源说明。
                //
                // 全空展开（如整条只有一个非节日的 `$LF`）不出空候选——
                // 与前缀路径 `lookup_prefix_at` 同处置。
                if !text.is_empty() {
                    out.push(PhraseHit::plain(text, e.weight).with_source(&e.text));
                }
            } else {
                // 含**不支持**的模板变量（写错了变量名）→ 整条跳过。前缀路径
                // `lookup_prefix_at` 对同一失败同处置；两边曾经现象相反（打全码没反应、
                // 打前缀反而看得见字面 `$xxx` 并原样上屏），已统一。
                //
                // 注意「变量取不到值」不走这里：`$LF` 在非节日返回**空串**，整条照常展开
                // （见 `wind_quick_input::lunar::var` 的「`None` 与空串的分工」）。到得了
                // 这里的只有真写错的变量名，故留 warn——不留日志就没人查得动。
                // 隐私红线（docs/logging-convention.md）：warn 不得含词条明文，源文降到 debug。
                warn!(
                    "短语模板变量不支持，该条被跳过 (chars={})",
                    e.text.chars().count()
                );
                debug!("短语模板展开失败 code={:?} text={:?}", code, e.text);
            }
            // 本条 entry 产出的所有 hit 继承其系统/用户归属。
            for h in out[sys_start..].iter_mut() {
                h.is_system = e.is_system;
            }
        }
        out
    }

    /// 是否存在「码以 `code` 开头且严格更长」的短语（含普通字面/模板与 marker，全量扫描）。
    /// 供短语自动上屏的「无更长后继」判据——避免短码短语（如 `ab`）在还能续打成更长短语
    /// （`abc`）时被自动上屏、打断输入。码表侧的更长后继由引擎 `has_longer_code` 判。
    pub fn has_longer_code(&self, code: &str, scope: &PhraseScope<'_>) -> bool {
        if code.is_empty() || scope.is_closed() {
            return false;
        }
        if scope.admits_everything() {
            // 快路径：不过滤时只看键，省掉逐条 admits。
            return self
                .map
                .keys()
                .any(|k| k.len() > code.len() && k.starts_with(code));
        }
        self.map.iter().any(|(k, v)| {
            k.len() > code.len() && k.starts_with(code) && v.iter().any(|e| scope.admits(e))
        })
    }

    /// 是否存在**码恰为** `code` 的短语（不含更长后继，那是 [`Self::has_longer_code`]）。
    ///
    /// 供顶码上屏的「整串仍是精确匹配」判据补齐短语侧：引擎 `handle_top_code` 的
    /// `has_full_input_match` 只问码表（`DictManager`），短语层归协调器持有、引擎够不着，
    /// 于是码长超过满码长的短语（如 5 码短语在 4 码方案里）会被判成「溢出该顶字」，
    /// 顶掉前 N 码首选、余码续打——那条短语永远打不出来。
    pub fn has_exact_code(&self, code: &str, scope: &PhraseScope<'_>) -> bool {
        if code.is_empty() || scope.is_closed() {
            return false;
        }
        self.map
            .get(code)
            .is_some_and(|v| v.iter().any(|e| scope.admits(e)))
    }

    /// 前缀导航：敲 `code`（长度 ≥ `min_len`）时，列出所有**码以 `code` 开头但更长**的
    /// marker 短语（`$CC`/`$SS`/`$AA`，未显式 `{prefix: false}`），每条出一个导航候选——
    /// `text` 为组名/命令显示名，`comment` 为码后缀。选中后由 coordinator 补全到完整码再展开。
    /// 数据驱动：新增短语零配置自动列出（对齐 Go SearchCommand 情况 3）。
    ///
    /// 不含精确码本身（走 [`Self::lookup_at`]），不列普通字面/模板短语（无 marker，维持
    /// 精确匹配语义，对齐 Go SearchPrefix 对 `$X` 模板的处理）。
    pub fn lookup_prefix(
        &self,
        code: &str,
        last: &[String],
        min_len: usize,
        scope: &PhraseScope<'_>,
    ) -> Vec<PhraseHit> {
        self.lookup_prefix_at(code, Local::now(), last, min_len, scope)
    }

    /// 同 [`Self::lookup_prefix`]，显式传时间便于测试。
    pub fn lookup_prefix_at(
        &self,
        code: &str,
        now: DateTime<Local>,
        last: &[String],
        min_len: usize,
        scope: &PhraseScope<'_>,
    ) -> Vec<PhraseHit> {
        if code.is_empty() || code.len() < min_len || scope.is_closed() {
            return Vec::new();
        }
        let reg = default_registry();
        // 廉价上下文：clip/sel/app/title 返回空，**避免列举时读整个剪贴板**（如 coad 的
        // display `剪贴板加词:{clip()}` 会读全部剪贴板内容 → 内存暴涨）；last 仍取真实
        // 快照（仅 Vec 索引，廉价，coll/cozd 等可正常显示）。真正执行命令时才用完整上下文。
        let ctx = NavCtx {
            input: code.to_string(),
            now,
            last,
        };
        let mut out = Vec::new();
        for (full_code, entries) in &self.map {
            // 只列更长的码（精确码本身走 lookup）；码均为 ASCII，字节长即字符长。
            if full_code.len() <= code.len() || !full_code.starts_with(code) {
                continue;
            }
            let suffix = full_code[code.len()..].to_string();
            for e in entries {
                if !scope.admits(e) {
                    continue;
                }
                let sys_start = out.len();
                let phrase = match parse(&e.text) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                // prefix 语义：除非显式 `{prefix: false}` 否则都列。
                // Literal/Template → 普通命中（command_src=None, nav_code=None）；
                // $SS/$AA → **组** nav（选中补全到码再展开成员，二级选择）；
                // $CC → **命令** nav（选中**直接执行**，不二级展开），display 经廉价上下文求值。
                match &phrase {
                    Phrase::Literal(t) => {
                        // 旧式简单模板（$Y/$M/$D 等）不含 cmdbar marker/插值，parse 后为 Literal。
                        //
                        // ★ 展开失败 → **跳过该条**，与 [`Self::lookup_at`] 精确路径同处置。
                        // 这里曾是 `unwrap_or_else(|| t.clone())`「失败退回原文」，后果是同一条
                        // 短语在两条路径上现象相反：打全码一个候选都没有，打前缀反而看得见字面
                        // `$LY$LZ$LMD$LF` 且选中即原样上屏。展开产物与模板源不是一回事，
                        // 源里带着 `$` 语法，回退它等于把乱码当结果。
                        let Some(display) = expand_template(t, &now) else {
                            continue;
                        };
                        // 全空展开（如整条只有一个非节日的 `$LF`）不出空候选。
                        if display.is_empty() {
                            continue;
                        }
                        out.push(
                            PhraseHit::plain(display, e.weight)
                                .with_comment(suffix.clone())
                                .with_source(&e.text),
                        );
                    }
                    Phrase::Template(_) => {
                        // cmdbar 模板（含 {expr} 插值）：经 evaluate 求值。
                        let display = match evaluate(&phrase, &ctx, reg) {
                            Ok(ev) => ev.display,
                            Err(_) => continue,
                        };
                        // 全空求值不出空候选 —— 与上面 Literal 分支、及 `lookup_at` 同处置。
                        // 这里原先无条件 push：`{clip()}` / `{dict.rev(clip())}` 这类依赖瞬时
                        // 状态的模板，在**廉价的 NavCtx 下必然求值为空**（见 `NavCtx::clip` /
                        // `reverse_lookup` 的说明），于是打前缀时会列出一条纯空白的候选。
                        if display.is_empty() {
                            continue;
                        }
                        out.push(
                            PhraseHit::plain(display, e.weight)
                                .with_comment(suffix.clone())
                                .with_source(&e.text),
                        );
                    }
                    Phrase::Array(ap) => {
                        if ap.modifiers.get_bool("prefix") == Some(false) {
                            continue;
                        }
                        out.push(
                            PhraseHit::nav(
                                ap.name.clone(),
                                e.weight,
                                full_code.clone(),
                                suffix.clone(),
                            )
                            .with_source(&e.text),
                        );
                    }
                    Phrase::Command(cp) => {
                        if cp.modifiers.get_bool("prefix") == Some(false) {
                            continue;
                        }
                        let display = match evaluate(&phrase, &ctx, reg) {
                            Ok(ev) => ev.display,
                            Err(_) => continue,
                        };
                        out.push(
                            PhraseHit::command_nav(
                                display,
                                e.weight,
                                e.text.clone(),
                                full_code.clone(),
                                suffix.clone(),
                            )
                            .with_source(&e.text),
                        );
                    }
                }
                // 本条 entry 产出的所有导航 hit 继承其系统/用户归属。
                for h in out[sys_start..].iter_mut() {
                    h.is_system = e.is_system;
                }
            }
        }
        // 权重降序，同权重按完整码字母序——导航候选顺序稳定可预测。
        out.sort_by(|a, b| {
            b.weight.cmp(&a.weight).then_with(|| {
                a.nav_code
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.nav_code.as_deref().unwrap_or(""))
            })
        });
        out
    }
}

/// 命令栏 display 侧的 [`wind_cmdbar::EvalContext`] 适配器（短语候选生成用）。
/// 提供 input/now/env + 上屏历史 last + 剪贴板 clip（供 `coll`/`coad` 等命令的 display
/// 标签显示 `last()`/`clip()`）；sel/app/title 与副作用服务侧留空（生成阶段不跑动作）。
/// 词库候选 value 的展开结果（供 coordinator 候选后处理复用短语 / cmdbar 的统一展开逻辑）。
#[derive(Debug, Clone, PartialEq)]
pub enum DictExpansion {
    /// 非特殊语法：候选保持原样。
    None,
    /// **是**特殊语法，但求值结果为空 —— 该候选整条丢弃。
    ///
    /// 与 [`Self::None`] 的区别是这条**必须**存在的理由：`None` 的语义是「这不是特殊语法，
    /// 把原文当文本用」。求值成空时若返回 `None`，候选会显示成模板**源码字面**
    /// （`{dict.rev(clip())}`）并原样上屏 —— 用户看到的是「配了没生效」，而真相是
    /// 「语法没问题，只是这次查不到」。两者的正确呈现完全相反，故不能共用一个变体。
    ///
    /// 对齐短语层 `lookup_at` 对同一情形的处置（那边直接不 push）。
    Drop,
    /// 单候选替换：`display` 替换候选文本；`command_src = Some` 表示 `$CC` 命令
    /// （选中 / 顶屏时执行动作，display 仅作展示）。
    Single {
        display: String,
        command_src: Option<String>,
    },
    /// `$AA`/`$SS` 组：携组名 + 成员列表。由调用方按"精确码展开 / 前缀折叠为组名"决定呈现
    /// （见 coordinator `finalize_candidates`）——精确码时逐成员炸开，前缀时折叠为单个组名候选
    /// （选中补全到完整码再展开，二级选择，与短语前缀分组一致）。
    Group {
        name: String,
        items: Vec<(String, Option<String>)>,
    },
}

/// 展开一条**词库候选 value**（对齐 Go `dict.ValueExpander.Expand`/`ExpandToCandidates`）：
/// 判定顺序——cmdbar marker（`$CC(`/`$SS(`/`$AA(` 或顶层 `{..}` 插值）优先，其次简单模板变量
/// （`$Y/$M/$D/...`），都不含则 [`DictExpansion::None`]。让**普通用户 / 系统词库码表词条**也能
/// 像短语一样内嵌命令 / 模板 / 组，而非把 value 原文当文本上屏。
///
/// `input` 供 cmdbar 语法内 `input()` 求值；`now/last/host` 为 display 上下文。
pub fn expand_dict_value(
    text: &str,
    input: &str,
    now: DateTime<Local>,
    last: &[String],
    host: &PhraseHost<'_>,
) -> DictExpansion {
    // 快路径：无 `$` 与 `{` 一律非特殊语法（普通词条零开销）。
    if !text.contains('$') && !text.contains('{') {
        return DictExpansion::None;
    }
    if is_cmdbar_grammar(text) {
        let ctx = PhraseCtx {
            input: input.to_string(),
            now,
            last,
            host,
        };
        match evaluate_phrase(text, &ctx, default_registry()) {
            // 纯 literal/template（如 {date()}）：display 即上屏文本。
            // 空展开 → Drop（整条丢弃），**不是** None —— 见 `DictExpansion::Drop`。
            Ok(PhraseEval::Single { display, actions })
                if actions.is_empty() && display.is_empty() =>
            {
                DictExpansion::Drop
            }
            Ok(PhraseEval::Single { display, actions }) if actions.is_empty() => {
                DictExpansion::Single {
                    display,
                    command_src: None,
                }
            }
            // $CC 命令（有动作）：携命令源，选中 / 顶屏执行动作。
            Ok(PhraseEval::Single { display, .. }) => DictExpansion::Single {
                display,
                command_src: Some(text.to_string()),
            },
            // $AA/$SS 组：炸开为多个候选（仅显现无动作的字面元素，与短语 lookup_at 一致；
            // 带动作的嵌入 $CC 需元素级源，后续补）。
            Ok(PhraseEval::Array(arr)) => {
                let name = arr.name.clone();
                // 空元素同样丢弃，与 `lookup_at` 的数组分支同处置。
                let items: Vec<(String, Option<String>)> = arr
                    .elements
                    .into_iter()
                    .filter(|el| el.actions.is_empty() && !el.display.is_empty())
                    .map(|el| (el.display, None))
                    .collect();
                if items.is_empty() {
                    // 同上：`$SS(...)` 是货真价实的特殊语法，一个成员都没剩时该整条丢弃，
                    // 而不是把 `$SS("反查", "{dict.rev(clip(),1)}", …)` 这串源码当文本上屏。
                    DictExpansion::Drop
                } else {
                    DictExpansion::Group { name, items }
                }
            }
            Err(err) => {
                // 隐私红线（docs/logging-convention.md）：warn 不得含词条明文，源文降到 debug。
                warn!(
                    "cmdbar 词库候选求值失败 (chars={}): {}",
                    text.chars().count(),
                    err
                );
                debug!("cmdbar 词库候选求值失败 text={:?}", text);
                DictExpansion::None
            }
        }
    } else if let Some(expanded) = expand_template(text, &now)
        && expanded != text
    {
        // 简单模板变量（$Y年$M月$D日 等）：确有展开才替换（对齐 Go Changed 语义；
        // 含 $ 但非模板变量的普通文本如「价格$5」原样保留 → None）。
        DictExpansion::Single {
            display: expanded,
            command_src: None,
        }
    } else {
        DictExpansion::None
    }
}

/// 短语 display 求值所需的**宿主能力回调束**（剪贴板、反查）。
///
/// # 为什么是结构体而不是继续加形参
///
/// 与 [`ProcSpawn`](wind_cmdbar::ProcSpawn) 同一理由：这些回调都是「宿主注入的能力」，
/// 会随功能增长。加字段时每个构造点都会编译失败、被迫面对新能力；而多加一个形参
/// 很容易被某个调用点原样漏掉——本 crate 的调用点分散在协调器的四条候选构建路径上，
/// 漏掉一条的表现是「某个入口下这个功能就是不出来」，没有任何报错。
///
/// 本 crate 不依赖平台层，故一律以回调注入，不直接读剪贴板 / 词库。
pub struct PhraseHost<'a> {
    /// 剪贴板读取：`n==0/1` 取当前，`n>1` 取历史第 n 条。
    /// 供命令栏 display 侧的 `clip(n)` 使用（如 `coad` 的 `剪贴板加词:{clip()}` 标签）。
    ///
    /// ⚠️ 这条在**每次按键的候选构建期**都会被调用，实现方必须走非阻塞的缓存版读取
    /// （见 `HostServices::clipboard_get_text_cached` 的行为契约）。
    pub clip: &'a dyn Fn(i64) -> String,
    /// 反查渲染：`(待查文本, format 模板) -> 渲染结果`；查不到返回空串。
    /// 供 `dict.rev(...)` 使用。模板语法与候选注释段一致，渲染在宿主侧完成。
    pub reverse: &'a dyn Fn(&str, &str) -> String,
}

impl PhraseHost<'static> {
    /// 全空宿主：剪贴板与反查都返回空串。供测试与无平台能力的 headless 路径。
    pub fn empty() -> Self {
        const CLIP: &dyn Fn(i64) -> String = &|_| String::new();
        const REVERSE: &dyn Fn(&str, &str) -> String = &|_, _| String::new();
        PhraseHost {
            clip: CLIP,
            reverse: REVERSE,
        }
    }
}

struct PhraseCtx<'a> {
    input: String,
    now: DateTime<Local>,
    /// 上屏历史快照（index 0 = 最近）。
    last: &'a [String],
    /// 宿主能力回调束（剪贴板 / 反查）。
    host: &'a PhraseHost<'a>,
}

impl wind_cmdbar::EvalContext for PhraseCtx<'_> {
    fn input(&self) -> String {
        self.input.clone()
    }
    fn last(&self, n: i64) -> String {
        if n < 1 {
            return String::new();
        }
        self.last.get((n - 1) as usize).cloned().unwrap_or_default()
    }
    fn clip(&self, n: i64) -> String {
        (self.host.clip)(n)
    }
    fn reverse_lookup(&self, text: &str, format: &str) -> String {
        (self.host.reverse)(text, format)
    }
    fn sel(&self) -> String {
        String::new()
    }
    fn app(&self) -> String {
        String::new()
    }
    fn title(&self) -> String {
        String::new()
    }
    fn env(&self, name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }
    fn now(&self) -> DateTime<Local> {
        self.now
    }
    fn services(&self) -> Option<&Services> {
        None
    }
}

/// 前缀导航列举用的**廉价**求值上下文：`clip`/`sel`/`app`/`title` 一律返回空，
/// 避免列举多条命令时各自读整个剪贴板/前台窗口等昂贵副作用（内存暴涨根因）。
/// `last` 仍取真实快照（仅 Vec 索引，廉价），`now`/`env` 廉价照常。命令真正执行时
/// 由 coordinator 用完整 CmdbarCtx（含真实剪贴板）求值。
struct NavCtx<'a> {
    input: String,
    now: DateTime<Local>,
    last: &'a [String],
}

impl wind_cmdbar::EvalContext for NavCtx<'_> {
    fn input(&self) -> String {
        self.input.clone()
    }
    fn last(&self, n: i64) -> String {
        if n < 1 {
            return String::new();
        }
        self.last.get((n - 1) as usize).cloned().unwrap_or_default()
    }
    fn clip(&self, _n: i64) -> String {
        String::new() // 列举阶段不读剪贴板
    }
    fn reverse_lookup(&self, _text: &str, _format: &str) -> String {
        // 同 clip：列举阶段不查词库。前缀导航要为**每条**候选命令跑一次 display 求值，
        // 在这里做整词反查等于把 N 次词库查询摊到按键线程上。
        //
        // 后果是 `cofc` 这类反查命令在**前缀列举**时求值为空，进而被 `lookup_prefix_at`
        // 的空串守卫丢弃：它们打前缀时不出现在列表里，打全码才出。这是刻意取舍——
        // 它们的 display 本就依赖剪贴板这类瞬时状态，列举阶段给不出真值，
        // 与其列一条空白的不如不列。
        String::new()
    }
    fn sel(&self) -> String {
        String::new()
    }
    fn app(&self) -> String {
        String::new()
    }
    fn title(&self) -> String {
        String::new()
    }
    fn env(&self, name: &str) -> String {
        std::env::var(name).unwrap_or_default()
    }
    fn now(&self) -> DateTime<Local> {
        self.now
    }
    fn services(&self) -> Option<&Services> {
        None
    }
}

/// 展开模板字符串；遇到不支持的变量返回 None（该短语项被跳过）。
/// 支持 `$name`、`${name}`，`$$` 转义为字面 `$`。
pub fn expand_template(text: &str, now: &DateTime<Local>) -> Option<String> {
    // 解析规则（`$name` / `${name}` / `$$`）与快捷输入格式表共用同一份实现
    // （`wind_quick_input::template`）：同一套变量写法若由两份解析器各自实现，
    // 用户就得在两个配置文件里分别试探边界。本模块只提供取值（[`expand_var`]，绑当前时间）。
    wind_quick_input::template::expand(text, |name| expand_var(name, now))
}

const WEEKDAY_CN: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

/// 展开单个变量；不支持的返回 None。
fn expand_var(name: &str, now: &DateTime<Local>) -> Option<String> {
    Some(match name {
        "Y" => now.year().to_string(),
        // `$YYYY`（补零四位）/ `$YY`（后两位）与快捷输入格式表的 `year_var` **同名同义**。
        // 短语层绑系统当前时间，年份恒是四位，故 `$YYYY` 与 `$Y` 实际同值——但仍必须有：
        // 少一个名字，用户把格式表里的 `$YYYY-$MM-$DD` 抄进短语就会命中「未知变量」，
        // 整条短语静默作废（`expand_template` 的 `?`），症状是「这条短语打不出来」且日志干净。
        "YYYY" => format!("{:04}", now.year()),
        // `rem_euclid` 而非 `%`：与格式表同一份口径，公元前年份（负数）不会得到 `-6` 这种结果。
        "YY" => format!("{:02}", now.year().rem_euclid(100)),
        "M" => now.month().to_string(),
        "MM" => format!("{:02}", now.month()),
        "D" => now.day().to_string(),
        "DD" => format!("{:02}", now.day()),
        "HH" => format!("{:02}", now.hour()),
        "mm" => format!("{:02}", now.minute()),
        "ss" => format!("{:02}", now.second()),
        "WC" => WEEKDAY_CN[now.weekday().num_days_from_sunday() as usize].to_string(),
        // 中文数字读法与快捷输入的日期候选共用一份实现（`wind-quick-input`）：
        // 同一个「二〇二六年六月十四日」在两处取值不同，用户无从分辨谁对。
        "YC" => wind_quick_input::year_to_chinese(now.year()),
        "MC" => wind_quick_input::small_int_to_chinese(now.month()),
        "DC" => wind_quick_input::small_int_to_chinese(now.day()),
        // 农历（`$LMD` `$LY` `$LZ` `$LM` `$LD` `$LF`）：与快捷输入的日期候选共用
        // 同一份换算，差别只在数据源——这里绑当前时间，那边绑用户打进去的日期。
        //
        // 系统日期超出 1900–2100 时**换算不出**农历 → None（整条短语被跳过），
        // 免得 `农历$LMD` 只剩「农历」二字上屏。
        //
        // 而 `$LF` 在非节日返回**空串**（不是 None）：`$LY年$LMD$LF` 平常日子应给出
        // 「丙午年四月廿九」、端午当天给出「…端午节」——追加式写法是实际意图，
        // 让整条消失没人想要。两种「没有值」的分工见 `wind_quick_input::lunar::var`。
        n if wind_quick_input::lunar::is_var(n) => {
            let d = wind_quick_input::lunar::solar_to_lunar(now.year(), now.month(), now.day())?;
            wind_quick_input::lunar::var(n, &d)?
        }
        "ts" => now.timestamp().to_string(),
        "tsms" => now.timestamp_millis().to_string(),
        // 随机 UUID：与命令栏 `{uuid()}` 共用同一份生成逻辑（同 dir_var 的理由——
        // 同一个写法不能只在 $CC 里生效、直接写就不生效）。此处走默认格式，不会失败。
        "uuid" => wind_cmdbar::generate_uuid("").ok()?,
        // 内部目录变量（${APP_DIR} 等）与命令栏字符串走同一份真相源：同样的写法
        // 若只在 $CC 里生效、直接写就不生效，用户无从分辨是语法错还是没支持。
        _ => return wind_config::dir_var_str(name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn has_longer_code_detects_strict_prefix() {
        let layer = PhraseLayer::from_records([
            PhraseSeed {
                code: "date".into(),
                text: "X".into(),
                weight: 0,
                position: 0,
                is_system: false,
                category: String::new(),
            },
            PhraseSeed {
                code: "dates".into(),
                text: "Y".into(),
                weight: 0,
                position: 0,
                is_system: false,
                category: String::new(),
            },
        ]);
        assert!(
            layer.has_longer_code("dat", &PhraseScope::ALL),
            "date/dates 都比 dat 长"
        );
        assert!(
            layer.has_longer_code("date", &PhraseScope::ALL),
            "存在更长码 dates"
        );
        assert!(
            !layer.has_longer_code("dates", &PhraseScope::ALL),
            "dates 已最长，无更长后继"
        );
        assert!(
            !layer.has_longer_code("zzz", &PhraseScope::ALL),
            "无以 zzz 为前缀的短语"
        );
        assert!(!layer.has_longer_code("", &PhraseScope::ALL), "空码不算");
    }

    fn fixed() -> DateTime<Local> {
        // 2026-06-14 09:05:07 周日
        Local.with_ymd_and_hms(2026, 6, 14, 9, 5, 7).unwrap()
    }

    /// 测试用空剪贴板读取回调。
    /// 无宿主能力的求值环境（剪贴板与反查均返回空串）。
    fn no_clip() -> PhraseHost<'static> {
        PhraseHost::empty()
    }

    /// ★ 剪贴板反查（出厂 `cofc`）的端到端形状。
    ///
    /// 锁两件事：**纯模板短语的 display 就是上屏文本**（无 `command_src`，不需要
    /// `type()`），以及**查不到时整条候选消失**而不是留一条空白的。
    #[test]
    fn clipboard_reverse_renders_and_drops_when_nothing_found() {
        let layer = PhraseLayer::from_records(vec![PhraseSeed {
            code: "cofc".into(),
            text: "{dict.rev(clip())}".into(),
            weight: 2000,
            position: 0,
            is_system: true,
            category: String::new(),
        }]);
        let clip = |_n: i64| "好人".to_string();
        // 只认「好」，借此模拟「这个字查不到」。
        let reverse = |text: &str, _fmt: &str| {
            if text == "好" {
                "好: vbg hǎo".to_string()
            } else {
                String::new()
            }
        };
        let host = PhraseHost {
            clip: &clip,
            reverse: &reverse,
        };
        let hits = layer.lookup_at("cofc", fixed(), &[], &host, &PhraseScope::ALL);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "好: vbg hǎo");
        assert!(
            hits[0].command_src.is_none(),
            "无动作短语：display 即上屏文本，不该被当成命令候选"
        );

        // 剪贴板为空 → 无字可查 → 不出候选（而不是出一条空白候选）。
        let empty_clip = |_n: i64| String::new();
        let host = PhraseHost {
            clip: &empty_clip,
            reverse: &reverse,
        };
        assert!(
            layer
                .lookup_at("cofc", fixed(), &[], &host, &PhraseScope::ALL)
                .is_empty(),
            "剪贴板为空时不得产出空白候选"
        );
    }

    /// ★ `$SS` 逐字反查：**元素个数即上限**，剪贴板不足位数时尾部元素整条消失。
    ///
    /// 这是「限制 N 个字」的表达方式——不靠配置键，靠词条里写几个元素。
    #[test]
    fn reverse_group_drops_tail_when_clipboard_is_shorter() {
        let src = r#"$SS("反查", "{dict.rev(clip(),1)}", "{dict.rev(clip(),2)}", "{dict.rev(clip(),3)}")"#;
        let layer = PhraseLayer::from_records(vec![PhraseSeed {
            code: "cofc".into(),
            text: src.into(),
            weight: 2000,
            position: 0,
            is_system: true,
            category: String::new(),
        }]);
        let clip = |_n: i64| "好人".to_string(); // 只有 2 个字，第 3 个元素必然落空
        let reverse = |text: &str, _f: &str| format!("{text}=x");
        let host = PhraseHost {
            clip: &clip,
            reverse: &reverse,
        };
        let texts: Vec<String> = layer
            .lookup_at("cofc", fixed(), &[], &host, &PhraseScope::ALL)
            .into_iter()
            .map(|h| h.text)
            .collect();
        assert_eq!(
            texts,
            vec!["好=x".to_string(), "人=x".to_string()],
            "第 3 条应整条消失，而不是留一条空白候选"
        );
    }

    #[test]
    fn test_expand_date() {
        let now = fixed();
        assert_eq!(
            expand_template("$Y年$M月$D日", &now).unwrap(),
            "2026年6月14日"
        );
        assert_eq!(expand_template("$Y-$MM-$DD", &now).unwrap(), "2026-06-14");
    }

    /// ★ 年份四态（`$Y` `$YYYY` `$YY` `$YC`）与快捷输入格式表**同名同义**。
    ///
    /// 直接拿 `QuickValues` 做对照，而不是各写各的期望值：这两处是用户眼里的同一套写法
    /// （文档也是这么承诺的），任一侧少一个名字或取值不同，症状都是「同样的模板抄到短语里
    /// 一条候选都不出」且日志干净——`expand_template` 遇未知变量整条作废。
    #[test]
    fn test_expand_year_forms_agree_with_quick_format_table() {
        let now = fixed(); // 2026-06-14
        assert_eq!(expand_template("$YY-$MM-$DD", &now).unwrap(), "26-06-14");
        assert_eq!(expand_template("${YY}年", &now).unwrap(), "26年");
        assert_eq!(
            expand_template("$YYYY-$MM-$DD", &now).unwrap(),
            "2026-06-14"
        );
        let quick = wind_quick_input::QuickValues::Date {
            y: 2026,
            m: 6,
            d: 14,
        };
        for name in ["Y", "YYYY", "YY", "YC", "M", "MM", "MC", "D", "DD", "DC"] {
            assert_eq!(
                expand_var(name, &now),
                quick.get(name),
                "${name} 在短语与格式表两处取值不一致"
            );
        }
    }

    #[test]
    fn test_expand_time_and_week() {
        let now = fixed();
        assert_eq!(expand_template("$HH:$mm:$ss", &now).unwrap(), "09:05:07");
        assert_eq!(expand_template("星期$WC", &now).unwrap(), "星期日");
    }

    #[test]
    fn test_expand_chinese() {
        let now = fixed();
        assert_eq!(
            expand_template("${YC}年${MC}月${DC}日", &now).unwrap(),
            "二〇二六年六月十四日"
        );
    }

    /// 农历变量在短语里可用，取值与快捷输入同源。
    #[test]
    fn test_expand_lunar() {
        let now = fixed(); // 2026-06-14 → 丙午年四月廿九，非节日
        assert_eq!(expand_template("农历$LMD", &now).unwrap(), "农历四月廿九");
        assert_eq!(
            expand_template("$LY年$LMD", &now).unwrap(),
            "丙午年四月廿九"
        );
        assert_eq!(expand_template("${LM}", &now).unwrap(), "四月");
        assert_eq!(expand_template("${LD}", &now).unwrap(), "廿九");
        assert_eq!(expand_template("$LZ年", &now).unwrap(), "马年");
    }

    /// ★ `$LF` 在非节日展开成**空串**，整条照常出——不是整条消失。
    ///
    /// 用户写 `$LY年$LMD$LF` 要的是「节日当天追加节日名」，平常日子仍要得到日期。
    /// 曾经的 `None` 语义会让这类短语一年 355 天打不出来。
    #[test]
    fn test_expand_lunar_festival_is_empty_on_ordinary_days() {
        let plain = fixed(); // 2026-06-14 不是节日
        assert_eq!(expand_template("今天是$LF", &plain).unwrap(), "今天是");
        // 追加式写法：平常日子只少了节日名，日期部分照常
        assert_eq!(
            expand_template("$LY年$LMD$LF", &plain).unwrap(),
            "丙午年四月廿九"
        );

        let duanwu = Local.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
        assert_eq!(
            expand_template("今天是$LF", &duanwu).unwrap(),
            "今天是端午节"
        );
        assert_eq!(
            expand_template("$LY年$LMD$LF", &duanwu).unwrap(),
            "丙午年五月初五端午节"
        );
        assert_eq!(expand_template("$LMD", &duanwu).unwrap(), "五月初五");
    }

    /// ★ 用户实际写的那条短语：紧邻的四个农历变量必须各自独立展开。
    ///
    /// 两件事同时压：变量名边界（`$LMD` 不能被切成 `$LM`+`D`、`$LY` 不能吞掉后面的 `$LZ`），
    /// 以及非节日下 `$LF` 不再毒杀整条。
    #[test]
    fn test_expand_adjacent_lunar_vars() {
        let plain = fixed(); // 2026-06-14 非节日
        assert_eq!(
            expand_template("$LY$LZ$LMD$LF", &plain).unwrap(),
            "丙午马四月廿九"
        );
        let duanwu = Local.with_ymd_and_hms(2026, 6, 19, 9, 0, 0).unwrap();
        assert_eq!(
            expand_template("$LY$LZ$LMD$LF", &duanwu).unwrap(),
            "丙午马五月初五端午节"
        );
        // $LM 与 $LMD 是不同变量，紧邻时不得相互吞并
        assert_eq!(
            expand_template("$LM|$LD|$LMD", &plain).unwrap(),
            "四月|廿九|四月廿九"
        );
        // $LY 与 $LYN 同理（前者干支、后者农历年数字）
        assert_eq!(expand_template("$LY|$LYN", &plain).unwrap(), "丙午|2026");
    }

    /// ★ 写错的变量名仍须整条作废——「取不到值给空串」不能退化成「什么都放行」。
    ///
    /// 这两件事共用一个 `Option`，改 `$LF` 时最容易连带打穿这道保护。
    #[test]
    fn test_unknown_var_still_kills_template() {
        let now = fixed();
        assert!(expand_template("$LNOPE", &now).is_none());
        assert!(expand_template("农历$LMD$NOPE", &now).is_none());
    }

    #[test]
    fn test_expand_dir_var() {
        // 内部目录变量在普通短语文本里也展开：同样的 `${APP_DIR}` 若只在 $CC 里生效、
        // 直接写就不生效，用户无从分辨是写错了还是没支持。
        let now = fixed();
        let want = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(!want.is_empty(), "APP_DIR 期望值不该为空");
        assert_eq!(expand_template("${APP_DIR}", &now).unwrap(), want);
        assert_eq!(
            expand_template(r"${APP_DIR}\data", &now).unwrap(),
            format!(r"{want}\data")
        );
        // 无花括号的 `$APP_DIR` 只吃到 `$APP`（变量名扫描遇下划线即止）→ 未知 → None。
        // 目录变量一律要求花括号形式，与 CLI 侧写法一致。
        assert!(expand_template("$APP_DIR", &now).is_none());
    }

    #[test]
    fn test_escape_and_unsupported() {
        let now = fixed();
        assert_eq!(expand_template("$$5", &now).unwrap(), "$5");
        // 含不支持的模板变量 → None（注意 $AA( 是 cmdbar 字符组 marker，走 cmdbar 路径，
        // 不经此简单模板展开；这里用一个永不存在的变量名验证未知变量降级）。
        assert!(expand_template("$QQ", &now).is_none());
    }

    #[test]
    fn test_cmdbar_dual_path() {
        // 命令栏语法（含 {expr}）走 cmdbar；其余走简单模板。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "rq".into(),
            vec![PhraseEntry {
                text: r#"{date("YYYY-MM-DD")}"#.into(),
                weight: 1000,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        map.insert(
            "js".into(),
            vec![PhraseEntry {
                text: "{calc(\"1+2*3\")}".into(),
                weight: 900,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        map.insert(
            "old".into(),
            vec![PhraseEntry {
                text: "$Y-$MM-$DD".into(),
                weight: 800,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        // $CC 命令短语（有动作）：暂不显现（待动作执行通路），避免误上屏 display 标签。
        map.insert(
            "cmd".into(),
            vec![PhraseEntry {
                text: r#"$CC("切简繁", ime.toggle("s2t"))"#.into(),
                weight: 700,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let now = fixed();
        let rq = layer.lookup_at("rq", now, &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(rq.len(), 1);
        assert_eq!(rq[0].text, "2026-06-14");
        assert_eq!(rq[0].weight, 1000);
        let js = layer.lookup_at("js", now, &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(js.len(), 1);
        assert_eq!(js[0].text, "7");
        // 旧简单模板路径仍工作；source_text 保留未展开原文（右键禁用短语用）。
        let old = layer.lookup_at("old", now, &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].text, "2026-06-14");
        assert_eq!(old[0].source_text, "$Y-$MM-$DD");
        // 命令短语：display 为标签，携带命令源（选中时执行动作）。
        let cmd = layer.lookup_at("cmd", now, &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(cmd.len(), 1);
        assert_eq!(cmd[0].text, "切简繁");
        assert_eq!(
            cmd[0].command_src.as_deref(),
            Some(r#"$CC("切简繁", ime.toggle("s2t"))"#)
        );
    }

    /// ★ **用户词库**里写 `{dict.rev(clip())}` 的完整两种结局。
    ///
    /// 这条路径与短语层是两套代码（`expand_dict_value` vs `lookup_at`），
    /// 短语层测绿**证明不了**词库路径也对 —— 用户正是把词条加在词库里的。
    #[test]
    fn dict_value_clipboard_reverse_both_outcomes() {
        let clip = |_n: i64| "好".to_string();

        // 查得到 → 展开为反查结果，无 command_src（选中即上屏该文本）。
        let ok = |_t: &str, _f: &str| "好: vbg hǎo".to_string();
        let host = PhraseHost {
            clip: &clip,
            reverse: &ok,
        };
        assert_eq!(
            expand_dict_value("{dict.rev(clip())}", "fc", fixed(), &[], &host),
            DictExpansion::Single {
                display: "好: vbg hǎo".into(),
                command_src: None,
            }
        );

        // 查不到 → **绝不能**退回 `None`：词库路径的 `None` 语义是「这不是特殊语法，
        // 保留原文」，于是候选会显示成源码字面 `{dict.rev(clip())}` 并原样上屏，
        // 看起来就像「配了没生效」。必须是 Drop（该候选整条不出现）。
        let miss = |_t: &str, _f: &str| String::new();
        let host = PhraseHost {
            clip: &clip,
            reverse: &miss,
        };
        assert_eq!(
            expand_dict_value("{dict.rev(clip())}", "fc", fixed(), &[], &host),
            DictExpansion::Drop,
            "查不到时必须丢弃候选，而不是把模板源码当文本留下"
        );
    }

    #[test]
    fn test_expand_dict_value_reuse() {
        // 词库候选复用短语 / cmdbar 的统一展开（对齐 Go dict.ValueExpander）。
        let now = fixed();
        let clip = no_clip();
        // 简单模板变量 $Y年$M月$D日 → 展开为日期（用户 now 词条 bug 的正解）。
        assert_eq!(
            expand_dict_value("$Y年$M月$D日", "now", now, &[], &clip),
            DictExpansion::Single {
                display: "2026年6月14日".into(),
                command_src: None,
            }
        );
        // 花括号插值 {date()} → display，无命令源。
        assert_eq!(
            expand_dict_value(r#"{date("YYYY-MM-DD")}"#, "rq", now, &[], &clip),
            DictExpansion::Single {
                display: "2026-06-14".into(),
                command_src: None,
            }
        );
        // $CC 命令 → display + 命令源。
        assert_eq!(
            expand_dict_value(r#"$CC("切简繁", ime.toggle("s2t"))"#, "co", now, &[], &clip),
            DictExpansion::Single {
                display: "切简繁".into(),
                command_src: Some(r#"$CC("切简繁", ime.toggle("s2t"))"#.into()),
            }
        );
        // $AA 字符组 → Group（组名 + 成员）；由调用方按精确/前缀决定展开或折叠。
        assert_eq!(
            expand_dict_value(r#"$AA("数字", "①②③")"#, "sz", now, &[], &clip),
            DictExpansion::Group {
                name: "数字".into(),
                items: vec![("①".into(), None), ("②".into(), None), ("③".into(), None),],
            }
        );
        // 普通词库文本（无 $ 与 {）→ None，零干预。
        assert_eq!(
            expand_dict_value("你好", "nh", now, &[], &clip),
            DictExpansion::None
        );
        // 含 $ 但非模板变量（如 价格$5）→ None，不误展开。
        assert_eq!(
            expand_dict_value("价格$5", "jg", now, &[], &clip),
            DictExpansion::None
        );
    }

    /// 静态短语的前缀命中要带**剩余编码**注释，与 marker 短语一致。
    ///
    /// 真机现场：5 码短语 `zzsfz` 敲到 `zzsf` 时候选已出现，但看不出「还差一个 z」——
    /// 而同一串码下系统 `zz*` 的 `$SS` 分组是带提示的。`suffix` 早就算出来了，
    /// 只有 Literal/Template 两个分支没往 `PhraseHit` 上放。
    #[test]
    fn prefix_hit_carries_remaining_code_comment() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "zzsfz".into(),
            vec![PhraseEntry {
                text: "TEST".into(),
                weight: 1800,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        map.insert(
            "zzsfzab".into(),
            vec![PhraseEntry {
                text: r#"$SS("组", "甲")"#.into(),
                weight: 100,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };

        let hits = layer.lookup_prefix_at("zzsf", fixed(), &[], 2, &PhraseScope::ALL);
        let stat = hits
            .iter()
            .find(|h| h.text == "TEST")
            .expect("静态短语应出现在前缀命中里");
        assert_eq!(stat.comment, "z", "静态短语须带剩余编码");
        let marker = hits
            .iter()
            .find(|h| h.text == "组")
            .expect("marker 短语应出现在前缀命中里");
        assert_eq!(marker.comment, "zab", "marker 短语的既有行为不得变");

        // 精确命中（lookup）无剩余编码，不得凭空冒出注释。
        let exact = layer.lookup_at("zzsfz", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].comment, "", "精确路径不得带剩余编码注释");
    }

    #[test]
    fn test_cmdbar_array_phrase_expands() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "fh".into(),
            vec![PhraseEntry {
                text: r#"$SS("符号", "（）", "【】")"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_at("fh", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        let src = r#"$SS("符号", "（）", "【】")"#;
        assert_eq!(
            got,
            vec![
                PhraseHit::plain("（）".into(), 500).with_source(src),
                PhraseHit::plain("【】".into(), 500).with_source(src),
            ]
        );
    }

    #[test]
    fn test_cmdbar_aa_char_group_expands() {
        // $AA 字符组：逐字符炸开为多个上屏候选（镜像发货 system.phrases.toml 的符号组）。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "zzsz".into(),
            vec![PhraseEntry {
                text: r#"$AA("数字", "①②③")"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_at("zzsz", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        let src = r#"$AA("数字", "①②③")"#;
        assert_eq!(
            got,
            vec![
                PhraseHit::plain("①".into(), 500).with_source(src),
                PhraseHit::plain("②".into(), 500).with_source(src),
                PhraseHit::plain("③".into(), 500).with_source(src),
            ]
        );
    }

    #[test]
    fn test_prefix_nav_lists_matching_groups() {
        // 敲 zz → 列出 zzbd/zzsz 字符组导航候选（组名 + 码后缀），不含无关码。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "zzbd".into(),
            vec![PhraseEntry {
                text: r#"$AA("标点", "、。")"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        map.insert(
            "zzsz".into(),
            vec![PhraseEntry {
                text: r#"$AA("数字", "①②")"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        map.insert(
            "xx".into(),
            vec![PhraseEntry {
                text: "无关".into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_prefix_at("zz", fixed(), &[], 2, &PhraseScope::ALL);
        assert_eq!(got.len(), 2);
        // 同权重按完整码字母序 zzbd < zzsz。
        assert_eq!(got[0].text, "标点");
        assert_eq!(got[0].comment, "bd");
        assert_eq!(got[0].nav_code.as_deref(), Some("zzbd"));
        assert_eq!(got[1].text, "数字");
        assert_eq!(got[1].comment, "sz");
        assert_eq!(got[1].nav_code.as_deref(), Some("zzsz"));
    }

    #[test]
    fn test_prefix_nav_min_len_gate_and_exact_excluded() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "zzbd".into(),
            vec![PhraseEntry {
                text: r#"$AA("标点", "、。")"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        // 前缀长度 < min_len → 不触发。
        assert!(
            layer
                .lookup_prefix_at("z", fixed(), &[], 2, &PhraseScope::ALL)
                .is_empty()
        );
        // 精确码本身（== 完整码）不作为导航候选返回（只列更长的码）。
        assert!(
            layer
                .lookup_prefix_at("zzbd", fixed(), &[], 2, &PhraseScope::ALL)
                .is_empty()
        );
        // 真前缀 → 1 个导航候选。
        assert_eq!(
            layer
                .lookup_prefix_at("zz", fixed(), &[], 2, &PhraseScope::ALL)
                .len(),
            1
        );
    }

    #[test]
    fn test_prefix_nav_command_default_on_prefix_false_off() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "cobd".into(),
            vec![PhraseEntry {
                text: r#"$CC("百度", open("https://baidu.com"))"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        map.insert(
            "coex".into(),
            vec![PhraseEntry {
                text: r#"$CC("退出", type("x"), {prefix: false})"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_prefix_at("co", fixed(), &[], 2, &PhraseScope::ALL);
        // $CC 默认列出（百度），显式 {prefix: false} 退出列举（退出）。
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "百度");
        assert_eq!(got[0].comment, "bd");
        assert_eq!(got[0].nav_code.as_deref(), Some("cobd"));
        // 命令 nav：携命令源（选中**直接执行**，非二级展开）。
        assert!(got[0].command_src.is_some());
    }

    #[test]
    fn test_prefix_nav_command_display_skips_clipboard_read() {
        // 命令 display 含 {clip()}（如 coad）：列举用廉价上下文，clip() 返回空——
        // 不读整个剪贴板（内存安全），只显示静态部分。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "coad".into(),
            vec![PhraseEntry {
                text: r#"$CC("剪贴板加词:{clip()}", type("x"))"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_prefix_at("co", fixed(), &[], 2, &PhraseScope::ALL);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "剪贴板加词:");
        assert!(got[0].command_src.is_some());
    }

    #[test]
    fn test_prefix_nav_includes_literal_template() {
        // 静态/旧模板短语（无 marker）参与前缀列举时，$Y/$MM/$DD 等旧式变量
        // 应经 expand_template 展开（对齐 lookup 精确匹配路径的双路径策略）。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "rq".into(),
            vec![PhraseEntry {
                text: "$Y-$MM-$DD".into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_prefix_at("r", fixed(), &[], 1, &PhraseScope::ALL);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "2026-06-14");
        assert!(got[0].command_src.is_none());
        assert!(got[0].nav_code.is_none());
    }

    /// ★ 两条路径对同一条短语必须**同处置**——这是用户实际踩到的那个 bug。
    ///
    /// 前缀路径曾是 `unwrap_or_else(|| t.clone())`「展开失败退回原文」，于是打全码一个候选
    /// 都没有、打前缀反而看得见字面 `$LY$LZ$LMD$LF` 且选中即原样上屏。同一条短语在两条
    /// 路径上现象相反，是最难被用户描述清楚的一类故障。
    ///
    /// 用**写错的变量名**做样本而非 `$LF`：`$LF` 如今展开成空串、根本走不到失败分支，
    /// 拿它当样本这条测试会恒绿而压不住任何东西。
    #[test]
    fn test_prefix_and_exact_agree_on_broken_template() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "rq".into(),
            vec![PhraseEntry {
                text: "农历$NOPE".into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        // 前缀路径：不得出候选，更不得把字面 `$NOPE` 端上来
        let pre = layer.lookup_prefix_at("r", fixed(), &[], 1, &PhraseScope::ALL);
        assert!(
            pre.is_empty(),
            "展开失败的模板不该出现在前缀候选里，实得 {:?}",
            pre.iter().map(|h| &h.text).collect::<Vec<_>>()
        );
        // 精确路径：同样不出候选
        let exact = layer.lookup_at("rq", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert!(exact.is_empty(), "精确路径同样应跳过该条");
    }

    /// ★ 含 `$LF` 的短语在**非节日**要照常出候选，两条路径都是。
    ///
    /// 这是「一年 355 天打不出来」的正面覆盖：`$LF` 展开成空串而非毒杀整条。
    #[test]
    fn test_lunar_festival_phrase_survives_ordinary_days() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "nl".into(),
            vec![PhraseEntry {
                text: "$LY年$LMD$LF".into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        // fixed() = 2026-06-14，非节日
        let exact = layer.lookup_at("nl", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(exact.len(), 1, "非节日也该出候选");
        assert_eq!(exact[0].text, "丙午年四月廿九");

        let pre = layer.lookup_prefix_at("n", fixed(), &[], 1, &PhraseScope::ALL);
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].text, "丙午年四月廿九", "两条路径给出同一个答案");
    }

    #[test]
    fn test_cmdbar_command_display_uses_last() {
        // coll = $CC(last(), type(last()))：候选 display 应显示上一次上屏内容，并携命令源。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "coll".into(),
            vec![PhraseEntry {
                text: "$CC(last(), type(last()))".into(),
                weight: 2000,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let recent = vec!["上次内容".to_string()];
        let got = layer.lookup_at("coll", fixed(), &recent, &no_clip(), &PhraseScope::ALL);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "上次内容"); // display = last() 显示上次上屏
        assert!(got[0].command_src.is_some()); // 仍是命令（选中执行 type(last())）
    }

    #[test]
    fn from_records_builds_lookup() {
        let layer = PhraseLayer::from_records([
            PhraseSeed {
                code: "bj".into(),
                text: "北京".into(),
                weight: 1000,
                position: 0,
                is_system: false,
                category: String::new(),
            },
            PhraseSeed {
                code: "bj".into(),
                text: "北京市".into(),
                weight: 500,
                position: 1,
                is_system: true,
                category: String::new(),
            },
        ]);
        let hits = layer.lookup("bj", &[], &no_clip(), &PhraseScope::ALL);
        // 两条同码，按 weight 降序
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "北京");
        assert_eq!(hits[1].text, "北京市");
    }

    // 中文数字读法本身的用例已随实现迁往 wind-quick-input
    // （`test_small_int_to_chinese`）；此处保留 `test_expand_chinese` 覆盖模板端到端。

    /// 回归：`${VAR}` 旧式模板变量不得被 cmdbar 语法探测劫走。
    ///
    /// `has_top_level_brace` 曾只看 `{`、不看它前面的 `$`，于是 `${YC}年${MC}月${DC}日`
    /// 被判成命令栏语法 → 走 `evaluate` 求值 `YC` → 不在函数注册表 → `UnknownFunc`
    /// → 候选被静默丢弃（症状：`date`/`datm`/`zzrq` 的中文数字日期那条不再显示）。
    ///
    /// **必须从 `lookup_at` 进**：它才覆盖 `is_cmdbar_grammar` 分发。只测 `expand_template`
    /// 纯函数的用例（如 `test_expand_template`）绕过分发，当年正是它让这个缺陷假绿通过 CI。
    #[test]
    fn lookup_expands_brace_template_vars() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "date".into(),
            vec![PhraseEntry {
                text: "${YC}年${MC}月${DC}日".into(),
                weight: 100,
                position: 0,
                is_system: true,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };

        // 精确匹配路径
        let got = layer.lookup_at("date", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(got.len(), 1, "${{YC}} 模板短语不应被丢弃");
        assert_eq!(got[0].text, "二〇二六年六月十四日");

        // 前缀枚举路径（lookup_prefix 的 Literal 分支同样要展开）
        let got = layer.lookup_prefix_at("dat", fixed(), &[], 2, &PhraseScope::ALL);
        assert_eq!(got.len(), 1, "前缀枚举也不应丢弃 ${{YC}} 模板短语");
        assert_eq!(got[0].text, "二〇二六年六月十四日");
    }

    /// 回归：系统短语 `uuid = '$uuid'` 曾因 `expand_var` 没有 `uuid` 分支而整条被丢弃
    /// （打全码无候选），前缀路径却回退显示字面 `$uuid`——同一根因两个相反现象。
    ///
    /// **必须从 `lookup_at` 进**（同 `lookup_expands_brace_template_vars` 的理由）：
    /// 只测 `expand_template` 会绕过 `is_cmdbar_grammar` 分发。
    #[test]
    fn lookup_expands_uuid_variable() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "uuid".into(),
            vec![PhraseEntry {
                text: "$uuid".into(),
                weight: 1000,
                position: 0,
                is_system: true,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };

        // 精确匹配路径：必须出候选，且不是字面量。
        let got = layer.lookup_at("uuid", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(got.len(), 1, "$uuid 短语不应被丢弃");
        assert_ne!(got[0].text, "$uuid", "应展开而非原样上屏");
        assert_eq!(got[0].text.len(), 36, "默认格式为带横杠 UUID");
        assert_eq!(got[0].text.matches('-').count(), 4);

        // 前缀枚举路径：同样展开，不再回退成字面 `$uuid`。
        let got_prefix = layer.lookup_prefix_at("uui", fixed(), &[], 2, &PhraseScope::ALL);
        assert_eq!(got_prefix.len(), 1);
        assert_ne!(got_prefix[0].text, "$uuid");

        // 每次求值都是新值（区别于 date/time 的秒级稳定值）。
        let again = layer.lookup_at("uuid", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert_ne!(got[0].text, again[0].text, "uuid 应每次重新生成");
    }

    /// 真正的 cmdbar 插值 `{expr}` 不受上面的 `${` 豁免影响。
    #[test]
    fn lookup_still_evaluates_cmdbar_interpolation() {
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        map.insert(
            "yy".into(),
            vec![PhraseEntry {
                text: r#"年份{date("YYYY")}"#.into(),
                weight: 1,
                position: 0,
                is_system: true,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_at("yy", fixed(), &[], &no_clip(), &PhraseScope::ALL);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "年份2026");
    }

    #[test]
    fn lookup_prefix_lists_static_phrases() {
        // 静态字面短语（Literal）应出现在前缀结果中，command_src=None，nav_code=None。
        let mut map: HashMap<String, Vec<PhraseEntry>> = HashMap::new();
        // 静态短语：码 yx，文本 user@example.com
        map.insert(
            "yx".into(),
            vec![PhraseEntry {
                text: "user@example.com".into(),
                weight: 800,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        // $CC 命令短语：码 yxbd，保证命令短语仍正常工作
        map.insert(
            "yxbd".into(),
            vec![PhraseEntry {
                text: r#"$CC("百度", open("https://baidu.com"))"#.into(),
                weight: 500,
                position: 0,
                is_system: false,
                category: String::new(),
            }],
        );
        let layer = PhraseLayer { map };
        let got = layer.lookup_prefix_at("y", fixed(), &[], 1, &PhraseScope::ALL);
        // 静态短语应出现
        let static_hit = got.iter().find(|h| h.text == "user@example.com");
        assert!(static_hit.is_some(), "静态短语应出现在前缀结果中");
        let sh = static_hit.unwrap();
        assert!(sh.command_src.is_none(), "静态短语 command_src 应为 None");
        assert!(sh.nav_code.is_none(), "静态短语 nav_code 应为 None");
        // $CC 命令短语仍正常工作
        let cmd_hit = got.iter().find(|h| h.text == "百度");
        assert!(cmd_hit.is_some(), "$CC 命令短语应仍出现在前缀结果中");
        assert!(
            cmd_hit.unwrap().command_src.is_some(),
            "$CC 命令短语 command_src 应非 None"
        );
    }
}
