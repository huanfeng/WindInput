//! 短语系统：静态/动态短语模板展开 + 命令栏（cmdbar）双路径。
//!
//! 与 Go 版本 `wind_input/internal/dict/phrase.go` + `internal/cmdbar` 对齐。
//! 加载 `system.phrases.toml`，输入码命中短语 code 时生成候选。
//!
//! 双路径（对齐 Go design §7.2）：
//! - 短语 text 使用命令栏语法（含 `$CC(`/`$SS(`/`$AA(` marker 或顶层 `{expr}` 插值）→ 经
//!   `wind-cmdbar` 解析求值（`{date()}`/`{calc(code)}`/`{upper(code)}`/`$SS` 字符串组/`$AA` 字符组等）。
//! - 否则 → 旧的简单模板变量展开
//!   （$Y/$YYYY/$YY/$M/$MM/$D/$DD/$HH/$mm/$ss/$WC/$YC/$MC/$DC/$ts/$tsms）。
//!
//! 本 crate 只做 display 侧求值（宿主能力经 [`PhraseHost`] 回调注入）；`$CC` 的副作用动作
//! 由协调器在选中时执行（`handle_cmdbar.rs`）。

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
/// 它是必填参数而不是 `Option` 或另一族 `*_scoped` 方法：短语查询有六个消费点，漏接一处
/// （如 `phrase_owns_code`）会让候选消失而顶码仍被短语层否决，打字卡住且零日志。必填参数
/// 让新增查询方法也躲不过编译期检查。见 `docs/design/schema-scoped-behavior.md` §6.3。
#[derive(Debug, Clone, Copy)]
pub struct PhraseScope<'a> {
    /// 本方案是否加载短语。`false` ⇒ 所有查询直接短路。
    pub enabled: bool,
    /// 白名单。空 = 不施加这一项限制（全部分类），不是「一条都不要」——
    /// 「一条都不要」由 `enabled = false` 表达。空串 `""` 匹配未分类短语。
    pub categories: &'a [String],
    /// 黑名单。空 = 不排除；在白名单之后再减。
    pub exclude: &'a [String],
}

impl PhraseScope<'_> {
    /// 不施加任何限制。测试与「无方案上下文」的调用点用。
    ///
    /// 它仍然是显式写出来的决定，与「忘了过滤」在代码里长得不一样——这正是必填参数
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

    /// 是否完全不过滤（只判 enabled，不看分类）——全表扫描的快路径判据。
    fn admits_everything(&self) -> bool {
        self.enabled && self.categories.is_empty() && self.exclude.is_empty()
    }
}

/// 一条短语命中：展开后的候选文本 + 权重 + 可选命令源 / 前缀导航目标。
/// - `command_src` 非空 → 这是 `$CC` 命令短语（选中时执行动作而非上屏 text），
///   其值为待重新求值/执行的命令源（如 `$CC("切简繁", ime.toggle("s2t"))`）。
/// - `nav_code` 非空 → 这是前缀导航候选（敲 `zz`/`co` 列出的 `zzbd`/`coen` 等），
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
    /// `Some(组名)` = 这是 `$SS`/`$AA` 在精确码下炸开的成员（含嵌入 `$CC` 的命令成员）。
    /// 宿主据此判「同码是否还有别的候选」，有重码时把整组折回一条组名候选，而不是自动展开。
    pub group: Option<String>,
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
            group: None,
        }
    }

    /// 标为 `$SS`/`$AA` 精确码展开出的组成员（见 [`PhraseHit::group`]）。
    fn with_group(mut self, name: &str) -> Self {
        self.group = Some(name.to_string());
        self
    }

    /// 附上原始记录文本（构造点统一经此填充，测试用 `plain` 直构不填）。
    fn with_source(mut self, src: &str) -> Self {
        self.source_text = src.to_string();
        self
    }

    /// 附上注释（前缀命中时＝剩余编码）。
    ///
    /// 只在 [`PhraseLayer::lookup_prefix_at`] 用：精确路径没有剩余编码，填了就是假提示。
    /// 静态短语与 marker 短语（经 `nav`/`command_nav` 带上 `suffix`）须一致地显示剩余编码。
    fn with_comment(mut self, comment: String) -> Self {
        self.comment = comment;
        self
    }

    /// 前缀导航——组候选（`$SS`/`$AA`）：`code` 为补全目标完整码，选中后补全展开。
    fn nav(text: String, weight: i32, code: String, comment: String) -> Self {
        Self {
            text,
            weight,
            command_src: None,
            nav_code: Some(code),
            comment,
            source_text: String::new(),
            is_system: false,
            group: None,
        }
    }

    /// 前缀导航——命令候选（`$CC`）：选中后直接执行 `src`（不二级展开），
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
            group: None,
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
    /// 出厂是否启用（TOML 的 `enabled`，缺省 = 开）。关着的照样入库，用户能在短语管理里打开；
    /// 用于「少数人用、编码又和码表撞」的条目，如时间戳。
    pub enabled: bool,
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

