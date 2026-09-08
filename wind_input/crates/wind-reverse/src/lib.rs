//! 候选反查（编码反查 / 拆字 / 拼音）
//!
//! 与 Go 版本 `wind_input/internal/tooltip/` 对齐（简化版）。
//! 为悬停候选提供"如何输入"的提示：五笔编码（拆字）+ 拼音读音。
//!
//! 数据源：
//! - 拆字/编码：主码表方案 `[engine.chaizi].db_path` 指向的拆字库（字\t字根\t编码），
//!   路径由调用方解析（用户方案目录优先，回落系统 data/schemas/）
//! - 拼音：`pinyin_map.txt`（pinyin-data 格式：`U+4E00: yī  # 一`，多音字逗号分隔）
//!   由 wind-tools `gen_pinyin` 从 mozillazg/pinyin-data 合并生成。

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// 悬停提示生成选项（对齐 Go `ui.tooltip.*` provider 开关）。
#[derive(Debug, Clone)]
pub struct TooltipOptions {
    /// 编码段（五笔编码）
    pub code: bool,
    /// 拼音段
    pub pinyin: bool,
    /// 拼音显示多音字所有读音（false 仅首音）
    pub heteronyms: bool,
    /// 每字最多显示读音数（0=不限）
    pub max_readings: usize,
    /// 拆字段（字根分解 [编码]）
    pub chaizi: bool,
}

impl Default for TooltipOptions {
    /// 与 Go 默认一致：编码+拼音(全读音)开，拆字关。
    fn default() -> Self {
        Self {
            code: true,
            pinyin: true,
            heteronyms: true,
            max_readings: 0,
            chaizi: false,
        }
    }
}

/// 反查表
#[derive(Default)]
pub struct ReverseLookup {
    /// 拆字表（字 → 字根/编码），可随主码表方案热重载（`reload_chaizi`）
    chaizi: ChaiziTable,
    /// 字 → 拼音读音（多音字按常用频率排序，最常用在前）
    pinyin: PinyinTable,
    /// 已挂载的注释库，**按优先级升序**（先挂载者优先）。每库一份，不合并 ——
    /// 见 [`CommentSource`]。可热重载（`reload_comments`）。
    comments: Vec<CommentSource>,
}

/// 一个已挂载的注释库。
///
/// # 为什么每库独立而不合并成一张表
///
/// 合并表的缓存键必然是「这一组文件的组合」：用户加挂一个库就要重建全部，两个方案挂了
/// 交集不同的库也无法共享。按**文件**缓存后，`.wcmt` 与源文件一一对应，加挂只建新的那份；
/// 多方案引用同一个库时经 `reader_pool` 复用同一份 mmap，映射不会翻倍。
///
/// 代价是查询要遍历 N 个库，但 N 是个位数、每库一次二分，且只在当前页候选上发生。
struct CommentSource {
    /// 源文件路径（**不是**缓存路径）：重载时用来认出「这个库我已经开着了」。
    src: std::path::PathBuf,
    /// 适用方案 id 白名单（`[[ui.comment_dicts]].schemas`）；**空 = 全部方案**。
    ///
    /// # ★ 为什么过滤在这里而不在挂载层
    ///
    /// 早先由 `sync_comment_dicts` 按活跃方案筛选**挂载集合**，于是每次切方案都要重挂
    /// mmap，而热路径成本几乎全在「读整份源文件算内容指纹」（百万条 ~39ms）。更要命的是
    /// 挂载层拿不到正确的语境：临英背后是硬编码的 `english` 方案，按 `active_schema_id`
    /// 筛的话 `schemas = ["english"]` 在五笔方案下永远挂不上。
    ///
    /// 下移到查询点后：挂载集合只跟**配置**走（不随方案抖动），白名单按**查询发生时**的
    /// 语境求值，临英/overlay 一并正确。mmap 的常驻内存与库大小无关，挂载全集的代价只是
    /// 启动时每库一次指纹校验——一次性，且不在按键路径上。
    ///
    /// ⇒ ★ 与既有判据同构：**过滤要尽量靠近消费点**。模板里不写 `${dict}` 就根本不调用
    /// `comment_of`（零开销），任何挂载层的过滤都做不到这一点。
    schemas: Vec<String>,
    body: CommentBody,
}

enum CommentBody {
    /// 正常路径：mmap `.wcmt` 缓存，常驻内存与库大小基本无关。
    Mmap(std::sync::Arc<wind_dict::commentdict::CommentReader>),
    /// 降级路径：缓存目录不可写 / 构建失败时，直接把解析结果留在内存。
    ///
    /// 没有它的话，只读安装 + 缓存目录异常会让注释功能**整个消失**，且表现为「配了没反应」
    /// 这种最难自查的样子。降级只影响内存占用，不影响正确性。
    Memory(CommentTable),
}

impl CommentSource {
    /// 本库是否适用于给定方案。空白名单 = 全部；`schema` 为空串（语境未知）时一律放行。
    ///
    /// 空串放行而非拒绝，与 `CommentDictSpec::applies_to` 的「留空=全部」同一取舍：
    /// 「到处都显示」比「哪都不显示」好查得多。
    fn applies_to(&self, schema: &str) -> bool {
        self.schemas.is_empty() || schema.is_empty() || self.schemas.iter().any(|s| s == schema)
    }
    fn lookup_by_code(&self, text: &str, code: &str) -> Option<&str> {
        match &self.body {
            CommentBody::Mmap(r) => r.lookup_by_code(text, code),
            CommentBody::Memory(t) => t.lookup_by_code(text, code),
        }
    }
    fn lookup_first(&self, text: &str) -> Option<&str> {
        match &self.body {
            CommentBody::Mmap(r) => r.lookup_first(text),
            CommentBody::Memory(t) => t.lookup_first(text),
        }
    }
    fn len(&self) -> usize {
        match &self.body {
            CommentBody::Mmap(r) => r.entry_count() as usize,
            CommentBody::Memory(t) => t.len(),
        }
    }
}

/// 注释表的**内存**形态（仅用于 [`CommentSource::Memory`] 降级路径）。
///
/// 与 [`ChaiziTable`] 同构的紧凑存储（排序数组 + 共享 arena + 二分），但**键是词而非字**
/// ——注释要标注的是「这个词是什么」，不是逐字属性。正常路径走 `.wcmt` mmap，见
/// [`wind_dict::commentdict`]；两者的查找语义必须保持一致，故本表的
/// `lookup_by_code` / `lookup_first` 与那边同名同义。
///
/// # 条目布局
///
/// 每条在 arena 里连续存 `text | comment | code` 三段，各自长度记在条目里。
/// **不沿用 ChaiziTable「start = 前一条的 end」那套**：那要求算 start 时能拿到前一条，
/// 而 `binary_search_by` 的闭包只给到 `&Entry`，够不着前一条。故改存绝对偏移，
/// 多 4 字节/条（十万条 ≈ 0.4MB）换取二分可行。
#[derive(Default)]
struct CommentTable {
    /// 按 `text` 升序；同 `text` 的多条相邻（供 code 消歧），组内保持挂载顺序。
    entries: Vec<CommentEntry>,
    /// 所有 text/comment/code 文本按条目序连续拼接。
    arena: String,
}

struct CommentEntry {
    /// 本条三段文本在 arena 中的起点。
    off: u32,
    text_len: u32,
    comment_len: u32,
    /// 该条目所属方案的编码；`0` = 无 code 列（通用条目，任何方案都匹配）。
    code_len: u32,
}

impl CommentEntry {
    fn text_end(&self) -> usize {
        (self.off + self.text_len) as usize
    }
    fn comment_end(&self) -> usize {
        self.text_end() + self.comment_len as usize
    }
    fn code_end(&self) -> usize {
        self.comment_end() + self.code_len as usize
    }
}

