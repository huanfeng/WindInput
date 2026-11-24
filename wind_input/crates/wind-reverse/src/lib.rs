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

use std::collections::HashSet;
use std::path::Path;

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
    /// 指向本库的 `[[ui.comment_dicts]].id`（供 `${dict:id}` 按库取）。一般一个；两条规格
    /// 解析到同一文件时挂载去重成一个库，两个 id 都认它。由 [`ReverseLookup::set_comment_ids`]
    /// 在挂载后写入。
    ids: Vec<String>,
    /// 只能按 id 取（`${dict:id}`），裸 `${dict}` / `${dict_all}` 不查它。随包内置的释义库
    /// 用：老模板里的 `${dict}` 不该因为升级就冒出英文释义。由 [`ReverseLookup::set_named_only`] 写入。
    named_only: bool,
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
    // 行尾规整：下面靠 `content.find("\n...")` 找正文起点，孤立 \r 会让整份文件算成
    // 一行 ⇒ 找不到分隔行 ⇒ 整个文件被当成正文再逐行解析，结果是一条注释都出不来。
    // 无孤立 \r 时零拷贝（注释库可以很大），\r\n 原样留着——下面本来就剥了它。
    let normalized = wind_utils::text::normalize_input(&content);
    let content: &str = &normalized;
    // 正文起点：首个独占一行的 `...` 之后。无该行 → 整个文件都是正文（容许无 YAML 头的裸表）。
    // `bare`：连 `...` 都没有的裸表。OpenCC 文本词典（rime-frost 的 `chinese_english.txt` 等）
    // 就是这种形态，按 [`opencc_comment`] 的约定读；有 `...` 的是 rime 词库形态，不动。
    let (header, body, bare) = match content.find("\n...") {
        Some(i) if content[i + 1..].starts_with("...") => {
            let rest = &content[i + 4..];
            let body = rest.strip_prefix("\r").unwrap_or(rest);
            (
                &content[..i],
                body.strip_prefix('\n').unwrap_or(body),
                false,
            )
        }
        _ if content.starts_with("...") => (&content[..0], &content[3..], false),
        _ => (&content[..0], content, true),
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
        let mut comment = parts[comment_col];
        if bare {
            let Some(c) = opencc_comment(text, comment) else {
                continue;
            };
            comment = c;
        }
        if text.is_empty() || comment.is_empty() {
            continue;
        }
        let code = code_col.and_then(|i| parts.get(i)).copied().unwrap_or("");
        out.push((text.to_string(), comment.to_string(), code.to_string()));
    }
    Ok(out)
}

/// 裸表（无 YAML 头）按 OpenCC 文本词典的约定取注释；返回 `None` 表示这行不是词条。
///
/// OpenCC 文本词典（`键<Tab>值1 值2 …`）只是 OpenCC 项目的约定，没有规范；rime 生态在它上面
/// 又叠了两条约定，这里都要认，用户从那边拿来的表才能直接放：
/// - **值的首项重复键本身**（`好\t好 good well`）：rime simplifier 用转换结果替换原候选，要保住
///   原词就得把它写成首项。作注释时它是噪音（显示成「好 good well」），丢掉。不带这一项的表
///   原样保留。同 `wind_dict::emojidict::parse_upstream`。
/// - **`---` 开头的行是注释**：OpenCC 文本词典没有注释语法，rime-frost 借「键不可重复」写成
///   `----------格式为：…\t----------…`。
fn opencc_comment<'a>(text: &str, value: &'a str) -> Option<&'a str> {
    if text.starts_with("---") {
        return None;
    }
    let v = value.trim_start();
    match v.strip_prefix(text) {
        Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
            Some(rest.trim_start())
        }
        _ => Some(value),
    }
}

/// 注释查询精确查不到时，大小写回退往哪个方向走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseFallback {
    /// 双向（[`case_fallbacks`] 全部变形）：查询的文本是用户**打出来的形态**，大小写不说明
    /// 它是哪个词——输入 `abc` 要能查到缩写 `ABC`。
    Both,
    /// 只往小写走（`Configure` → `configure`、`ASP` → `asp`），不升格（论坛 t281）。
    ///
    /// 用于词的大小写**已由词库确证**的场合：英文候选按输入大小写投影过时（`asp` 显示成
    /// `ASP`），调用方拿词库原文 `asp` 来查。小写 `asp`（毒蛇）与缩写 `ASP` 是两个词，
    /// 升格会给前者贴上后者的释义；往小写走只是句首 / 大写锁定的形态差，照常放行。
    LowerOnly,
}

impl CaseFallback {
    fn variants(self, text: &str) -> Vec<String> {
        let mut out = case_fallbacks(text);
        if self == Self::LowerOnly {
            out.retain(|v| only_lowers(text, v));
        }
        out
    }
}