/// 本进程所在平台在短语 `platform` 字段里的名字。
///
/// 曾经两处过滤都写死只收 `"windows"`，于是 macOS / Linux 上显示的是 Windows 那一份
/// （`notepad.exe`、`calc.exe`、`USERPROFILE`），而 `darwin` 条目从未生效过。
const CURRENT_PLATFORM: &str = if cfg!(windows) {
    "windows"
} else if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "android") {
    // 先于 linux 判：Android 的 target_os 是 "android"，但别处有按 unix 归 linux 的习惯
    "android"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "other"
};

/// 平台名归一：大小写不敏感；`"darwin"` 是 `"macos"` 的旧写法（文件里两种都可能有）。
fn canonical_platform(p: &str) -> String {
    let p = p.to_lowercase();
    if p == "darwin" { "macos".into() } else { p }
}

/// 条目的 `platform` 是否适用于 `current`：缺省 / 空 / `"all"` = 全平台；`"desktop"` =
/// 三种电脑（windows / macos / linux）；否则按名字比对（取值见 system.phrases.toml 文件头）。
///
/// `desktop` 让电脑专用的条目（打开网页、按键序列等）不必为了对手机隐藏而复制三份。
fn platform_matches(platform: Option<&str>, current: &str) -> bool {
    let Some(p) = platform else {
        return true;
    };
    let p = canonical_platform(p);
    let current = canonical_platform(current);
    match p.as_str() {
        "" | "all" => true,
        "desktop" => matches!(current.as_str(), "windows" | "macos" | "linux"),
        _ => p == current,
    }
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
    /// 出厂启用（缺省 = 开），见 [`SystemPhraseEntry::enabled`]。
    #[serde(default)]
    enabled: Option<bool>,
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
            if !platform_matches(r.platform.as_deref(), CURRENT_PLATFORM)
                || r.enabled == Some(false)
            {
                continue;
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
        Self::parse_system_entries_for(path, CURRENT_PLATFORM)
    }

    /// 同 [`Self::parse_system_entries`]，但按指定平台（`windows` / `macos` / `linux` / `android`）过滤。
    ///
    /// 供测试在任一主机上核对别的平台看到的条目集。
    pub fn parse_system_entries_for(
        path: &std::path::Path,
        platform: &str,
    ) -> Vec<SystemPhraseEntry> {
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
            if !platform_matches(r.platform.as_deref(), platform) {
                continue;
            }
            out.push(SystemPhraseEntry {
                code: r.code,
                text: r.text,
                weight: r.weight.unwrap_or(1000),
                position: r.position.unwrap_or(0),
                category: r.category.unwrap_or_default(),
                enabled: r.enabled.unwrap_or(true),
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
                    // 空展开不出空候选（与 `expand_template` 分支、`lookup_prefix_at` 同处置）：
                    // 求值型 display 落空是常态，如 `{dict.rev(clip(),2)}` 在剪贴板只有 1 个字时。
                    Ok(PhraseEval::Single {
                        display, actions, ..
                    }) if actions.is_empty() && !display.is_empty() => {
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
                        group: None,
                    }),
                    Ok(PhraseEval::Array(arr)) => {
                        for el in arr.elements {
                            // 空元素丢弃：`$SS` 的元素是运行时求值的，词条固定写 N 个
                            // 元素、各查第 1..N 个字时，剪贴板不足 N 字必然让尾部几条落空。
                            if el.display.is_empty() {
                                continue;
                            }
                            match (el.actions.is_empty(), el.src) {
                                // 带动作的嵌入 `$CC`：每个成员是独立的命令候选，携元素级
                                // 命令源（不是整组的源），选中时只执行这一个成员的动作链。
                                (false, Some(src)) => out.push(PhraseHit {
                                    text: el.display,
                                    weight: e.weight,
                                    command_src: Some(src),
                                    nav_code: None,
                                    comment: String::new(),
                                    source_text: e.text.clone(),
                                    is_system: false,
                                    group: Some(arr.name.clone()),
                                }),
                                _ => out.push(
                                    PhraseHit::plain(el.display, e.weight)
                                        .with_source(&e.text)
                                        .with_group(&arr.name),
                                ),
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
                // 短语文本按原样出候选：库里存的就是真实文本（含真换行），转义只在系统边界
                // 发生，见 `wind_store::store_layer::record_to_candidate`。全空展开（如非节日
                // 的 `$LF`）不出空候选，与 `lookup_prefix_at` 同处置。
                if !text.is_empty() {
                    out.push(PhraseHit::plain(text, e.weight).with_source(&e.text));
                }
            } else {
                // 含不支持的模板变量（写错变量名）→ 整条跳过，与 `lookup_prefix_at` 同处置。
                // 「变量取不到值」不走这里（`$LF` 非节日返回空串，见 `wind_quick_input::lunar::var`），
                // 故留 warn。warn 不得含词条明文（docs/logging-convention.md），源文降到 debug。
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

    /// 是否存在码恰为 `code` 的短语（不含更长后继，那是 [`Self::has_longer_code`]）。
    ///
    /// 供顶码上屏的「整串仍是精确匹配」判据补齐短语侧：引擎的 `has_full_input_match` 只问
    /// 码表，否则超过满码长的短语（如 4 码方案里的 5 码短语）会被顶掉，永远打不出来。
    pub fn has_exact_code(&self, code: &str, scope: &PhraseScope<'_>) -> bool {
        if code.is_empty() || scope.is_closed() {
            return false;
        }
        self.map
            .get(code)
            .is_some_and(|v| v.iter().any(|e| scope.admits(e)))
    }

    /// 前缀导航：敲 `code`（长度 ≥ `min_len`）时，列出所有码以 `code` 开头但更长的
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
        // 廉价上下文：clip/sel/app/title 返回空，避免列举时读整个剪贴板（如 coad 的
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
                // $SS/$AA → 组 nav（选中补全到码再展开成员，二级选择）；
                // $CC → 命令 nav（选中直接执行，不二级展开），display 经廉价上下文求值。
                match &phrase {
                    Phrase::Literal(t) => {
                        // 旧式简单模板（$Y/$M/$D 等）不含 cmdbar marker/插值，parse 后为 Literal。
                        // 展开失败 → 跳过该条，与 [`Self::lookup_at`] 同处置；别退回模板原文，
                        // 那会把带 `$` 语法的源文当候选上屏。
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
                        // 全空求值不出空候选，与 Literal 分支、`lookup_at` 同处置：依赖瞬时状态
                        // 的模板在廉价的 NavCtx 下必然求值为空（见 `NavCtx::clip`）。
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
    /// 是特殊语法，但求值结果为空 —— 该候选整条丢弃。
    ///
    /// 不能与 [`Self::None`]（「不是特殊语法，原文当文本用」）合并：求值成空时返回 `None`
    /// 会把模板源码字面（`{dict.rev(clip())}`）当候选上屏。对齐短语层 `lookup_at` 的处置。
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

/// 展开一条词库候选 value（对齐 Go `dict.ValueExpander.Expand`/`ExpandToCandidates`）：
/// 判定顺序——cmdbar marker（`$CC(`/`$SS(`/`$AA(` 或顶层 `{..}` 插值）优先，其次简单模板变量
/// （`$Y/$M/$D/...`），都不含则 [`DictExpansion::None`]。让普通用户 / 系统词库码表词条也能
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
            // 空展开 → Drop（整条丢弃），不是 None —— 见 `DictExpansion::Drop`。
            Ok(PhraseEval::Single {
                display, actions, ..
            }) if actions.is_empty() && display.is_empty() => DictExpansion::Drop,
            Ok(PhraseEval::Single {
                display, actions, ..
            }) if actions.is_empty() => DictExpansion::Single {
                display,
                command_src: None,
            },
            // $CC 命令（有动作）：携命令源，选中 / 顶屏执行动作。
            Ok(PhraseEval::Single { display, .. }) => DictExpansion::Single {
                display,
                command_src: Some(text.to_string()),
            },
            // $AA/$SS 组：炸开为多个候选。字面元素 → 上屏文本；带动作的嵌入 `$CC` →
            // 携元素级命令源（`items` 第二槽），选中时只执行该成员的动作链。
            Ok(PhraseEval::Array(arr)) => {
                let name = arr.name.clone();
                // 空元素同样丢弃，与 `lookup_at` 的数组分支同处置。
                let items: Vec<(String, Option<String>)> = arr
                    .elements
                    .into_iter()
                    .filter(|el| !el.display.is_empty())
                    .map(|el| {
                        let src = if el.actions.is_empty() { None } else { el.src };
                        (el.display, src)
                    })
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

/// 短语 display 求值所需的宿主能力回调束（剪贴板、反查）。
///
/// 做成结构体而不是形参（同 [`ProcSpawn`](wind_cmdbar::ProcSpawn)）：加字段时每个构造点都会
/// 编译失败，而协调器四条候选构建路径里漏传一个形参只会让该入口下功能静默缺失。
/// 本 crate 不依赖平台层，故一律以回调注入，不直接读剪贴板 / 词库。
pub struct PhraseHost<'a> {
    /// 剪贴板读取：`n==0/1` 取当前，`n>1` 取历史第 n 条。
    /// 供命令栏 display 侧的 `clip(n)` 使用（如 `coad` 的 `剪贴板加词:{clip()}` 标签）。
    ///
    /// 这条在每次按键的候选构建期都会被调用，实现方必须走非阻塞的缓存版读取
    /// （见 `HostServices::clipboard_get_text_cached` 的行为契约）。
    pub clip: &'a dyn Fn(i64) -> String,
    /// 反查渲染：`(待查文本, format 模板) -> 渲染结果`；查不到返回空串。
    /// 供 `dict.rev(...)` 使用。模板语法与候选注释段一致，渲染在宿主侧完成。
    pub reverse: &'a dyn Fn(&str, &str, usize) -> String,
}

impl PhraseHost<'static> {
    /// 全空宿主：剪贴板与反查都返回空串。供测试与无平台能力的 headless 路径。
    pub fn empty() -> Self {
        const CLIP: &dyn Fn(i64) -> String = &|_| String::new();
        const REVERSE: &dyn Fn(&str, &str, usize) -> String = &|_, _, _| String::new();
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
    fn reverse_lookup(&self, text: &str, format: &str, min_len: usize) -> String {
        (self.host.reverse)(text, format, min_len)
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

/// 前缀导航列举用的廉价求值上下文：`clip`/`sel`/`app`/`title` 一律返回空，
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
    fn reverse_lookup(&self, _text: &str, _format: &str, _min_len: usize) -> String {
        // 同 clip：列举阶段不查词库，否则前缀导航会把 N 次词库查询摊到按键线程上。
        // 代价是 `cofc` 这类反查命令前缀列举时求值为空被丢弃，只在打全码时出现；它们的
        // display 本就依赖剪贴板等瞬时状态，列举阶段给不出真值。
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
        // `$YYYY`（补零四位）/ `$YY`（后两位）与快捷输入格式表的 `year_var` 同名同义。
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
        // 农历变量：与快捷输入的日期候选共用同一份换算，这里绑当前时间。系统日期超出
        // 1900–2100 时换算不出 → None（整条跳过）；`$LF` 非节日返回空串而非 None，让
        // `$LY年$LMD$LF` 这类追加式写法平常日子照常出。分工见 `wind_quick_input::lunar::var`。
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
#[path = "lib_tests.rs"]
mod tests;