impl CommentTable {
    fn text_of<'a>(&'a self, e: &CommentEntry) -> &'a str {
        &self.arena[e.off as usize..e.text_end()]
    }
    fn comment_of<'a>(&'a self, e: &CommentEntry) -> &'a str {
        &self.arena[e.text_end()..e.comment_end()]
    }
    fn code_of<'a>(&'a self, e: &CommentEntry) -> &'a str {
        &self.arena[e.comment_end()..e.code_end()]
    }

    /// 从 `(词, 注释, 编码)` 行构建。**挂载顺序即优先级**：同 (词, 编码) 重复时保留**首次**
    /// 出现的那条，于是先挂载的库覆盖后挂载的。
    ///
    /// 排序用 `sort_by`（稳定），组内因此保持挂载顺序 —— 这正是「先到先得」得以成立的前提，
    /// 换成 `sort_unstable_by` 同 text 条目的相对顺序就不再有保证，优先级会随输入规模抖动。
    fn build(mut rows: Vec<(String, String, String)>) -> Self {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let mut entries = Vec::with_capacity(rows.len());
        let mut arena =
            String::with_capacity(rows.iter().map(|r| r.0.len() + r.1.len() + r.2.len()).sum());
        // 同 (text, code) 去重：只在相邻的同 text 组内比对 code，无需全局 HashSet。
        let mut i = 0usize;
        while i < rows.len() {
            let mut j = i;
            while j < rows.len() && rows[j].0 == rows[i].0 {
                // 本组内是否已有同 code 的条目（含都无 code 的情形）。
                let dup = entries[entries.len() - (j - i)..]
                    .iter()
                    .any(|e: &CommentEntry| self_code_eq(&arena, e, &rows[j].2));
                if !dup {
                    let off = arena.len() as u32;
                    arena.push_str(&rows[j].0);
                    arena.push_str(&rows[j].1);
                    arena.push_str(&rows[j].2);
                    entries.push(CommentEntry {
                        off,
                        text_len: rows[j].0.len() as u32,
                        comment_len: rows[j].1.len() as u32,
                        code_len: rows[j].2.len() as u32,
                    });
                }
                j += 1;
            }
            i = j;
        }
        Self { entries, arena }
    }

    /// 该词对应的连续条目组（按挂载顺序）。
    fn group(&self, text: &str) -> impl Iterator<Item = &CommentEntry> {
        let lo = self.entries.partition_point(|e| self.text_of(e) < text);
        self.entries[lo..]
            .iter()
            .take_while(move |e| self.text_of(e) == text)
    }

    /// 该词在本库的首条注释。空注释视为未命中。
    fn lookup_first(&self, text: &str) -> Option<&str> {
        self.group(text)
            .map(|e| self.comment_of(e))
            .find(|c| !c.is_empty())
    }

    /// 该词中 `code` **精确匹配**的注释。用于方案内消歧。
    ///
    /// 对不上时返回 None 由调用方回落 `lookup_first` —— 注释库里的 `tfhh` 是五笔码，
    /// 拿拼音候选的 `hang` 去比对必然不匹配，而跨方案挂同一份注释库是常态，
    /// 那里不该因为对不上 code 就什么都不显示。
    fn lookup_by_code(&self, text: &str, code: &str) -> Option<&str> {
        if code.is_empty() {
            return None;
        }
        self.group(text)
            .find(|e| self.code_of(e) == code)
            .map(|e| self.comment_of(e))
            .filter(|c| !c.is_empty())
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// `build` 内部去重用：比对已入 arena 的条目 code 与待入行的 code。
/// 独立成函数是因为 `build` 里 `arena` 已被可变借用，走不了 `&self` 方法。
fn self_code_eq(arena: &str, e: &CommentEntry, code: &str) -> bool {
    &arena[e.comment_end()..e.code_end()] == code
}

/// 拆字表：字 → (字根, 编码)。紧凑存储——按字升序的定长条目数组 + 共享文本 arena，
/// 相比每字两个 `HashMap<char, String>`（桶空位 + 逐串堆分配），十万字级词库省数倍内存。
/// 查询走二分：拆字仅用于悬停提示与加词出码，均为低频路径，无需 O(1)。
#[derive(Default)]
struct ChaiziTable {
    /// 按 `ch` 升序。条目文本区间：字根=[start, rad_end)、编码=[rad_end, code_end)，
    /// start = 前一条目的 code_end（首条为 0）。
    entries: Vec<ChaiziEntry>,
    /// 所有字根/编码文本按条目序连续拼接。
    arena: String,
}

struct ChaiziEntry {
    ch: char,
    rad_end: u32,
    code_end: u32,
}

impl ChaiziTable {
    /// 从 (字, 字根, 编码) 行构建；同字多行取靠后者（与旧 HashMap 覆盖语义一致）。
    fn build(mut rows: Vec<(char, &str, &str)>) -> Self {
        rows.sort_by_key(|r| r.0); // 稳定排序保文件序，取每组末条即"后行覆盖"
        let mut entries = Vec::with_capacity(rows.len());
        let mut arena = String::with_capacity(rows.iter().map(|r| r.1.len() + r.2.len()).sum());
        let mut i = 0;
        while i < rows.len() {
            let mut j = i;
            while j + 1 < rows.len() && rows[j + 1].0 == rows[i].0 {
                j += 1;
            }
            let (ch, rad, code) = rows[j];
            arena.push_str(rad);
            let rad_end = arena.len() as u32;
            arena.push_str(code);
            entries.push(ChaiziEntry {
                ch,
                rad_end,
                code_end: arena.len() as u32,
            });
            i = j + 1;
        }
        arena.shrink_to_fit();
        Self { entries, arena }
    }

    /// 二分查字，返回 (字根, 编码)（可为空串）。
    fn lookup(&self, c: char) -> Option<(&str, &str)> {
        let i = self.entries.binary_search_by_key(&c, |e| e.ch).ok()?;
        let start = if i == 0 {
            0
        } else {
            self.entries[i - 1].code_end as usize
        };
        let e = &self.entries[i];
        Some((
            &self.arena[start..e.rad_end as usize],
            &self.arena[e.rad_end as usize..e.code_end as usize],
        ))
    }

    fn radicals(&self, c: char) -> Option<&str> {
        self.lookup(c).map(|(r, _)| r).filter(|s| !s.is_empty())
    }

    fn code(&self, c: char) -> Option<&str> {
        self.lookup(c).map(|(_, c)| c).filter(|s| !s.is_empty())
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 拼音表：字 → 读音列表（多音字按常用频率排序，最常用在前）。与 `ChaiziTable` 同构的紧凑
/// 存储——按字升序的定长条目数组 + 共享文本 arena，相比 `HashMap<char, Vec<String>>`
/// （桶空位 + 每字一个 Vec + 每读音一次堆分配）省数倍内存。读音是变长列表，故比拆字表多一级
/// `reading_ends` 下标。查询走二分：拼音仅用于悬停提示与加词出码，均为低频路径，无需 O(1)。
#[derive(Default)]
struct PinyinTable {
    /// 按 `ch` 升序。本条读音在 `reading_ends` 中的下标区间
    /// = [前一条目的 `reading_end`, 本条 `reading_end`)，首条起点为 0。
    entries: Vec<PinyinEntry>,
    /// 每个读音在 `arena` 中的结束偏移，按条目序连续。
    /// 单条读音的文本区间 = [前一项, 本项)，首项起点为 0。
    reading_ends: Vec<u32>,
    /// 所有读音文本按序连续拼接。
    arena: String,
}

struct PinyinEntry {
    ch: char,
    reading_end: u32,
}

/// 某字的读音列表视图：按需从 `arena` 切片，取用不分配。
#[derive(Clone, Copy)]
struct PinyinReadings<'a> {
    table: &'a PinyinTable,
    /// `reading_ends` 下标区间 [start, end)
    start: usize,
    end: usize,
}

impl<'a> PinyinReadings<'a> {
    fn len(&self) -> usize {
        self.end - self.start
    }

    /// 最常用读音（首项）。
    fn first(&self) -> Option<&'a str> {
        (self.start < self.end).then(|| self.table.reading_at(self.start))
    }

    /// 按序遍历读音（最常用在前）。
    fn iter(self) -> impl Iterator<Item = &'a str> {
        (self.start..self.end).map(move |i| self.table.reading_at(i))
    }
}

impl PinyinTable {
    /// 从 (字, 读音列表) 行构建；同字多行取靠后者（与旧 HashMap 覆盖语义一致）。
    fn build(mut rows: Vec<(char, Vec<&str>)>) -> Self {
        rows.sort_by_key(|r| r.0); // 稳定排序保文件序，取每组末条即"后行覆盖"
        let mut entries = Vec::with_capacity(rows.len());
        let mut reading_ends = Vec::with_capacity(rows.iter().map(|r| r.1.len()).sum());
        let mut arena =
            String::with_capacity(rows.iter().flat_map(|r| &r.1).map(|s| s.len()).sum());
        let mut i = 0;
        while i < rows.len() {
            let mut j = i;
            while j + 1 < rows.len() && rows[j + 1].0 == rows[i].0 {
                j += 1;
            }
            for r in &rows[j].1 {
                arena.push_str(r);
                reading_ends.push(arena.len() as u32);
            }
            entries.push(PinyinEntry {
                ch: rows[j].0,
                reading_end: reading_ends.len() as u32,
            });
            i = j + 1;
        }
        entries.shrink_to_fit();
        reading_ends.shrink_to_fit();
        arena.shrink_to_fit();
        Self {
            entries,
            reading_ends,
            arena,
        }
    }