/// `to` 是否只是把 `from` 的若干大写字母改成了小写（逐字符对应，其余原样）。
fn only_lowers(from: &str, to: &str) -> bool {
    from.chars().count() == to.chars().count()
        && from
            .chars()
            .zip(to.chars())
            .all(|(f, t)| f == t || (f.is_uppercase() && f.to_lowercase().eq(std::iter::once(t))))
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
            ids: Vec::new(),
            named_only: false,
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
            // `x.wcmt` / `x.wcmt.fp` / `x.wcmt.st` / `x.wcmt.tmp` 都归到主名 `x.wcmt` 上判定
            let base = name
                .strip_suffix(".fp")
                .or_else(|| name.strip_suffix(".st"))
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

/// 扫 YAML 头里的 `columns:` 声明 → 列名序列；**`None` = 整个声明不存在**。
///
/// 「无声明」与「声明了但没有某列」必须分开：注释库无声明时按默认 `[text, comment]` 读，
/// 而主词库无声明时绝不能被当成带注释的库（见 [`declares_comment_column`]）。把两态压成
/// 一个 `Vec` 就再也分不出来了。
///
/// 两种 YAML 写法都认：块序列（`columns:` 换行后 `  - text`）与流式（`columns: [text, comment]`）。
fn columns_names(header: &str) -> Option<Vec<String>> {
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
    in_columns.then_some(names)
}

/// 从 YAML 头解析注释库的列位置 → `(text, comment, code)`。
/// 无 `columns:` 声明时取默认 `[text, comment]`；声明里缺 text 或 comment 返回 `None`。
fn comment_columns(header: &str) -> Option<(usize, usize, Option<usize>)> {
    let Some(names) = columns_names(header) else {
        return Some((0, 1, None)); // 默认列序
    };
    let find = |k: &str| names.iter().position(|n| n == k);
    Some((find("text")?, find("comment")?, find("code")))
}

/// 只读 YAML 头能有多大 —— 超出即认定这不是个 rime 词库头（正文早该开始了）。
///
/// 主词库正文动辄几百 MB，检测「有没有 comment 列」绝不能把整份读进来：本函数是
/// **每次启动对每个已启用词库都要跑一遍**的，代价必须与文件大小无关。
const HEADER_SCAN_LIMIT: usize = 64 * 1024;

/// 一个 `.dict.yaml` 是否**显式声明**了可用的 `comment` 列（同时得有 `text`）。
///
/// 这是「词库自带注释 → 自动派生一个注释源」的准入判据，故意比 [`comment_columns`] 严格：
/// 后者在**无 `columns:` 声明**时回落默认 `[text, comment]`，那对注释库是合理默认，对主
/// 词库则是彻头彻尾的误判 —— 绝大多数码表是 `text\tcode\tweight`，按默认列序读会把整列
/// 编码当成注释，且没有任何一处会报错。**「无声明」在这里必须等于「没有注释」。**
///
/// 只读到 YAML 头结束（独占一行的 `...`）或 [`HEADER_SCAN_LIMIT`] 为止。
pub fn declares_comment_column(path: &Path) -> bool {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = String::new();
    for line in std::io::BufReader::new(f).lines() {
        let Ok(line) = line else { return false }; // 非 UTF-8 → 不是我们认的格式
        if line.trim_end() == "..." {
            break;
        }
        if header.len() + line.len() > HEADER_SCAN_LIMIT {
            break;
        }
        header.push_str(&line);
        header.push('\n');
    }
    columns_names(&header).is_some_and(|names| {
        names.iter().any(|n| n == "comment") && names.iter().any(|n| n == "text")
    })
}

impl ReverseLookup {
    /// 诊断：(拆字表, 拼音表, 常驻内存的注释库) 各自的堆字节（按容量）；mmap 的注释库不计。
    pub fn heap_bytes(&self) -> (usize, usize, usize) {
        let chaizi = self.chaizi.entries.capacity() * size_of::<ChaiziEntry>()
            + self.chaizi.arena.capacity();
        let pinyin = self.pinyin.entries.capacity() * size_of::<PinyinEntry>()
            + self.pinyin.reading_ends.capacity() * size_of::<u32>()
            + self.pinyin.arena.capacity();
        let comments = self
            .comments
            .iter()
            .map(|c| match &c.body {
                CommentBody::Mmap(_) => 0,
                CommentBody::Memory(t) => {
                    t.entries.capacity() * size_of::<CommentEntry>() + t.arena.capacity()
                }
            })
            .sum();
        (chaizi, pinyin, comments)
    }

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
        self.comment_in(None, text, code, schema, CaseFallback::Both)
    }

    /// [`Self::comment_of`] / [`Self::comment_of_id`]（`id` 为 `Some`）的通用形：可指定大小写
    /// 回退的方向（见 [`CaseFallback`]）。
    pub fn comment_of_with(
        &self,
        id: Option<&str>,
        text: &str,
        code: Option<&str>,
        schema: &str,
        fallback: CaseFallback,
    ) -> String {
        self.comment_in(id, text, code, schema, fallback)
    }

    /// 只在 id 为 `id` 的那个注释库里查（`${dict:id}`）；其余同 [`Self::comment_of`]，
    /// 方案白名单照样生效。
    ///
    /// `${dict}` 只回**一条**：按挂载顺序第一个命中的库胜出。中英释义与汉字拆分都挂成注释库
    /// 时，单字「好」只能显示其一；按库取才能在同一模板里并排写 `${dict:en} ${dict:chaifen}`。
    /// 不存在的 id 得空串（与查不到同档）。
    pub fn comment_of_id(&self, id: &str, text: &str, code: Option<&str>, schema: &str) -> String {
        self.comment_in(Some(id), text, code, schema, CaseFallback::Both)
    }

    /// 写入各库对应的 `[[ui.comment_dicts]].id`（`(源文件路径, ids)`，路径同 `reload_comments`）。
    /// 与挂载分开：改 id 不改路径时不必重挂 mmap。
    pub fn set_comment_ids(&mut self, ids: &[(std::path::PathBuf, Vec<String>)]) {
        for s in &mut self.comments {
            s.ids = ids
                .iter()
                .find(|(p, _)| *p == s.src)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
        }
    }

    /// 标记哪些库只能按 id 取（见 [`CommentSource::named_only`]），路径同 `reload_comments`。
    pub fn set_named_only(&mut self, paths: &[std::path::PathBuf]) {
        for s in &mut self.comments {
            s.named_only = paths.contains(&s.src);
        }
    }

    /// 查词在**全部适用库**里的注释（`${dict_all}`）：每库最多一条（先同码、无则该库首条），
    /// 按挂载顺序，按字符串去重，空串跳过。所有库都没有精确命中时，再按大小写变形各库
    /// 取一条（同 [`Self::comment_of`] 的回退那遍，且同受白名单约束）。
    ///
    /// 不变量：[`Self::comment_of`] 的结果总在本函数的结果里——它取的是某个库的那一条，
    /// 而那个库在这里出的正是同一条（逐库「先同码后首条」与跨库两遍扫描对单个库等价）。
    pub fn comments_of(&self, text: &str, code: Option<&str>, schema: &str) -> Vec<String> {
        self.comments_of_with(text, code, schema, CaseFallback::Both)
    }

    /// [`Self::comments_of`] 的通用形：可指定大小写回退的方向（见 [`CaseFallback`]）。
    pub fn comments_of_with(
        &self,
        text: &str,
        code: Option<&str>,
        schema: &str,
        fallback: CaseFallback,
    ) -> Vec<String> {
        let code = code.filter(|c| !c.is_empty());
        let mut out: Vec<String> = Vec::new();
        let mut push = |s: &str| {
            if !s.is_empty() && !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        };
        let mut exact = false;
        for src in self.applicable_comments(None, schema) {
            let one = code
                .and_then(|c| src.lookup_by_code(text, c))
                .or_else(|| src.lookup_first(text));
            if let Some(s) = one {
                exact = true;
                push(s);
            }
        }
        if !exact {
            let variants = fallback.variants(text);
            if !variants.is_empty() {
                for src in self.applicable_comments(None, schema) {
                    if let Some(s) = variants.iter().find_map(|v| src.lookup_first(v)) {
                        push(s);
                    }
                }
            }
        }
        out
    }

    /// 适用于 `schema` 语境的注释库（`id` 给定时只限该库），按挂载顺序。
    ///
    /// ★ 白名单判据只此一份：`comment_in` 的三遍扫描与 `comments_of` 的两遍都经它。
    /// 漏掉任何一遍，那条路径就会从被排除的库里取出注释，表现为「限定了方案，可有些词
    /// 还是显示别的方案的注释」——只在大小写回退这类少数路径上出现，最难查。
    fn applicable_comments<'a>(
        &'a self,
        id: Option<&'a str>,
        schema: &'a str,
    ) -> impl Iterator<Item = &'a CommentSource> + 'a {
        self.comments.iter().filter(move |s| {
            s.applies_to(schema)
                && match id {
                    Some(id) => s.ids.iter().any(|i| i == id),
                    None => !s.named_only,
                }
        })
    }

    fn comment_in(
        &self,
        id: Option<&str>,
        text: &str,
        code: Option<&str>,
        schema: &str,
        fallback: CaseFallback,
    ) -> String {
        let applicable = || self.applicable_comments(id, schema);
        if let Some(c) = code.filter(|c| !c.is_empty())
            && let Some(hit) = applicable().find_map(|s| s.lookup_by_code(text, c))
        {
            return hit.to_string();
        }
        if let Some(hit) = applicable().find_map(|s| s.lookup_first(text)) {
            return hit.to_string();
        }
        for v in fallback.variants(text) {
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

    /// 拆字表是否在内存里（有条目）。
    pub fn has_chaizi(&self) -> bool {
        !self.chaizi.is_empty()
    }

    /// 与 `other` 交换拆字表。供「锁外读盘建好、锁内一次换入」：`other` 用
    /// `ReverseLookup::load(None, Some(path))` 建，换完它持有的是旧表、随之释放。
    pub fn swap_chaizi(&mut self, other: &mut ReverseLookup) {
        std::mem::swap(&mut self.chaizi, &mut other.chaizi);
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
        // 行尾规整同 parse_comment_dict。★ 这一处与 load_pinyin 一度被守卫的**文件粒度**
        // 放过：同文件的 parse_comment_dict 调了 normalize_input，整个 lib.rs 就被判成
        // 「已覆盖」，这两个独立的加载器就此隐身。守卫已收紧到函数粒度。
        let content = wind_utils::text::normalize_input(&content);
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
        // 行尾规整，同 load_chaizi
        let content = wind_utils::text::normalize_input(&content);
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
                    //
                    // 例外是**简拼音节**（`sa'h` 造出的「撒哈」，词条码为 `sa`+`h`）：简拼段是读音
                    // 的前缀，仍是词条给的依据，故精确不中时取首个以它开头的读音（t200）。
                    // 只认**纯声母**段：带元音的是完整音节（`an`），前缀回退会把它错配成 `áng`。
                    Some(w) => readings.iter().find(|r| strip_tone(r) == w).or_else(|| {
                        let initial_only = !w.contains(['a', 'e', 'i', 'o', 'u', 'v']);
                        initial_only
                            .then(|| readings.iter().find(|r| strip_tone(r).starts_with(&w)))
                            .flatten()
                    }),
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
    /// 与悬停提示的拆字段是**同源不同粒度**：那里逐字一行、每行还带该字的编码
    /// （在教「这个字怎么拆」），这里整词一行、只给字根（在标注「这个词长什么样」）。
    /// 候选注释段只有一行，容不下逐字展开，故需要这个整词形态。
    pub fn radicals_of(&self, text: &str, sep: &str) -> String {
        self.radicals_each(text).join(sep)
    }

    /// [`Self::radicals_of`] 的分项形态：逐字字根，无拆字数据的字跳过。
    pub fn radicals_each(&self, text: &str) -> Vec<&str> {
        text.chars()
            .filter_map(|c| self.chaizi.radicals(c))
            .collect()
    }

    /// 词的**整词反查编码串**（逐字编码以 `sep` 连接，无拆字数据的字跳过）。全空返回空串。
    ///
    /// 与 [`Self::radicals_of`] 对称、同源不同列：那边取字根（「这个词长什么样」），这里
    /// 取拆字库里记录的编码（「这个词逐字怎么打」）。与 [`Self::chaizi_code_of`] 的关系
    /// 也对应 `radicals_of` 与 [`Self::chaizi_code_of`] 里 `chaizi`/单字取码的关系：
    /// 那个仅单字候选可用，这个不限字数。
    pub fn codes_of(&self, text: &str, sep: &str) -> String {
        self.codes_each(text).join(sep)
    }

    /// [`Self::codes_of`] 的分项形态：逐字编码，无编码的字跳过。
    pub fn codes_each(&self, text: &str) -> Vec<&str> {
        text.chars().filter_map(|c| self.chaizi.code(c)).collect()
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

    /// 单字的读音（最常用在前），至多 `max` 个（0 = 不限），以 `sep` 连接。查不到返回空串。
    ///
    /// 悬停提示逐字段 `${readings[:N]}` 的数据源：读音全集是**字**的属性，与词里念哪个无关，
    /// 这正是它和 [`Self::toned_pinyin_of`]（按词消歧、每字一个）的分工。
    pub fn readings_of(&self, c: char, max: usize, sep: &str) -> String {
        let Some(readings) = self.pinyin.readings(c) else {
            return String::new();
        };
        let n = if max == 0 { readings.len() } else { max };
        readings.iter().take(n).collect::<Vec<_>>().join(sep)
    }
}

/// 去声调：带调号韵母 → 基本字母（ü→v，符合拼音输入习惯）。
///
/// 与 `wind-coordinator` 的 `comment::strip_tones` 平行但口径不同：这里产出与输入码
/// 比对的键（`ü`→`v`、小写、不处理 `ń`/`ḿ`），那边产出给人看的注释（`ü` 保留、
/// `ń`/`ḿ` 去调）。改其一时核对另一处是否也该改。
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
#[path = "lib_tests.rs"]
mod tests;