    /// 二分查字，返回读音列表视图。
    fn readings(&self, c: char) -> Option<PinyinReadings<'_>> {
        let i = self.entries.binary_search_by_key(&c, |e| e.ch).ok()?;
        let start = if i == 0 {
            0
        } else {
            self.entries[i - 1].reading_end as usize
        };
        Some(PinyinReadings {
            table: self,
            start,
            end: self.entries[i].reading_end as usize,
        })
    }

    /// 按 `reading_ends` 全局下标取单条读音文本。
    fn reading_at(&self, i: usize) -> &str {
        let start = if i == 0 {
            0
        } else {
            self.reading_ends[i - 1] as usize
        };
        &self.arena[start..self.reading_ends[i] as usize]
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── 内部 Section 结构（对齐 Go tooltip.Section）──────────────────────────────

struct Section {
    label: String,
    lines: Vec<String>,
    /// 强制多行展开格式（即使只有 1 行内容）
    always_expand: bool,
}

/// 格式化 sections → 最终文本（对齐 Go `FormatContent`）。
/// 单行 section：`标签: 内容`；多行或 always_expand：`[标签]` + 逐行。
fn format_sections(sections: Vec<Section>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for sec in sections {
        if sec.lines.is_empty() {
            continue;
        }
        if sec.lines.len() == 1 && !sec.always_expand {
            let line = sec.lines.into_iter().next().unwrap();
            if sec.label.is_empty() {
                parts.push(line);
            } else {
                parts.push(format!("{}: {}", sec.label, line));
            }
        } else {
            if !sec.label.is_empty() {
                parts.push(format!("[{}]", sec.label));
            }
            parts.extend(sec.lines);
        }
    }
    parts.join("\n")
}

/// 当 sections 中同时包含"拆字"和"拼音"时，按字合并为"拆字 / 拼音" section。
/// 合并行格式：`<拆字行>\t<拼音读音>`（渲染层可按 \t 做列对齐）。
/// 对齐 Go `tooltip.MergeChaiziPinyin`。
fn merge_chaizi_pinyin(sections: Vec<Section>) -> Vec<Section> {
    let ci = sections.iter().position(|s| s.label == "拆字");
    let pi = sections.iter().position(|s| s.label == "拼音");
    let (Some(ci), Some(pi)) = (ci, pi) else {
        return sections;
    };

    // 建拼音 map：rune → 读音（剥离"字："前缀，避免合并行重复出现汉字）
    const FULL_COLON: char = '：';
    let flen = FULL_COLON.len_utf8();
    let mut pin_map: HashMap<char, String> = HashMap::new();
    let mut pin_full: HashMap<char, String> = HashMap::new();
    let mut pin_order: Vec<char> = Vec::new();
    for line in &sections[pi].lines {
        if let Some(head) = line.chars().next() {
            pin_full.insert(head, line.clone());
            let reading = line
                .find(FULL_COLON)
                .map(|i| line[i + flen..].to_string())
                .unwrap_or_else(|| line.clone());
            pin_map.insert(head, reading);
            pin_order.push(head);
        }
    }

    // 合并拆字行 + 拼音读音（\t 分隔）
    let mut used: HashSet<char> = HashSet::new();
    let mut merged: Vec<String> = Vec::new();
    for cz in &sections[ci].lines {
        match cz.chars().next() {
            Some(h) if pin_map.contains_key(&h) => {
                used.insert(h);
                merged.push(format!("{}\t{}", cz, pin_map[&h]));
            }
            _ => merged.push(cz.clone()),
        }
    }
    // 拼音独有字（拆字库未收录）补在末尾，保留"字：读音"完整格式
    for &r in &pin_order {
        if !used.contains(&r) {
            merged.push(pin_full[&r].clone());
        }
    }

    let combined = Section {
        label: "拆字 / 拼音".to_string(),
        lines: merged,
        always_expand: true,
    };
    // 用合并 section 替换拆字位置，删除拼音 section
    let mut combined_opt = Some(combined);
    let mut out: Vec<Section> = Vec::with_capacity(sections.len() - 1);
    for (i, s) in sections.into_iter().enumerate() {
        if i == pi {
            // skip 拼音 section
        } else if i == ci {
            out.push(combined_opt.take().unwrap());
        } else {
            out.push(s);
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────

/// 解析注释词库（rime `.dict.yaml` 形态：YAML 头 + `...` + TSV 正文）→ `(词, 注释, 编码)`。
///
/// 列序由头部 `columns:` 声明，**未声明时默认 `[text, comment]`**。合法列名 `text` /
/// `comment` / `code`；其余列名占位但不读（与 librime 对未知列的处理一致）。缺 `text` 或
/// 缺 `comment` 的声明整库跳过 —— 没有注释的注释库是配置错误，静默当空会让人以为是路径问题。
///
/// # 为什么不复用 `wind_dict::codetable` 的 rime 解析
///
/// 那个解析器背着 `PARSE_SEMANTICS_VERSION`：它一动，**全部主词库的 wdat 缓存失效并重建**
/// （300MB 级）。给它加一个只有注释库用得上的 `comment` 列，等于让每个用户为一个他可能
/// 没启用的功能付一次全量重建。而注释库要的只是这套格式的一个子集 —— 不需要 weight、
/// boundary、简拼、`# no comment` 指令、并行分块，那些正是那个文件复杂度的来源。
///
/// 保持格式**兼容**（用户能拿 rime 形态的文件直接用）与共用**实现**是两件事，这里只要前者。
fn parse_comment_dict(path: &std::path::Path) -> std::io::Result<Vec<(String, String, String)>> {
    let content = std::fs::read_to_string(path)?;
    // 正文起点：首个独占一行的 `...` 之后。无该行 → 整个文件都是正文（容许无 YAML 头的裸表）。
    let (header, body) = match content.find("\n...") {
        Some(i) if content[i + 1..].starts_with("...") => {
            let rest = &content[i + 4..];
            let body = rest.strip_prefix("\r").unwrap_or(rest);
            (&content[..i], body.strip_prefix('\n').unwrap_or(body))
        }
        _ if content.starts_with("...") => (&content[..0], &content[3..]),
        _ => (&content[..0], content.as_str()),
    };
    let Some((text_col, comment_col, code_col)) = comment_columns(header) else {
        tracing::warn!(
            "注释库 {} 的 columns 声明缺 text 或 comment，整库跳过",
            path.display()
        );
        return Ok(Vec::new());
    };
    // 只要求 text/comment 两列到位，**不把 code 列算进来**：声明了 code 列的库里，没有
    // 编码的行通常直接写成两列（或写成 `text\tcomment\t`，行尾 tab 又会被 trim_end 剥掉）。
    // 把 code 计入 need 会让这些行整行消失，表现为「库里明明有这个词却没注释」。
    let need = text_col.max(comment_col) + 1;
    let mut out = Vec::new();
    for line in body.lines() {
        // 只剥行尾：词条本身可能以 U+3000 之类开头（同 codetable 的 trim_line_end 决定）。
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < need {
            continue;
        }
        let text = parts[text_col];
        let comment = parts[comment_col];
        if text.is_empty() || comment.is_empty() {
            continue;
        }
        let code = code_col.and_then(|i| parts.get(i)).copied().unwrap_or("");
        out.push((text.to_string(), comment.to_string(), code.to_string()));
    }
    Ok(out)
}

/// 精确查不到时依次重试的大小写变形（全小写 → 首字母大写 → 全大写，去掉与原文相同者）。
///
/// **必须双向**：英文注释库里既有小写词条（`apple`）也有大写缩写（`ABC`、`AAP`），
/// 所以既要 `Apple` → `apple`，也要 `abc` → `ABC`。只做 `to_lowercase()` 会解决一半、
/// 漏掉另一半，而漏掉的那半表现为「有些词就是没注释」——最难自查的那种。
///
/// 无 ASCII 字母（中文候选是绝大多数）直接返回空表：变形对它们恒等，白算三次
/// `to_lowercase` 还各要一次分配。中文输入是主路径，这条守卫不是可选优化。
///
/// # 为什么不复用 `wind_coordinator::en_case_variants`
///
/// 产出形状一样，语义域不同：那个是给用户**选**的候选形态（故意剔除原文，因为原文自身
/// 已是首候选）；这里是找同一个词在库里的**别的写法**。二者将来会各自演化——比如这里
/// 可能要加 `US`→`U.S.` 之类的规范化，而那边不该跟着变。本 crate 也不依赖 coordinator。
fn case_fallbacks(text: &str) -> Vec<String> {
    if !text.bytes().any(|b| b.is_ascii_alphabetic()) {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let mut chars = lower.chars();
    let title = match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    let mut out = Vec::with_capacity(3);
    for v in [lower, title, text.to_uppercase()] {
        if v != text && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// 源文件 → 缓存路径：`<cache>/<相对 schemas 的目录链>/<文件干>.wcmt`。
///
/// 与 `wind_engine::manager::cache_path` **同源**：命名空间与文件干都取自
/// [`wind_dict::cache_ns`]，不再是「各写一份、注释里声明同构」——那正是两套命名法
/// 分叉的起点(词库那边已因二级子目录丢掉方案段，见论坛 #115)。挂在 `schemas/comments/`
/// 下的库仍落到 `<cache>/comments/`。
///
/// # 为什么不用路径哈希
///
/// 初版是 `<文件干>.<路径哈希>.wcmt`，为的是让「用户目录版」与「安装目录版」同名文件
/// 各持一份缓存。但那是自造的第二套命名法，而词库那边早用 schemas 下的目录链解决了同一问题。
/// 更关键的是，同一时刻 `resolve_data_file` 只会解析出其中**一个**路径，两份缓存里
/// 必有一份是孤儿——共用一份、靠内容指纹判定重建，反而连孤儿都不会产生。
fn comment_cache_path(cache_root: &Path, src: &Path) -> std::path::PathBuf {
    let mut stem = wind_dict::cache_ns::cache_stem(src);
    if stem.is_empty() {
        stem = "comment".to_string(); // 取不出文件干（路径以 `..` 结尾之类）时的兜底名
    }
    wind_dict::cache_ns::cache_path_in(cache_root, src, &format!("{stem}.wcmt"))
}

/// 加载一个注释库：优先 mmap `.wcmt` 缓存，缓存不新鲜则重建，重建失败降级内存表。
///
/// 源文件读不出来（路径错、无权限）返回 `None` —— 那是配置问题，应当跳过并告警，
/// 而不是降级成一个空表让人以为「库里没这个词」。
fn load_comment_source(
    src: &Path,
    schemas: &[String],
    cache_dir: Option<&Path>,
) -> Option<CommentSource> {
    use wind_dict::{cache_fp, commentdict, reader_pool};

    let wrap = |body| {
        Some(CommentSource {
            src: src.to_path_buf(),
            schemas: schemas.to_vec(),
            body,
        })
    };
    let Some(dir) = cache_dir else {
        let rows = parse_or_warn(src)?;
        return wrap(CommentBody::Memory(CommentTable::build(rows)));
    };
    let cache_file = comment_cache_path(dir, src);

    // single-flight：同一缓存文件的构建区间互斥。注释库虽只由协调器串行加载，但同一
    // 文件可能同时被别处（如设置页预览）打开，沿用词库那套锁不额外花什么代价。
    let lock = reader_pool::file_lock(&cache_file);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    if cache_fp::cache_is_fresh(&cache_file, &[src], cache_fp::COMMENT_TAG)
        && let Ok(r) = reader_pool::open_comment(&cache_file)
    {
        return wrap(CommentBody::Mmap(r));
    }

    let rows = parse_or_warn(src)?;
    match commentdict::write_comment_wcmt(&cache_file, &rows) {
        Ok(()) => {
            cache_fp::write_cache_fp(&cache_file, &[src], cache_fp::COMMENT_TAG);
            match reader_pool::open_comment(&cache_file) {
                Ok(r) => return wrap(CommentBody::Mmap(r)),
                Err(e) => tracing::warn!("注释库缓存写成但打不开 {}: {}", cache_file.display(), e),
            }
        }
        Err(e) => tracing::warn!("注释库缓存构建失败 {}: {}", cache_file.display(), e),
    }
    // 缓存这条路走不通（目录只读、磁盘满、文件被占）——功能继续，只是这一库常驻内存。
    wrap(CommentBody::Memory(CommentTable::build(rows)))
}

fn parse_or_warn(src: &Path) -> Option<Vec<(String, String, String)>> {
    match parse_comment_dict(src) {
        Ok(rows) => Some(rows),
        Err(e) => {
            tracing::warn!("读取注释库失败 {}: {}", src.display(), e);
            None
        }
    }
}

/// 清掉挂载列表里已不存在的库留下的 `.wcmt`（含指纹 sidecar 与残留 tmp）。
///
/// 缓存按 schemas 下的目录链分了命名空间（见 [`comment_cache_path`]），故要**逐层下探**——但只认
/// 这三种后缀，绝不删目录本身：缓存根同时住着词库的 `.wdat`/`.wdb`，扫到它们必须原样放过。
/// 「只删自己认识的文件」比「这个目录归我管所以可以递归删」安全一个量级。
///
/// 正被本进程映射的文件删不掉（Windows），失败即跳过，下次再清。
fn prune_comment_cache(cache_root: &Path, specs: &[(std::path::PathBuf, Vec<String>)]) {
    let keep: HashSet<std::path::PathBuf> = specs
        .iter()
        .map(|(p, _)| comment_cache_path(cache_root, p))
        .collect();
    let mut stack = vec![cache_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // `x.wcmt` / `x.wcmt.fp` / `x.wcmt.tmp` 都归到主名 `x.wcmt` 上判定
            let base = name
                .strip_suffix(".fp")
                .or_else(|| name.strip_suffix(".tmp"))
                .unwrap_or(&name);
            if !base.ends_with(".wcmt") {
                continue;
            }
            if keep.contains(&dir.join(base)) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                tracing::info!("清理已移除注释库的缓存：{}", name);
            }
        }
    }
}

/// 从 YAML 头解析注释库的列位置 → `(text, comment, code)`。
/// 无 `columns:` 声明时取默认 `[text, comment]`；声明里缺 text 或 comment 返回 `None`。
fn comment_columns(header: &str) -> Option<(usize, usize, Option<usize>)> {
    let mut in_columns = false;
    let mut names: Vec<String> = Vec::new();
    for raw in header.lines() {
        // 剥行内注释（`columns: [text, comment]  # 说明`）。
        let line = raw.split('#').next().unwrap_or("");
        let trimmed = line.trim();
        if !in_columns {
            let Some(rest) = trimmed.strip_prefix("columns:") else {
                continue;
            };
            // 顶格的 columns: 才是块起点（缩进的同名键属于别的映射）。
            if line.starts_with([' ', '\t']) {
                continue;
            }
            in_columns = true;
            // 流式：`columns: [text, comment]`
            if let Some(inner) = rest
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
            {
                names.extend(
                    inner
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                );
                break;
            }
            continue;
        }
        let Some(item) = trimmed.strip_prefix('-') else {
            if trimmed.is_empty() {
                continue;
            }
            break; // 回到非缩进键 → columns 块结束
        };
        names.push(item.trim().to_string());
    }
    if !in_columns {
        return Some((0, 1, None)); // 默认列序
    }
    let find = |k: &str| names.iter().position(|n| n == k);
    Some((find("text")?, find("comment")?, find("code")))
}

impl ReverseLookup {
    /// 加载反查表：两份资源的路径**都**由调用方解析后传入（无则跳过）——拆字库来自方案
    /// `[engine.chaizi].db_path`，拼音读音表 `pinyin_map.txt` 来自数据根。
    ///
    /// 之所以不在这里拼 `data_dir/pinyin_map.txt`：那样就绕过了「用户目录同名文件优先」
    /// 的解析，用户放的覆盖版永远不生效。本 crate 不依赖 wind-config，解析职责一律上提。
    pub fn load(pinyin_map: Option<&Path>, chaizi_path: Option<&Path>) -> Self {
        let mut rl = Self::default();
        if let Some(p) = chaizi_path {
            rl.load_chaizi(p);
        }
        if let Some(p) = pinyin_map {
            rl.load_pinyin(p);
        }
        rl
    }

    pub fn is_empty(&self) -> bool {
        self.chaizi.is_empty() && self.pinyin.is_empty() && self.comments.is_empty()
    }

    /// 重载注释库（挂载列表变更 / 开关切换时热切换）；空列表清空并释放映射。
    ///
    /// `specs` 是 `(源文件路径, 适用方案白名单)` 对——白名单只影响**查询**（见
    /// [`CommentSource::schemas`]），挂载集合与方案无关，故切方案不再走本函数。
    ///
    /// 路径 **按优先级升序**（先到先得）：同一个词在多个库里都有注释时，取靠前那个库的。
    /// 路径由调用方解析（用户目录优先），与拆字/拼音表同一约定 —— 本 crate 不依赖
    /// wind-config，解析职责一律上提。
    ///
    /// `cache_dir` 是 `.wcmt` 缓存的存放目录（通常是 `<cache>/comments`）。传 `None`
    /// 则全部走内存（测试与无缓存环境）。每个源文件各自缓存、各自校验新鲜度，
    /// 增删一个库不牵动其他库。
    pub fn reload_comments(
        &mut self,
        specs: &[(std::path::PathBuf, Vec<String>)],
        cache_dir: Option<&Path>,
    ) {
        // 先接管旧列表、构建完新的再让它析构：仍在新列表里的库直接原样搬过去，既省掉
        // 一次「读整份源文件算内容指纹」，也让映射不必解除重建。
        //
        // 挂了一份十万条词典的用户，每次热重载都重新校验就要多读几 MB —— 而那份库
        // 通常压根没变。
        let mut old = std::mem::take(&mut self.comments);
        for (p, schemas) in specs {
            if let Some(i) = old.iter().position(|s| s.src == *p) {
                // 顺序无所谓：`old` 之后只用于按路径查找
                let mut reused = old.swap_remove(i);
                // ★ 复用的是**映射**，不是整份规格：只按 path 认领会让改过 `schemas` 的库
                // 仍按旧白名单查——表现是「设置里改了适用方案，重启才生效」。
                reused.schemas.clone_from(schemas);
                self.comments.push(reused);
                continue;
            }
            match load_comment_source(p, schemas, cache_dir) {
                Some(src) => {
                    tracing::info!(
                        "已加载注释库 {}：{} 条{}",
                        p.display(),
                        src.len(),
                        if matches!(src.body, CommentBody::Memory(_)) {
                            "（内存降级）"
                        } else {
                            ""
                        }
                    );
                    self.comments.push(src);
                }
                None => tracing::warn!("注释库加载失败，已跳过：{}", p.display()),
            }
        }
        // 已卸载的库在此释放映射，随后 prune 才删得掉它们的缓存文件（Windows 上被映射
        // 的文件删不掉）。
        drop(old);
        if let Some(dir) = cache_dir {
            prune_comment_cache(dir, specs);
        }
    }

    /// 查词的注释；`code` 非空时同码条目优先，无同码回落该词首条。查不到返回空串。
    ///
    /// 键是**词**而非字：一份「英汉释义」「emoji 名称」可跨五笔/拼音/双拼全部方案复用。
    /// `code` 是可选的方案内消歧（注释库写了 `columns: [text, code, comment]` 时才有）。
    ///
    /// **两遍扫描**：先跨全部库找 code 精确命中，都没有再按挂载顺序取首条。这与合并单表
    /// 时代的语义一致（组内 code 优先于挂载顺序）—— 若改成「逐库各自先 code 后首条」，
    /// 第一个库有该词但 code 对不上时就会截胡，后面库里精确匹配的那条永远轮不到。
    ///
    /// 全都没命中且词含 ASCII 字母时，再按大小写变形重试一遍（见 [`case_fallbacks`]）。
    pub fn comment_of(&self, text: &str, code: Option<&str>, schema: &str) -> String {
        // ★ 白名单过滤对三遍扫描**统一施加**：漏掉任何一遍，那条路径就会从被排除的库里
        // 取出注释，表现为「限定了方案，可有些词还是显示别的方案的注释」——只在大小写
        // 回退这类少数路径上出现，最难查。
        let applicable = || self.comments.iter().filter(|s| s.applies_to(schema));
        if let Some(c) = code.filter(|c| !c.is_empty())
            && let Some(hit) = applicable().find_map(|s| s.lookup_by_code(text, c))
        {
            return hit.to_string();
        }
        if let Some(hit) = applicable().find_map(|s| s.lookup_first(text)) {
            return hit.to_string();
        }
        for v in case_fallbacks(text) {
            if let Some(hit) = applicable().find_map(|s| s.lookup_first(&v)) {
                return hit.to_string();
            }
        }
        String::new()
    }

    /// 重载拆字表（主码表方案变更时热切换）；`path=None` 清空并释放内存。
    pub fn reload_chaizi(&mut self, path: Option<&Path>) {
        self.chaizi = ChaiziTable::default();
        if let Some(p) = path {
            self.load_chaizi(p);
        }
    }

    /// 载入拆字库（字\t字根\t编码）；存编码列 + 字根列。
    fn load_chaizi(&mut self, path: &Path) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("读取拆字库失败 {}: {}", path.display(), e);
                return;
            }
        };
        let mut rows: Vec<(char, &str, &str)> = Vec::new();
        for line in content.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(ch), radicals, code) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let mut chars = ch.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let rad = radicals.map(str::trim).unwrap_or("");
                let code = code.map(str::trim).unwrap_or("");
                if !rad.is_empty() || !code.is_empty() {
                    rows.push((c, rad, code));
                }
            }
        }
        self.chaizi = ChaiziTable::build(rows);
        tracing::info!(
            "拆字库加载完成 {}: {} 字",
            path.display(),
            self.chaizi.len()
        );
    }

    /// 载入拼音表（pinyin-data 格式：`U+4E00: yī  # 一`，多音字逗号分隔）。
    fn load_pinyin(&mut self, path: &Path) {
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let mut rows: Vec<(char, Vec<&str>)> = Vec::new();
        for line in content.lines() {
            let mut line = line.trim();
            if !line.starts_with("U+") {
                continue;
            }
            // 去掉行内 `# 汉字` 注释
            if let Some(idx) = line.find('#') {
                line = line[..idx].trim();
            }
            let Some((hexpart, rest)) = line.split_once(':') else {
                continue;
            };
            let hex = hexpart.trim_start_matches("U+").trim();
            let Ok(cp) = u32::from_str_radix(hex, 16) else {
                continue;
            };
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            // 逗号分隔多音字读音，首项为最常用读音
            let readings: Vec<&str> = rest
                .trim()
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if !readings.is_empty() {
                rows.push((c, readings));
            }
        }
        self.pinyin = PinyinTable::build(rows);
    }

    /// 词的**整词注音**（带声调，逐字以 `sep` 连接）。无读音的字跳过；全空返回空串。
    ///
    /// # `syllables`：用词条编码消歧多音字
    ///
    /// 传入该词在词库里的**音节序列**（来自词条 `code` + `boundary`，如「行长」→ `["hang","zhang"]`）
    /// 时，逐字用对应音节筛掉不匹配的读音（按 [`strip_tone`] 后相等比较）：
    /// 「行」的读音表 `[xíng, háng]` 被 `hang` 筛剩 `háng`，「长」的 `[cháng, zhǎng]` 被 `zhang`
    /// 筛剩 `zhǎng` —— 得 `háng zhǎng` 而非逐字取首音的 `xíng cháng`。
    ///
    /// **这是唯一能让词组注音正确的信息来源**：候选文本本身不携带读音，而词条编码是词库作者
    /// 标注的真值。传 `None`（非拼音来源候选，如五笔候选，没有拼音码可依）则逐字取最常用读音，
    /// 多音字可能不准 —— 这是数据的下界，不是实现缺陷。
    ///
    /// # 声调无法从编码恢复（信息论下界）
    ///
    /// 拼音输入法不打声调，故**同音异调**（「好」hǎo/hào 去调都是 `hao`）筛完仍剩多个，
    /// 只能取最常用的那个。要根治须引入词级注音表 —— 那属于独立注释库的范畴。
    ///
    /// # 长度不匹配即整体降级
    ///
    /// `syllables` 与 `text` 的字数对不上时（词条含非汉字、或调用方切分有误）**放弃筛选**、
    /// 退回逐字首音，而不是按位错配 —— 错配比不筛更糟：它会给出一个看起来精确的错答案。
    /// 参见加词取码在「含非汉字的词」上栽过的同型问题。
    pub fn toned_pinyin_of(&self, text: &str, syllables: Option<&[&str]>, sep: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        // 对不上就不用——宁可少一层精度，也不给按位错配的结果。
        let syls = syllables.filter(|s| s.len() == chars.len());
        chars
            .iter()
            .enumerate()
            .filter_map(|(i, &c)| {
                let readings = self.pinyin.readings(c)?;
                let want = syls.map(|s| strip_tone(s[i]));
                match want {
                    // 有音节可依：筛出去调后相等的首个读音；一个都不匹配时**不回退**到首音——
                    // 不匹配意味着这个字的读音表里根本没有词条标注的那个音（词库或读音表有一方
                    // 过时），此时首音同样没有依据，给出它只会掩盖数据问题。
                    Some(w) => readings.iter().find(|r| strip_tone(r) == w),
                    None => readings.first(),
                }
            })
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// 单字在拆字库里记录的**编码**（`好` → `vbg`）；非单字、无数据均返回空串。
    ///
    /// 与 [`Self::radicals_of`] 取自同一条拆字记录的两列，刻意分开暴露：候选注释模板要能
    /// 把「字根」「编码」各自摆到用户想要的位置（`亻尔 [wq]` / `wq·亻尔` / 只要其一），
    /// 合成一个成品串就把版式写死了。悬停提示那边的 `字根 [编码]` 只是其中一种排法。
    pub fn chaizi_code_of(&self, text: &str) -> String {
        let mut it = text.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => self.chaizi.code(c).unwrap_or_default().to_string(),
            _ => String::new(),
        }
    }

    /// 词的**整词字根串**（逐字字根以 `sep` 连接，无拆字数据的字跳过）。全空返回空串。
    ///
    /// 与 `tooltip_for` 的拆字段是**同源不同粒度**：那里逐字一行、每行还带该字的编码
    /// （在教「这个字怎么拆」），这里整词一行、只给字根（在标注「这个词长什么样」）。
    /// 候选注释段只有一行，容不下逐字展开，故需要这个整词形态。
    pub fn radicals_of(&self, text: &str, sep: &str) -> String {
        text.chars()
            .filter_map(|c| self.chaizi.radicals(c))
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// 生成词的拼音编码（空格分隔、去声调小写；ü→v）。无读音的字跳过。
    /// 用于设置页 dict.genPinyin / 拼音方案加词自动出码。
    pub fn gen_pinyin(&self, text: &str) -> String {
        text.chars()
            .filter_map(|c| self.pinyin.readings(c).and_then(|r| r.first()))
            .map(strip_tone)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 五笔词组取码（86 版首码法）：1字=全码；2字=各取前2码；3字=前2字各首码+末字前2码；
    /// ≥4字=前3字首码+末字首码。
    ///
    /// # ⚠ 已无生产消费点，勿在新代码中使用
    ///
    /// 造词/加词的取码已统一到 `wind_engine::EngineManager::encode_word`
    /// （码源=码表词库自身的单字全码，规则=方案 `[[encoder.rules]]` 声明的公式）。
    /// 本函数保留仅作五笔 86 规则的参考实现，有三个不可用于生产的缺陷：
    ///
    /// 1. **码源是拆字表**，与实际词库解耦 —— 用户换词库/加扩展库后可能算出**打不出来**的码；
    ///    且拆字表是可选资源（全仓 5 个方案只有 `wubi86` 配了），未配的方案取码恒空。
    /// 2. **规则硬编码为五笔 86**，不读方案声明，换任何非五笔码表方案静默出错。
    /// 3. **缺码静默跳过**（`firstn` 返回空串）：「你X好」中 X 无码时会算成「你好」的码，
    ///    产出一个错码却照常入库。
    ///
    /// 见 `docs/design/codetable-auto-phrase.md` §2「码源统一」。
    pub fn wubi_word_code(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let firstn = |c: char, n: usize| -> String {
            self.chaizi
                .code(c)
                .map(|s| s.chars().take(n).collect())
                .unwrap_or_default()
        };
        match chars.len() {
            0 => String::new(),
            1 => self
                .chaizi
                .code(chars[0])
                .map(str::to_string)
                .unwrap_or_default(),
            2 => format!("{}{}", firstn(chars[0], 2), firstn(chars[1], 2)),
            3 => format!(
                "{}{}{}",
                firstn(chars[0], 1),
                firstn(chars[1], 1),
                firstn(chars[2], 2)
            ),
            _ => {
                let last = *chars.last().unwrap();
                format!(
                    "{}{}{}{}",
                    firstn(chars[0], 1),
                    firstn(chars[1], 1),
                    firstn(chars[2], 1),
                    firstn(last, 1)
                )
            }
        }
    }

    /// 为候选文本生成反查提示，按 `opts` 门控各 provider，分段输出（对齐 Go tooltip）。
    ///
    /// 格式规则（对齐 Go `FormatContent`）：
    /// - 单行 section → `标签: 内容`（行内标签，无 []）
    /// - 多行或 always_expand → `[标签]` 标题行 + 逐行内容
    ///
    /// 拆字+拼音同时开启时融合为 `[拆字 / 拼音]`，每行用 `\t` 分隔两列。
    /// 编码段（`word_code`）由调用方按方案词库反查后传入：码表方案=自身全部编码
    /// （码长升序 `/` 连接，如 `a/ab/abc`）、拼音/混输=主码表编码；词不在词库时传
    /// None 不显示——本层不按取码规则生成，生成码常与词库实际码不一致（会提示出打不出的码）。
    /// `code_source`：编码来源方案名——候选并非用该编码方案直接输入时（拼音/临时拼音/
    /// 混输反查主码表）传入，标题显示为 `[编码(五笔)]`；None 显示 `[编码]`。
    pub fn tooltip_for(
        &self,
        text: &str,
        opts: &TooltipOptions,
        word_code: Option<&str>,
        code_source: Option<&str>,
    ) -> String {
        if self.is_empty() && word_code.is_none() {
            return String::new();
        }
        let chars: Vec<char> = text.chars().filter(|c| (*c as u32) >= 0x3400).collect();
        if chars.is_empty() {
            return String::new();
        }
        let mut sections: Vec<Section> = Vec::new();

        // 编码段（整词维度，不逐字拆）：置于最前——编码是核心「如何输入」信息。
        // 以 `[编码]` 标题格式独立成段（always_expand）。只要开启编码就显示整词码，
        // 与拆字互不影响（拆字段另按字给出「字根 [逐字编码]」，二者粒度不同、可并存）。
        if opts.code
            && let Some(code) = word_code.filter(|c| !c.is_empty())
        {
            let label = match code_source.filter(|s| !s.is_empty()) {
                Some(src) => format!("编码({src})"),
                None => "编码".to_string(),
            };
            sections.push(Section {
                label,
                lines: vec![code.to_string()],
                always_expand: true, // 强制 [编码] 标题行
            });
        }

        // 拼音段（逐字，always_expand）
        if opts.pinyin {
            let mut lines = Vec::new();
            for &c in &chars {
                if let Some(readings) = self.pinyin.readings(c) {
                    let n = if !opts.heteronyms {
                        1
                    } else if opts.max_readings > 0 {
                        opts.max_readings.min(readings.len())
                    } else {
                        readings.len()
                    };
                    let shown = readings.iter().take(n).collect::<Vec<_>>().join("/");
                    if !shown.is_empty() {
                        lines.push(format!("{c}：{shown}"));
                    }
                }
            }
            if !lines.is_empty() {
                sections.push(Section {
                    label: "拼音".into(),
                    lines,
                    always_expand: true,
                });
            }
        }

        // 拆字段（字根 [编码]，always_expand；对齐 Go ChaiziProvider）
        if opts.chaizi {
            let mut lines = Vec::new();
            for &c in &chars {
                if let Some(rad) = self.chaizi.radicals(c) {
                    let line = match self.chaizi.code(c) {
                        Some(code) => format!("{c}：{rad} [{code}]"),
                        None => format!("{c}：{rad}"),
                    };
                    lines.push(line);
                }
            }
            if !lines.is_empty() {
                sections.push(Section {
                    label: "拆字".into(),
                    lines,
                    always_expand: true,
                });
            }
        }

        // 拆字+拼音融合（对齐 Go MergeChaiziPinyin）
        let sections = merge_chaizi_pinyin(sections);

        format_sections(sections)
    }
}

/// 去声调：带调号韵母 → 基本字母（ü→v，符合拼音输入习惯）。
fn strip_tone(py: &str) -> String {
    py.chars()
        .map(|c| match c {
            'ā' | 'á' | 'ǎ' | 'à' => 'a',
            'ō' | 'ó' | 'ǒ' | 'ò' => 'o',
            'ē' | 'é' | 'ě' | 'è' => 'e',
            'ī' | 'í' | 'ǐ' | 'ì' => 'i',
            'ū' | 'ú' | 'ǔ' | 'ù' => 'u',
            'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' | 'ü' => 'v',
            other => other,
        })
        .collect::<String>()
        .to_lowercase()
}

#[cfg(test)]
impl ReverseLookup {
    /// 测试助手：直接构建拆字表（字, 字根, 编码）。
    fn set_chaizi(&mut self, rows: Vec<(char, &str, &str)>) {
        self.chaizi = ChaiziTable::build(rows);
    }

    /// 测试助手：直接构建拼音表（字, 读音列表）。
    fn set_pinyin(&mut self, rows: Vec<(char, Vec<&str>)>) {
        self.pinyin = PinyinTable::build(rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_strip_tone() {
        assert_eq!(strip_tone("nǐ"), "ni");
        assert_eq!(strip_tone("hǎo"), "hao");
        assert_eq!(strip_tone("lǜ"), "lv");
    }

    #[test]
    fn test_wubi_word_code_rules() {
        let mut rl = ReverseLookup::default();
        rl.set_chaizi(vec![
            ('工', "", "aaaa"),
            ('人', "", "wwww"),
            ('大', "", "dddd"),
            ('小', "", "ihty"),
        ]);
        // 1字=全码
        assert_eq!(rl.wubi_word_code("工"), "aaaa");
        // 2字=各前2码
        assert_eq!(rl.wubi_word_code("工人"), "aaww");
        // 3字=前2字首码+末字前2码
        assert_eq!(rl.wubi_word_code("工人大"), "awdd");
        // ≥4字=前3字首码+末字首码
        assert_eq!(rl.wubi_word_code("工人大小"), "awdi");
    }

    fn sample_rl() -> ReverseLookup {
        let mut rl = ReverseLookup::default();
        rl.set_chaizi(vec![('好', "女子", "vbg"), ('人', "人", "w")]);
        rl.set_pinyin(vec![('好', vec!["hǎo", "hào"]), ('人', vec!["rén"])]);
        rl
    }

    /// 多音字读音表：「行」xíng/háng、「长」cháng/zhǎng，首项为最常用。
    fn heteronym_rl() -> ReverseLookup {
        let mut rl = ReverseLookup::default();
        rl.set_pinyin(vec![
            ('行', vec!["xíng", "háng", "hàng"]),
            ('长', vec!["cháng", "zhǎng"]),
        ]);
        rl
    }

    /// ★★ 词条音节消歧多音字 —— 本函数存在的全部理由。
    ///
    /// 「行长」逐字取首音会得到 `xíng cháng`，**两个字都错**。带上词库标注的音节
    /// `["hang","zhang"]` 后各自筛剩唯一读音，得 `háng zhǎng`。
    #[test]
    fn syllables_disambiguate_heteronyms() {
        let rl = heteronym_rl();
        assert_eq!(
            rl.toned_pinyin_of("行长", None, " "),
            "xíng cháng",
            "无音节可依时只能逐字取首音（这正是要修的形态）"
        );
        assert_eq!(
            rl.toned_pinyin_of("行长", Some(&["hang", "zhang"]), " "),
            "háng zhǎng",
            "带词条音节应筛出正确读音"
        );
        // 同一个词换一组音节即换一组读音——证明筛选真的按音节走，不是碰巧。
        assert_eq!(
            rl.toned_pinyin_of("行长", Some(&["xing", "chang"]), " "),
            "xíng cháng"
        );
    }

    /// 声调无法从编码恢复：同音异调筛完仍剩多个，取最常用（首项）。
    /// 这是数据下界而非实现缺陷 —— 拼音码里根本没有声调信息。
    #[test]
    fn same_syllable_different_tones_falls_back_to_most_common() {
        let rl = sample_rl(); // 好: hǎo / hào，去调都是 hao
        assert_eq!(rl.toned_pinyin_of("好", Some(&["hao"]), " "), "hǎo");
    }

    /// ★ 音节数与字数对不上时**整体放弃筛选**、退回逐字首音，而不是按位错配。
    ///
    /// 错配比不筛更糟：它会给出一个看起来精确的错答案（把第 2 个字的音套到第 1 个字上）。
    #[test]
    fn mismatched_syllable_count_degrades_instead_of_misaligning() {
        let rl = heteronym_rl();
        // 两个字却只给一个音节：若按位取用会拿 "hang" 套到「行」、第二字越界或错位。
        assert_eq!(
            rl.toned_pinyin_of("行长", Some(&["hang"]), " "),
            "xíng cháng",
            "数量不匹配应整体降级为逐字首音"
        );
    }

    /// 音节与读音表全不匹配时**不回退首音**：不匹配意味着词库与读音表有一方过时，
    /// 此时首音同样没有依据，给出它只会掩盖数据问题。
    #[test]
    fn unmatched_syllable_yields_nothing_for_that_char() {
        let rl = heteronym_rl();
        assert_eq!(
            rl.toned_pinyin_of("行", Some(&["zzz"]), " "),
            "",
            "读音表里没有该音节时不得拿首音充数"
        );
    }

    /// 无读音的字跳过（不产出空段/孤立分隔符）；整词皆无返回空串。
    #[test]
    fn chars_without_readings_are_skipped() {
        let rl = heteronym_rl();
        assert_eq!(rl.toned_pinyin_of("行X", None, " "), "xíng");
        assert_eq!(rl.toned_pinyin_of("XY", None, " "), "");
    }

    // ---------------- 注释表 ----------------

    fn ct(rows: &[(&str, &str, &str)]) -> CommentTable {
        CommentTable::build(
            rows.iter()
                .map(|(t, c, k)| (t.to_string(), c.to_string(), k.to_string()))
                .collect(),
        )
    }

    /// 把若干「库」各写成一个 `.dict.yaml`，返回 `reload_comments` 用的 spec 列表
    /// （顺序即优先级）。白名单一律留空 = 全部方案适用；要测白名单的用例自行改写。
    fn write_libs(
        tag: &str,
        libs: &[&[(&str, &str, &str)]],
    ) -> (PathBuf, Vec<(PathBuf, Vec<String>)>) {
        let dir = std::env::temp_dir().join(format!("wind_cmt_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = libs
            .iter()
            .enumerate()
            .map(|(i, rows)| {
                let mut s =
                    String::from("name: t\ncolumns:\n  - text\n  - comment\n  - code\n...\n");
                for (t, c, k) in rows.iter() {
                    s.push_str(&format!("{t}\t{c}\t{k}\n"));
                }
                let p = dir.join(format!("lib{i}.dict.yaml"));
                std::fs::write(&p, s).unwrap();
                (p, Vec::new())
            })
            .collect();
        (dir, paths)
    }

    /// 用给定的若干库建反查表，**两种后端各建一份**：内存降级（`cache_dir=None`）与
    /// mmap `.wcmt`。同一组断言跑两遍，两条路径的查找语义必须完全一致。
    ///
    /// 断言了 mmap 那份确实走到 `CommentSource::Mmap` —— 少了这一句，缓存目录一旦不可写
    /// 就会静默降级成两份内存表，parity 测试照样全绿却什么都没验证到。
    fn rl_both(
        tag: &str,
        libs: &[&[(&str, &str, &str)]],
    ) -> (PathBuf, Vec<(&'static str, ReverseLookup)>) {
        let (dir, paths) = write_libs(tag, libs);
        let mut mem = ReverseLookup::default();
        mem.reload_comments(&paths, None);
        let mut mm = ReverseLookup::default();
        mm.reload_comments(&paths, Some(&dir.join("cache")));

        assert_eq!(mem.comments.len(), libs.len(), "每个库各一个 source");
        assert!(
            mm.comments
                .iter()
                .all(|s| matches!(s.body, CommentBody::Mmap(_))),
            "mmap 后端必须真的走 mmap，否则本测试退化成两份内存表的自比"
        );
        assert!(
            mem.comments
                .iter()
                .all(|s| matches!(s.body, CommentBody::Memory(_)))
        );
        (dir, vec![("内存", mem), ("mmap", mm)])
    }

    /// 基本点查：命中返回注释，未命中返回 None。
    #[test]
    fn comment_lookup_basic() {
        let t = ct(&[("苹果", "apple", ""), ("香蕉", "banana", "")]);
        assert_eq!(t.lookup_first("苹果"), Some("apple"));
        assert_eq!(t.lookup_first("香蕉"), Some("banana"));
        assert_eq!(t.lookup_first("梨"), None);
        assert_eq!(t.lookup_first(""), None);
    }

    /// ★★ 两种后端（内存降级 / mmap `.wcmt`）的查找语义必须逐条一致。
    ///
    /// mmap 是常态路径、内存是降级路径，二者分叉的话，用户只在缓存目录出问题时才会撞见
    /// 差异——那是最难复现也最难归因的一类故障。
    #[test]
    fn memory_and_mmap_backends_agree() {
        let (dir, both) = rl_both(
            "parity",
            &[&[
                ("行", "háng 行列", "tfhh"),
                ("行", "xíng 走路", "tfhx"),
                ("好", "hǎo 美好", ""),
                ("你好", "hello", ""),
                ("𠮷", "扩展区汉字", ""),
            ]],
        );
        for (name, rl) in &both {
            assert_eq!(rl.comment_of("行", Some("tfhh"), ""), "háng 行列", "{name}");
            assert_eq!(rl.comment_of("行", Some("tfhx"), ""), "xíng 走路", "{name}");
            assert_eq!(
                rl.comment_of("行", Some("hang"), ""),
                "háng 行列",
                "{name} 码不匹配回落首条"
            );
            assert_eq!(rl.comment_of("行", None, ""), "háng 行列", "{name}");
            assert_eq!(rl.comment_of("好", Some("vb"), ""), "hǎo 美好", "{name}");
            assert_eq!(rl.comment_of("你好", None, ""), "hello", "{name}");
            assert_eq!(rl.comment_of("𠮷", None, ""), "扩展区汉字", "{name}");
            assert_eq!(rl.comment_of("没有的词", None, ""), "", "{name}");
            assert_eq!(rl.comment_of("", None, ""), "", "{name}");
        }
        drop(both);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★★ 跨库仲裁：**code 精确匹配优先于库顺序**。
    ///
    /// 靠前的库有这个词但 code 对不上时，不得就地回落——后面库里精确匹配的那条应当胜出。
    /// 合并单表时代这条由「组内先扫 code 再取首条」保证；改成每库独立后，若写成「逐库各自
    /// 先 code 后首条」，第一个库会截胡，这条语义就悄悄丢了。
    #[test]
    fn exact_code_wins_across_libraries() {
        let (dir, both) = rl_both(
            "cross",
            &[
                &[("行", "通用释义", "")],     // 靠前：无 code
                &[("行", "五笔专用", "tfhh")], // 靠后：精确 code
            ],
        );
        for (name, rl) in &both {
            assert_eq!(
                rl.comment_of("行", Some("tfhh"), ""),
                "五笔专用",
                "{name}：精确 code 必须越过靠前库的通用条目"
            );
            assert_eq!(
                rl.comment_of("行", None, ""),
                "通用释义",
                "{name}：无 code 时靠前库优先"
            );
            assert_eq!(
                rl.comment_of("行", Some("hang"), ""),
                "通用释义",
                "{name}：都对不上 code 时回落靠前库首条"
            );
        }
        drop(both);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 源文件改动后必须读到新内容；未改动则复用缓存不重建。
    ///
    /// 这是二进制缓存最容易出错的地方，而错法很隐蔽：缓存恒新鲜 → 「改了词库不生效，
    /// 重启也没用」。用 `.wcmt` 的 mtime 判断是否重建过 —— 直接断言查询结果不足以区分
    /// 「重建了」与「压根没缓存」。
    #[test]
    fn cache_reused_until_source_changes() {
        let (dir, paths) = write_libs("fresh", &[&[("甲", "旧释义", "")]]);
        let cache = dir.join("cache");

        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(rl.comment_of("甲", None, ""), "旧释义");
        let wcmt = find_wcmt(&cache)
            .into_iter()
            .next()
            .expect("应生成 .wcmt 缓存");
        let stamp1 = std::fs::metadata(&wcmt).unwrap().modified().unwrap();

        // 源未变 → 复用缓存（不重写文件）
        drop(rl);
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(rl.comment_of("甲", None, ""), "旧释义");
        assert_eq!(
            std::fs::metadata(&wcmt).unwrap().modified().unwrap(),
            stamp1,
            "源未变时不该重建缓存"
        );

        // 源变更 → 必须重建并读到新内容。先释放映射：Windows 上被 mmap 的文件虽能被
        // rename 覆盖，但旧 view 会继续指向替换前的数据（见 reader_pool 的同名测试）。
        drop(rl);
        std::fs::write(
            &paths[0].0,
            "name: t\ncolumns:\n  - text\n  - comment\n  - code\n...\n甲\t新释义\t\n",
        )
        .unwrap();
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(
            rl.comment_of("甲", None, ""),
            "新释义",
            "源文件改了必须读到新内容——恒新鲜的缓存表现为「改了不生效，重启也没用」"
        );

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 重载时仍在列表里的库**原样复用**：不重新校验指纹，也就不再读源文件。
    ///
    /// 这是切方案（`schemas` 字段）的高频路径——方案专属库开关的同时，全局库通常一字未改，
    /// 而校验指纹要读完整份源文件（十万条约 3.6MB）。
    ///
    /// 判据是「把源文件删掉后仍能查」：复用路径压根不碰源文件，重新加载则会在
    /// `cache_is_fresh` 读源失败 → 重新解析 → 整库消失。
    ///
    /// **`Arc::ptr_eq` 在这里是无效判据**（试过）：即使不复用，`reader_pool` 也会因为旧
    /// Arc 仍存活而交出同一个指针，测试照样全绿——那测的是 reader_pool，不是本函数。
    #[test]
    fn unchanged_libraries_are_reused_without_rereading_source() {
        let (dir, paths) = write_libs("reuse", &[&[("甲", "全局库", "")], &[("乙", "专属库", "")]]);
        let cache = dir.join("cache");
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(rl.comment_of("甲", None, ""), "全局库");

        // 源文件消失（等价于「这次重载没有去读它」的可观测代理）
        std::fs::remove_file(&paths[0].0).unwrap();

        // 去掉第二个库，模拟切到不挂它的方案
        rl.reload_comments(&paths[..1], Some(&cache));
        assert_eq!(
            rl.comment_of("甲", None, ""),
            "全局库",
            "未变动的库必须原样复用——一旦重新校验指纹，每次切方案都要重读整份源文件"
        );
        assert_eq!(rl.comment_of("乙", None, ""), "", "已卸载的库不再参与查询");

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 卸载一个库后，它的 `.wcmt` 与指纹 sidecar 应被清掉，不在缓存目录里越积越多。
    #[test]
    fn removed_library_cache_is_pruned() {
        let (dir, paths) = write_libs("prune", &[&[("甲", "一号库", "")], &[("乙", "二号库", "")]]);
        let cache = dir.join("cache");
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(find_wcmt(&cache).len(), 2);

        // 只留第一个库：第二个的缓存应被清理，第一个的保留（不能连坐）
        rl.reload_comments(&paths[..1], Some(&cache));
        assert_eq!(find_wcmt(&cache).len(), 1, "已卸载库的缓存应被清掉");
        assert_eq!(rl.comment_of("甲", None, ""), "一号库", "保留库不受影响");
        assert_eq!(rl.comment_of("乙", None, ""), "");

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ prune 递归下探时**不得碰词库缓存**：缓存根同住着 `.wdat`/`.wdb`。
    ///
    /// 「这个目录归我管所以可以递归清理」是这类清理最容易写错的地方——注释库缓存与词库
    /// 缓存共用一个根（`comment_cache_path` 按 schemas 下的目录链分命名空间），扫到别人的文件必须原样放过。
    #[test]
    fn prune_never_touches_other_cache_files() {
        let (dir, paths) = write_libs("prune-foreign", &[&[("甲", "一号库", "")]]);
        let cache = dir.join("cache");
        std::fs::create_dir_all(cache.join("wubi86")).unwrap();
        let foreign = cache.join("wubi86").join("wubi86.wdat");
        std::fs::write(&foreign, b"<pretend wdat>").unwrap();
        let foreign_root = cache.join("unigram.wdb");
        std::fs::write(&foreign_root, b"<pretend wdb>").unwrap();

        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        // 再清一次（此时挂载列表为空，最激进的清理条件）
        rl.reload_comments(&[], Some(&cache));

        assert!(foreign.exists(), "词库缓存不得被注释库的清理波及");
        assert!(foreign_root.exists(), "缓存根下的其他文件同样不得被删");
        assert_eq!(find_wcmt(&cache).len(), 0, "自己的缓存该清干净");

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归找出缓存根下所有 `.wcmt`（缓存按 schemas 下的目录链分了命名空间，不止一层）。
    fn find_wcmt(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "wcmt") {
                    out.push(p);
                }
            }
        }
        out
    }

    /// 同名但父目录不同的两个库：缓存靠命名空间隔离，不得互相覆盖。
    ///
    /// 这是去掉路径哈希后唯一真正需要命名空间的场景——两份缓存若落到同一路径，
    /// 每次加载都会因内容指纹不符而互相重建，表现为启动变慢却查不出原因。
    #[test]
    fn same_name_different_dirs_get_separate_caches() {
        let base = std::env::temp_dir().join(format!("wind_cmt_ns_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("libA"), base.join("libB"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let head = "name: t\ncolumns:\n  - text\n  - comment\n...\n";
        std::fs::write(a.join("x.dict.yaml"), format!("{head}词\tA 库\n")).unwrap();
        std::fs::write(b.join("x.dict.yaml"), format!("{head}词\tB 库\n")).unwrap();
        let paths = vec![
            (a.join("x.dict.yaml"), Vec::new()),
            (b.join("x.dict.yaml"), Vec::new()),
        ];
        let cache = base.join("cache");

        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(find_wcmt(&cache).len(), 2, "同名库须各自持有缓存");
        assert_eq!(rl.comment_of("词", None, ""), "A 库", "靠前的库优先");

        drop(rl);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// ★★ 大小写回退必须**双向**：英文库里既有小写词条也有大写缩写。
    ///
    /// 只做 `to_lowercase()` 的话 `Apple`→`apple` 能中，但 `abc`→`ABC` 不能——
    /// 而后者的失败表现为「有些词就是没注释」，从界面上看不出是哪一半漏了。
    #[test]
    fn case_fallback_is_bidirectional() {
        let (dir, both) = rl_both(
            "case",
            &[&[
                ("apple", "n.苹果", ""), // 小写词条
                ("ABC", "字母表", ""),   // 大写缩写
                ("Beijing", "北京", ""), // 首字母大写的专名
                ("好", "hǎo", ""),       // 中文：不该被大小写逻辑碰到
            ]],
        );
        for (name, rl) in &both {
            // 小写词条 ← 各种输入形态
            assert_eq!(rl.comment_of("apple", None, ""), "n.苹果", "{name} 精确");
            assert_eq!(
                rl.comment_of("Apple", None, ""),
                "n.苹果",
                "{name} 首字母大写→小写"
            );
            assert_eq!(
                rl.comment_of("APPLE", None, ""),
                "n.苹果",
                "{name} 全大写→小写"
            );
            // 大写缩写 ← 小写输入（单向 to_lowercase 在这里必失败）
            assert_eq!(rl.comment_of("ABC", None, ""), "字母表", "{name} 精确");
            assert_eq!(
                rl.comment_of("abc", None, ""),
                "字母表",
                "{name} 小写→全大写"
            );
            assert_eq!(
                rl.comment_of("Abc", None, ""),
                "字母表",
                "{name} 首字母大写→全大写"
            );
            // 专名
            assert_eq!(
                rl.comment_of("beijing", None, ""),
                "北京",
                "{name} 小写→首字母大写"
            );
            // 中文与未命中
            assert_eq!(rl.comment_of("好", None, ""), "hǎo", "{name}");
            assert_eq!(rl.comment_of("nosuchword", None, ""), "", "{name}");
        }
        drop(both);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★★ `schemas` 白名单是**查询期**判据，不是挂载期判据。
    ///
    /// 判据必须同时问两件事：① 两个库**都挂上了**（过滤若还在挂载层，这一条就红）；
    /// ② 查询按传入方案分流。只断言 ② 的话，「挂载时筛掉 + 查询时不筛」这种旧实现
    /// 在单方案用例下照样绿。
    #[test]
    fn schema_whitelist_filters_at_query_time() {
        let (dir, mut paths) =
            write_libs("wl", &[&[("甲", "英文库", "")], &[("乙", "通用库", "")]]);
        paths[0].1 = vec!["english".into()];
        let cache = dir.join("cache");
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));

        assert_eq!(rl.comments.len(), 2, "两个库都要挂上——过滤不在挂载层");
        assert_eq!(rl.comment_of("甲", None, "english"), "英文库");
        assert_eq!(
            rl.comment_of("甲", None, "wubi86"),
            "",
            "白名单外的方案查不到这份库"
        );
        assert_eq!(
            rl.comment_of("乙", None, "wubi86"),
            "通用库",
            "空白名单 = 全部方案"
        );
        assert_eq!(
            rl.comment_of("甲", None, ""),
            "英文库",
            "语境未知（空串）时放行——「到处都显示」比「哪都不显示」好查"
        );

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 白名单必须施加到**三遍扫描的每一遍**。
    ///
    /// 只在前两遍加过滤时，本用例的大小写回退那一遍会把被排除的库里的注释捞出来——
    /// 表现是「限定了方案，可有些词还是显示别的方案的注释」，且只在英文大小写不一致时
    /// 出现，是最难自查的那种漏。
    #[test]
    fn schema_whitelist_applies_to_case_fallback_scan() {
        let (dir, mut paths) = write_libs("wl-case", &[&[("apple", "n.苹果", "")]]);
        paths[0].1 = vec!["english".into()];
        let cache = dir.join("cache");
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));

        assert_eq!(
            rl.comment_of("APPLE", None, "english"),
            "n.苹果",
            "本方案内回退照常"
        );
        assert_eq!(
            rl.comment_of("APPLE", None, "wubi86"),
            "",
            "大小写回退那一遍同样受白名单约束"
        );

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 复用旧映射时必须**更新白名单**：只按路径认领会让改过 `schemas` 的库继续按旧
    /// 白名单查，表现是「设置里改了适用方案，重启才生效」。
    #[test]
    fn reload_refreshes_whitelist_on_reused_source() {
        let (dir, mut paths) = write_libs("wl-reuse", &[&[("甲", "释义", "")]]);
        paths[0].1 = vec!["english".into()];
        let cache = dir.join("cache");
        let mut rl = ReverseLookup::default();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(rl.comment_of("甲", None, "wubi86"), "", "先限定在 english");

        // 同一个路径、只改白名单 ⇒ 必然走「复用旧映射」那条分支
        paths[0].1.clear();
        rl.reload_comments(&paths, Some(&cache));
        assert_eq!(
            rl.comment_of("甲", None, "wubi86"),
            "释义",
            "复用的是映射而不是整份规格，白名单要当场跟上"
        );

        drop(rl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ★ 精确匹配**优先于**任何大小写回退：同时存在 `US`/`us` 时各取各的，不得互相顶替。
    #[test]
    fn exact_case_wins_over_fallback() {
        let (dir, both) = rl_both("case-exact", &[&[("US", "美国", ""), ("us", "我们", "")]]);
        for (name, rl) in &both {
            assert_eq!(rl.comment_of("US", None, ""), "美国", "{name}");
            assert_eq!(rl.comment_of("us", None, ""), "我们", "{name}");
            // 两者都不精确匹配时才回退，此处 Us→us（lower 先于 upper）
            assert_eq!(
                rl.comment_of("Us", None, ""),
                "我们",
                "{name} 回退顺序：小写在先"
            );
        }
        drop(both);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无 ASCII 字母的词不进大小写分支（中文是主路径，这条守卫是性能约束不是可选优化）。
    #[test]
    fn non_ascii_text_skips_case_fallbacks() {
        assert!(case_fallbacks("中文词").is_empty());
        assert!(case_fallbacks("１２３").is_empty());
        assert!(case_fallbacks("123").is_empty());
        assert!(case_fallbacks("!@#").is_empty());
        assert!(!case_fallbacks("a").is_empty(), "含字母才产出变形");
    }

    /// 挂载顺序即优先级（跨库）：同一个词在两个库里都有时取靠前那个。
    #[test]
    fn earlier_library_wins() {
        let (dir, both) = rl_both(
            "order",
            &[&[("苹果", "先挂载", "")], &[("苹果", "后挂载", "")]],
        );
        for (name, rl) in &both {
            assert_eq!(rl.comment_of("苹果", None, ""), "先挂载", "{name}");
        }
        drop(both);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 声明了 `code` 列、但某些行只写两列：这些行必须照常入表。
    ///
    /// 曾把 code 列计入「最少列数」，于是这类行整行被丢弃 —— 表现为「库里明明有这个词
    /// 却没注释」，而库本身看起来完全正常。
    #[test]
    fn rows_without_code_survive_when_code_column_declared() {
        let rows =
            parse_str("columns:\n  - text\n  - comment\n  - code\n...\n甲\t有码\tlhnh\n乙\t无码\n");
        assert_eq!(
            rows,
            vec![
                ("甲".to_string(), "有码".to_string(), "lhnh".to_string()),
                ("乙".to_string(), "无码".to_string(), String::new()),
            ]
        );
    }

    /// ★ 挂载顺序即优先级：同词同码时**保留首次出现**的那条（先挂载的库覆盖后挂载的）。
    ///
    /// 依赖 `build` 用稳定排序 —— 换成 `sort_unstable_by`，同 text 条目的相对顺序不再有
    /// 保证，优先级会随输入规模抖动（小输入下可能碰巧对，大输入下随机翻转）。
    #[test]
    fn earlier_source_wins_on_duplicate() {
        let t = ct(&[("行", "第一份", ""), ("行", "第二份", "")]);
        assert_eq!(t.lookup_first("行"), Some("第一份"));
        assert_eq!(t.len(), 1, "同词同码只保留一条");
    }

    /// ★ code 消歧：同词不同码各存一条，按 code 精确匹配。
    #[test]
    fn code_disambiguates_same_text() {
        let t = ct(&[("行", "háng 行列", "tfhh"), ("行", "xíng 走路", "tfhx")]);
        assert_eq!(t.len(), 2, "同词不同码应各存一条");
        assert_eq!(t.lookup_by_code("行", "tfhh"), Some("háng 行列"));
        assert_eq!(t.lookup_by_code("行", "tfhx"), Some("xíng 走路"));
    }

    /// ★★ code 对不上时**回落该词首条**，而不是返回空。
    ///
    /// 注释库里的 `tfhh` 是五笔码；拿拼音候选的 `hang` 去比对必然不匹配。跨方案挂同一份
    /// 注释库是常态，那里不该因为对不上 code 就什么都不显示。
    #[test]
    fn unmatched_code_falls_back_to_first_entry() {
        let t = ct(&[("行", "háng 行列", "tfhh"), ("行", "xíng 走路", "tfhx")]);
        assert_eq!(t.lookup_by_code("行", "hang"), None, "对不上即未命中");
        assert_eq!(t.lookup_first("行"), Some("háng 行列"), "由调用方回落首条");
        assert_eq!(t.lookup_by_code("行", ""), None, "空 code 不参与消歧");
    }

    /// 无 code 的通用条目与带 code 的条目并存：带 code 者按码命中，其余回落首条。
    #[test]
    fn generic_and_coded_entries_coexist() {
        let t = ct(&[("行", "通用", ""), ("行", "五笔专用", "tfhh")]);
        assert_eq!(t.len(), 2);
        assert_eq!(t.lookup_by_code("行", "tfhh"), Some("五笔专用"));
        assert_eq!(
            t.lookup_first("行"),
            Some("通用"),
            "首条是无 code 的通用条目"
        );
    }

    /// 二分边界：首条 / 末条 / 排序键相邻的词都要能查到。
    #[test]
    fn comment_lookup_covers_binary_search_edges() {
        let t = ct(&[
            ("a", "A", ""),
            ("ab", "AB", ""),
            ("b", "B", ""),
            ("z", "Z", ""),
        ]);
        for (k, v) in [("a", "A"), ("ab", "AB"), ("b", "B"), ("z", "Z")] {
            assert_eq!(t.lookup_first(k), Some(v), "查 {k}");
        }
        assert_eq!(
            t.lookup_first("aa"),
            None,
            "落在 a 与 ab 之间的词不得误命中"
        );
        assert_eq!(t.lookup_first("zz"), None, "越过末条不得越界");
    }

    /// 多字节键（中文）在 arena 里按字节切分，不得切在 UTF-8 中间。
    #[test]
    fn multibyte_keys_are_sliced_safely() {
        let t = ct(&[("你好", "hello", "wqvb"), ("世界", "world", "")]);
        assert_eq!(t.lookup_first("你好"), Some("hello"));
        assert_eq!(t.lookup_first("世界"), Some("world"));
    }

    // ---------------- 注释库解析 ----------------

    fn parse_str(content: &str) -> Vec<(String, String, String)> {
        let p = std::env::temp_dir().join(format!(
            "wind_comment_test_{}.dict.yaml",
            content.len() as u64 * 31 + content.as_bytes().first().copied().unwrap_or(0) as u64
        ));
        std::fs::write(&p, content).unwrap();
        let r = parse_comment_dict(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        r
    }

    /// 默认列序 `[text, comment]`（无 columns 声明）。
    #[test]
    fn parse_defaults_to_text_comment() {
        let rows = parse_str("name: x\n...\n苹果\tapple\n香蕉\tbanana\n");
        assert_eq!(
            rows,
            vec![
                ("苹果".into(), "apple".into(), String::new()),
                ("香蕉".into(), "banana".into(), String::new()),
            ]
        );
    }

    /// 显式 columns 声明（流式与块式两种写法都要认）+ code 列。
    #[test]
    fn parse_honors_columns_declaration() {
        let flow = parse_str("columns: [text, code, comment]\n...\n行\ttfhh\thang\n");
        assert_eq!(flow, vec![("行".into(), "hang".into(), "tfhh".into())]);

        let block = parse_str("columns:\n  - text\n  - code\n  - comment\n...\n行\ttfhh\thang\n");
        assert_eq!(block, flow, "块式与流式声明结果须一致");
    }

    /// 声明里缺 comment（或缺 text）→ 整库跳过。没有注释的注释库是配置错误，
    /// 静默当空会让人以为是路径问题。
    #[test]
    fn parse_skips_library_without_comment_column() {
        assert!(parse_str("columns: [text, code]\n...\n行\ttfhh\n").is_empty());
        assert!(parse_str("columns: [comment]\n...\nx\n").is_empty());
    }

    /// `#` 注释行、空行、列数不足的行一律跳过；空 text / 空 comment 同样跳过。
    #[test]
    fn parse_skips_comments_and_incomplete_rows() {
        let rows = parse_str("...\n# 这是注释\n\n只有一列\n苹果\tapple\n\tempty_text\n梨\t\n");
        assert_eq!(rows, vec![("苹果".into(), "apple".into(), String::new())]);
    }

    /// 无 YAML 头的裸 TSV 也能读（容许用户直接丢一张两列表）。
    #[test]
    fn parse_accepts_headerless_tsv() {
        let rows = parse_str("苹果\tapple\n");
        assert_eq!(rows, vec![("苹果".into(), "apple".into(), String::new())]);
    }

    /// 注释内容含空格/标点原样保留（只按 `\t` 切列，不 trim 内容）。
    #[test]
    fn parse_preserves_comment_content() {
        let rows = parse_str("...\n行\txíng; 走路 / háng; 行列\n");
        assert_eq!(rows[0].1, "xíng; 走路 / háng; 行列");
    }

    /// 整词字根串：逐字拼接，无拆字数据的字跳过。
    #[test]
    fn radicals_of_joins_per_char() {
        let rl = sample_rl();
        assert_eq!(rl.radicals_of("好", " "), "女子");
        assert_eq!(rl.radicals_of("好人", " "), "女子 人");
        assert_eq!(rl.radicals_of("好X", " "), "女子", "无字根的字跳过");
        assert_eq!(rl.radicals_of("XY", " "), "");
    }

    #[test]
    fn test_chaizi_table_lookup_and_dup_last_wins() {
        // 乱序输入 + 同字重复：查询按二分命中，重复取文件靠后者（对齐旧 HashMap 覆盖语义）。
        let t = ChaiziTable::build(vec![
            ('乙', "乙", "nnll"),
            ('甲', "田", "old"),
            ('甲', "田", "lhnh"),
            ('丙', "", "gmw"),
        ]);
        assert_eq!(t.len(), 3, "重复字应合并");
        assert_eq!(t.lookup('甲'), Some(("田", "lhnh")), "同字取靠后行");
        assert_eq!(t.code('乙'), Some("nnll"));
        assert_eq!(t.radicals('丙'), None, "空字根应视为无");
        assert_eq!(t.code('丙'), Some("gmw"));
        assert_eq!(t.lookup('丁'), None);
    }

    #[test]
    fn test_reload_chaizi_none_clears() {
        let mut rl = sample_rl();
        assert!(!rl.chaizi.is_empty());
        rl.reload_chaizi(None);
        assert!(rl.chaizi.is_empty(), "None 应清空拆字表");
        assert!(!rl.pinyin.is_empty(), "拼音表不受影响");
        assert_eq!(rl.wubi_word_code("好"), "");
    }

    #[test]
    fn test_tooltip_default_pinyin_and_code() {
        let rl = sample_rl();
        // 编码由调用方按方案词库反查传入（词级）
        let t = rl.tooltip_for("好人", &TooltipOptions::default(), Some("vbww"), None);
        // [拼音] 标题行
        assert!(t.contains("[拼音]"), "应有 [拼音] 标题: {t}");
        assert!(t.contains("好：hǎo/hào"), "默认 heteronyms 显示全读音: {t}");
        // 编码为调用方传入的词库实际码，以 [编码] 标题格式独立成段
        assert!(
            t.contains("[编码]") && t.contains("vbww"),
            "整词编码带标题: {t}"
        );
        assert!(!t.contains("拆字"), "默认不含拆字: {t}");
        // 纯 ASCII 无反查（即使传了编码）
        assert_eq!(
            rl.tooltip_for("abc", &TooltipOptions::default(), Some("x"), None),
            ""
        );
    }

    #[test]
    fn test_tooltip_single_char_code() {
        let rl = sample_rl();
        let t = rl.tooltip_for("好", &TooltipOptions::default(), Some("vbg"), None);
        // 单字编码=词库实际全码，以 [编码] 标题格式独立成段
        assert!(
            t.contains("[编码]") && t.contains("vbg"),
            "单字编码带标题: {t}"
        );
        // 调用方未传编码（词不在方案词库）→ 无编码段，不臆测生成
        let t2 = rl.tooltip_for("好", &TooltipOptions::default(), None, None);
        assert!(!t2.contains("[编码]"), "无词库码不显示编码段: {t2}");
    }

    #[test]
    fn test_tooltip_provider_gating() {
        let rl = sample_rl();
        // 仅拼音：code 开关关闭时传入的编码也不显示
        let opts = TooltipOptions {
            code: false,
            pinyin: true,
            heteronyms: true,
            max_readings: 0,
            chaizi: false,
        };
        let t = rl.tooltip_for("好", &opts, Some("vbg"), None);
        assert!(
            t.contains("拼音") && !t.contains("编码") && !t.contains("拆字"),
            "{t}"
        );
    }

    #[test]
    fn test_tooltip_code_only_with_empty_tables() {
        // 反查表全空（无拆字库/拼音表）但调用方传入词库码 → 仍显示编码段
        let rl = ReverseLookup::default();
        let t = rl.tooltip_for("好", &TooltipOptions::default(), Some("vbg"), None);
        assert_eq!(t, "[编码]\nvbg", "空表仅编码段: {t}");
    }

    #[test]
    fn test_tooltip_code_source_label() {
        // 编码来源方案名标注：拼音/临时拼音下编码来自主码表 → 标题带方案名
        let rl = ReverseLookup::default();
        let t = rl.tooltip_for("好", &TooltipOptions::default(), Some("vbg"), Some("五笔"));
        assert_eq!(t, "[编码(五笔)]\nvbg", "标题应带来源方案名: {t}");
        // 多码按长度排列原样显示
        let t2 = rl.tooltip_for("好", &TooltipOptions::default(), Some("v/vb/vbg"), None);
        assert_eq!(t2, "[编码]\nv/vb/vbg", "多码列表原样显示: {t2}");
        // 空来源名等同无标注
        let t3 = rl.tooltip_for("好", &TooltipOptions::default(), Some("vbg"), Some(""));
        assert_eq!(t3, "[编码]\nvbg", "空来源名不加括注: {t3}");
    }

    #[test]
    fn test_tooltip_heteronyms_and_max_readings() {
        let rl = sample_rl();
        // heteronyms=false → 仅首音
        let opts = TooltipOptions {
            heteronyms: false,
            ..Default::default()
        };
        let t = rl.tooltip_for("好", &opts, None, None);
        assert!(t.contains("好：hǎo") && !t.contains("hào"), "仅首音: {t}");
        // max_readings=1 → 截断到 1
        let opts2 = TooltipOptions {
            max_readings: 1,
            ..Default::default()
        };
        let t2 = rl.tooltip_for("好", &opts2, None, None);
        assert!(t2.contains("好：hǎo") && !t2.contains("hào"), "截断: {t2}");
    }

    #[test]
    fn test_tooltip_chaizi() {
        let rl = sample_rl();
        let opts = TooltipOptions {
            code: false,
            pinyin: false,
            chaizi: true,
            ..Default::default()
        };
        let t = rl.tooltip_for("好", &opts, None, None);
        // 多行 always_expand → [拆字] 标题 + 内容行
        assert_eq!(t, "[拆字]\n好：女子 [vbg]", "拆字段含字根+编码: {t}");
    }

    #[test]
    fn test_tooltip_code_and_chaizi_coexist() {
        let rl = sample_rl();
        // 编码 + 拆字同开：整词 [编码] 段显示（置顶），拆字行另含逐字编码，二者并存。
        let opts = TooltipOptions {
            code: true,
            pinyin: false,
            chaizi: true,
            ..Default::default()
        };
        let t = rl.tooltip_for("好", &opts, Some("vbg"), None);
        assert!(t.contains("[编码]"), "整词编码段应显示: {t}");
        assert!(t.contains("[拆字]"), "拆字标题: {t}");
        assert!(t.contains("好：女子 [vbg]"), "逐字编码内嵌于拆字行: {t}");
        // [编码] 段置于拆字之前（编码是核心「如何输入」信息）
        assert!(
            t.find("[编码]") < t.find("[拆字]"),
            "编码段应在拆字段之前: {t}"
        );
    }

    #[test]
    fn test_tooltip_merge_chaizi_pinyin() {
        let rl = sample_rl();
        let opts = TooltipOptions {
            code: false,
            pinyin: true,
            chaizi: true,
            ..Default::default()
        };
        let t = rl.tooltip_for("好", &opts, None, None);
        // 拆字+拼音融合为 [拆字 / 拼音]
        assert!(t.contains("[拆字 / 拼音]"), "融合标题: {t}");
        // 拆字行 + \t + 拼音读音（剥离"字："前缀）
        assert!(
            t.contains("好：女子 [vbg]\thǎo/hào"),
            "融合行含拆字+拼音: {t}"
        );
        // 不应有独立的拼音或拆字标题
        assert!(!t.contains("[拼音]"), "无独立拼音段: {t}");
        assert!(!t.contains("[拆字]"), "无独立拆字段: {t}");
    }

    #[test]
    fn test_gen_pinyin_uses_first_reading() {
        let mut rl = ReverseLookup::default();
        // 多音字"重"：首音 zhòng（最常用），次音 chóng
        rl.set_pinyin(vec![('重', vec!["zhòng", "chóng"]), ('要', vec!["yào"])]);
        assert_eq!(rl.gen_pinyin("重要"), "zhong yao");
    }

    #[test]
    fn test_tooltip_multi_reading_joined() {
        let mut rl = ReverseLookup::default();
        rl.set_pinyin(vec![('重', vec!["zhòng", "chóng"])]);
        let t = rl.tooltip_for("重", &TooltipOptions::default(), None, None);
        assert!(t.contains("zhòng/chóng"), "多音字读音应以 / 连接: {t}");
    }

    #[test]
    fn test_load_pinyin_parses_multi_reading() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("wind-reverse-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pinyin_map.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# 头部注释").unwrap();
        writeln!(f, "U+4E00: yī  # 一").unwrap();
        writeln!(f, "U+91CD: zhòng,chóng  # 重").unwrap();
        drop(f);

        let mut rl = ReverseLookup::default();
        rl.load_pinyin(&path);
        assert_eq!(
            rl.pinyin.readings('一').unwrap().iter().collect::<Vec<_>>(),
            vec!["yī"]
        );
        assert_eq!(
            rl.pinyin.readings('重').unwrap().iter().collect::<Vec<_>>(),
            vec!["zhòng", "chóng"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_pinyin_table_lookup_and_dup_last_wins() {
        // 与拆字表对称：乱序输入 + 同字重复，查询按二分命中，重复取靠后者。
        let t = PinyinTable::build(vec![
            ('乙', vec!["yǐ"]),
            ('甲', vec!["jiǎ"]),
            ('甲', vec!["jiá", "jiǎ"]),
            ('丙', vec!["bǐng"]),
        ]);
        assert_eq!(t.entries.len(), 3, "重复字应合并");
        assert_eq!(
            t.readings('甲').unwrap().iter().collect::<Vec<_>>(),
            vec!["jiá", "jiǎ"],
            "同字取靠后行"
        );
        // 相邻条目的读音区间不串味（甲有 2 个读音，其后的乙仍应只取自己那条）
        assert_eq!(t.readings('乙').unwrap().first(), Some("yǐ"));
        assert_eq!(t.readings('乙').unwrap().len(), 1);
        assert_eq!(t.readings('丙').unwrap().first(), Some("bǐng"));
        assert!(t.readings('丁').is_none());
    }
}
