//! `.wemj`：mmap 零拷贝的 emoji 扩展表（词 → emoji 列表）
//!
//! 「打出词组后在候选里追加对应 emoji」的查表数据。设计见
//! `docs/design/emoji-suggestion.md`，本模块只负责**存储格式与构建**，不含任何候选逻辑。
//!
//! # 为什么走 mmap 而不是内存表
//!
//! 与 [`crate::commentdict`] 同一条理由，那份模块文档已论证过：输入法是常驻进程，
//! **全解析进内存意味着容量直接变成常驻内存**，而这是个出厂关闭的可选功能。
//! ★ 判据不是「这次数据小」（上游现为 4668 行 / 126KB）——那是每个功能都能说的话；
//! 真正的代价是一旦走内存表，就会长出一条平行的加载/失效/降级路径（指纹怎么算、
//! 转换失败怎么办、换表后怎么清、多实例怎么共享），每条都要重答一遍，且迟早与词库
//! 那套分叉。
//!
//! # 为什么另建格式而不直接用 `.wcmt`
//!
//! `.wcmt` 的 Row 是 `text | comment | code`，把 emoji 塞进 comment 段确实能跑。但仓里
//! 已有一次同样的取舍并给出了答案——[`crate::reverseidx`] 的模块文档：「本格式的骨架取自
//! `commentdict`（`.wcmt`：排序数组 + 二分），**数据语义**不同」。即**骨架复用、格式独立**。
//! 本表随设计还会长出分类维度（`emoji_category.txt`）与来源标记，挤在 comment 段里迟早要拆。
//!
//! 索引同样只做**精确点查**（每次只查当前页那 5~9 条候选），排序数组 + 二分即可，不需要 DAT。
//!
//! # 文件布局
//!
//! - Header (24B)：magic `WEMJ` + version u32 + entry_count u32 + index_off u32
//!   + str_off u32 + reserved u32
//! - Entry[entry_count] (8B 每条，**按 text 升序**)：off u32（相对 str_off）
//!   + text_len u16 + emoji_len u16
//! - StringPool：每条连续存 `text | emojis`，`emojis` 为**空格分隔**的 UTF-8
//!
//! ★ 与 `.wcmt` 不同，**同一 text 至多一条**——重复键在 [`write_emoji_wemj`] 构建时就已合并
//! （见该函数关于繁→简撞键的说明）。查询端因此不需要组迭代。

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use tracing::{info, warn};

const MAGIC: [u8; 4] = *b"WEMJ";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 8;

/// emoji 扩展表 mmap 读取器。
pub struct EmojiReader {
    mmap: Mmap,
    entry_count: u32,
    index_off: u32,
    str_off: u32,
}

/// 一条记录的两段文本（借用自 mmap，零拷贝）。
struct Row<'a> {
    text: &'a str,
    emojis: &'a str,
}

impl EmojiReader {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < HEADER_SIZE {
            anyhow::bail!("emoji wemj too short");
        }
        if mmap[0..4] != MAGIC {
            anyhow::bail!("invalid emoji magic");
        }
        let version = u32::from_le_bytes(mmap[4..8].try_into().unwrap());
        if version != VERSION {
            anyhow::bail!("unsupported emoji version: {}", version);
        }
        let entry_count = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        let index_off = u32::from_le_bytes(mmap[12..16].try_into().unwrap());
        let str_off = u32::from_le_bytes(mmap[16..20].try_into().unwrap());

        let index_end = index_off as usize + entry_count as usize * ENTRY_SIZE;
        if index_end > mmap.len() || str_off as usize > mmap.len() {
            anyhow::bail!("emoji wemj offsets out of range");
        }
        info!(
            "Opened emoji dict: {} ({} entries)",
            path.display(),
            entry_count
        );
        Ok(Self {
            mmap,
            entry_count,
            index_off,
            str_off,
        })
    }

    pub fn entry_count(&self) -> u32 {
        self.entry_count
    }

    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// 读第 `i` 条的两段文本。越界 / UTF-8 损坏 → None（当作该条不存在，**不 panic**：
    /// 缓存文件可能被外部破坏或半写，功能降级好过崩进程。同 `.wcmt` 的处置）。
    fn row(&self, i: u32) -> Option<Row<'_>> {
        let off = self.index_off as usize + i as usize * ENTRY_SIZE;
        if off + ENTRY_SIZE > self.mmap.len() {
            return None;
        }
        let str_start = u32::from_le_bytes(self.mmap[off..off + 4].try_into().ok()?) as usize;
        let text_len = u16::from_le_bytes(self.mmap[off + 4..off + 6].try_into().ok()?) as usize;
        let emoji_len = u16::from_le_bytes(self.mmap[off + 6..off + 8].try_into().ok()?) as usize;

        let base = self.str_off as usize + str_start;
        let text_end = base + text_len;
        let emoji_end = text_end + emoji_len;
        if emoji_end > self.mmap.len() {
            return None;
        }
        Some(Row {
            text: std::str::from_utf8(&self.mmap[base..text_end]).ok()?,
            emojis: std::str::from_utf8(&self.mmap[text_end..emoji_end]).ok()?,
        })
    }

    /// 首个 `text` 不小于给定值的下标（lower bound）。
    ///
    /// 条目损坏时按「不小于」处理（收缩 hi）：宁可查不到也不要死循环或越界。
    fn lower_bound(&self, text: &str) -> u32 {
        let (mut lo, mut hi) = (0u32, self.entry_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.row(mid) {
                Some(r) if r.text < text => lo = mid + 1,
                _ => hi = mid,
            }
        }
        lo
    }

    /// 查该词的 emoji 串（**空格分隔**，调用方按需 `split_whitespace`）。
    ///
    /// 返回原始串而非 `Vec<String>`：查询在按键热路径上，每次只用得到前 `max_per_word` 个，
    /// 先整串借出、由调用方惰性切分，可完全避免这次分配。
    pub fn lookup(&self, text: &str) -> Option<&str> {
        let r = self.row(self.lower_bound(text))?;
        if r.text == text && !r.emojis.is_empty() {
            Some(r.emojis)
        } else {
            None
        }
    }
}

/// 构建 `.wemj`：排序 + **同键合并** + 写盘（tmp + rename 原子替换）。
///
/// # 同键合并（不是去重，更不是覆盖）
///
/// 同键有两个来源，**两者的后果完全不同，别把它们当成一回事**：
///
/// 1. **繁→简归一的撞键**（见 [`parse_upstream`]）。归一是多对一，原本不同的两行会撞到
///    同一个简体键：实测 4669 行 → 4657 键，12 组。但这 12 组**全是异体字对**
///    （煙/菸、台/臺、機/昇）指向同一个 emoji ⇒ 此处合并与覆盖结果相同，**当前数据下
///    看不出差别**。
/// 2. **两张上游表的同键**（`emoji_word.txt` × `emoji_category.txt`）。实测交集 14 个键，
///    且**14 个的内容全都不同**：「奖项」在 word 表只有 `🏅`，在 category 表是
///    `🏅 🎖️ 🥇 🥈 🥉 🏆`；「帽」`🧢` vs `👒 🧢 🎩 🎓 ⛑️ 🪖`。
///
/// ⚠️ **覆盖会在第 2 类上整组丢数据**。第 1 类今天恰好无害，但那是上游数据的巧合而非
/// 保证——判据只能是「合并」，不能是「反正现在一样」。合并保序去重：先到的排前面。
///
/// 排序用稳定的 `sort_by`：组内相对顺序若不稳定，「先到先得」会随输入规模抖动
/// （`sort_unstable_by` 在小数组走插入排序恰好稳定，大数组不稳定，测试因此抓不到）。
/// 这条与 [`crate::commentdict::write_comment_wcmt`] 是同一个教训。
pub fn write_emoji_wemj(
    path: impl AsRef<Path>,
    rows: &[(String, Vec<String>)],
) -> anyhow::Result<()> {
    use std::io::Write;

    let mut sorted: Vec<&(String, Vec<String>)> = rows.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut pool: Vec<u8> = Vec::new();
    let mut index: Vec<u8> = Vec::with_capacity(sorted.len() * ENTRY_SIZE);
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut merged = 0usize;

    let mut i = 0usize;
    while i < sorted.len() {
        // 同 text 组：[i, j)
        let mut j = i;
        while j < sorted.len() && sorted[j].0 == sorted[i].0 {
            j += 1;
        }
        if j - i > 1 {
            merged += 1;
        }
        // 组内合并 emoji：保序去重
        let text = sorted[i].0.as_str();
        let mut emojis: Vec<&str> = Vec::new();
        for (_, list) in sorted[i..j].iter().copied() {
            for e in list {
                let e = e.as_str();
                if !e.is_empty() && !emojis.contains(&e) {
                    emojis.push(e);
                }
            }
        }
        // ⚠️ 必须在下面任何 `continue` 之前推进，否则空条目会让循环卡死。
        i = j;

        if text.is_empty() || emojis.is_empty() {
            continue;
        }
        let joined = emojis.join(" ");
        if text.len() > u16::MAX as usize || joined.len() > u16::MAX as usize {
            skipped += 1;
            continue;
        }
        if pool.len() > u32::MAX as usize {
            anyhow::bail!("emoji dict string pool exceeds 4GB");
        }
        let off = pool.len() as u32;
        pool.extend_from_slice(text.as_bytes());
        pool.extend_from_slice(joined.as_bytes());
        index.extend_from_slice(&off.to_le_bytes());
        index.extend_from_slice(&(text.len() as u16).to_le_bytes());
        index.extend_from_slice(&(joined.len() as u16).to_le_bytes());
        written += 1;
    }
    if skipped > 0 {
        warn!("emoji 表有 {} 条词条/值超长（>64KB），已跳过", skipped);
    }

    let index_off = HEADER_SIZE as u32;
    let str_off = index_off + index.len() as u32;

    let tmp = path.as_ref().with_extension("wemj.tmp");
    if let Some(dir) = tmp.parent() {
        std::fs::create_dir_all(dir)?;
    }
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&MAGIC)?;
        f.write_all(&VERSION.to_le_bytes())?;
        f.write_all(&(written as u32).to_le_bytes())?;
        f.write_all(&index_off.to_le_bytes())?;
        f.write_all(&str_off.to_le_bytes())?;
        f.write_all(&0u32.to_le_bytes())?; // reserved
        f.write_all(&index)?;
        f.write_all(&pool)?;
    }
    std::fs::rename(&tmp, path.as_ref())?;
    info!(
        "Wrote emoji dict: {} entries ({} merged by key)",
        written, merged
    );
    Ok(())
}

/// 解析上游 rime-emoji 词表（`emoji_word.txt` / `emoji_category.txt`），键经 `normalize` 归一。
///
/// # 上游格式契约
///
/// 行形如 `键<TAB>键 emoji1 emoji2 …`，且上游 `check.py` 强制**值的首项与键完全相同**
/// （正则 `(\S+)\t(\S+)( \S+)+` 且 `group(1) == group(2)`）。这是 OpenCC `simplifier`
/// 组件的要求：它把值当作候选集，rime 跳过与原文相同的那个，其余作为新候选。
///
/// ⇒ 我们**丢弃值的首项**，只取其后的 emoji。⚠️ 不能改成「过滤掉等于键的项」：归一之后
/// 键已经是简体，而首项仍是原繁体，两者不再相等，那样写会把首项当成 emoji 收进去。
/// 判据必须是**位置**（首项），不是**内容**。
///
/// `normalize` 由调用方注入（繁→简），因为本 crate 不依赖 `wind-transform`。传恒等闭包
/// 即为「不归一」。
pub fn parse_upstream(
    content: &str,
    normalize: impl Fn(&str) -> String,
) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(tab) = line.find('\t') else {
            continue;
        };
        let key = &line[..tab];
        if key.is_empty() {
            continue;
        }
        // 值区按空格切分，**跳过首项**（上游契约：首项 == 键）。
        let emojis: Vec<String> = line[tab + 1..]
            .split(' ')
            .filter(|v| !v.is_empty())
            .skip(1)
            .map(str::to_string)
            .collect();
        if emojis.is_empty() {
            continue;
        }
        out.push((normalize(key), emojis));
    }
    out
}

/// 加载 emoji 扩展表：优先 mmap `.wemj` 缓存，不新鲜则从上游 txt 重建。
///
/// - `tables`：要解析的上游文本表，**按顺序合并**（先 `emoji_word.txt` 后
///   `emoji_category.txt`）。顺序即优先级——同键时精确词表的 emoji 排在分类表之前。
///   不存在的文件跳过（分类表是可选的，出厂关）。
/// - `fp_extra`：**只参与指纹、不参与解析**的源。归一表（`TSCharactersDerived.octrie`）
///   必须放这里：本表的产出取决于它，换一版归一表就得重建，而它不是要解析的词表。
///   漏传的后果是 OpenCC 数据升级后旧缓存被永久复用（见 [`cache_fp::EMOJI_TAG`]）。
///
/// # 为什么失败不降级成内存表
///
/// 注释库在缓存这条路走不通时会退回内存表（「功能继续，只是这一库常驻内存」）。**本表
/// 刻意不这样做**：emoji 扩展是纯锦上添花的可选功能，而「常驻内存随功能数量累积」正是
/// 本设计要避开的东西（见模块文档与 `docs/design/emoji-suggestion.md` §3.4）。缓存建不了
/// （目录只读、磁盘满）就让这个功能不可用并告警，不偷偷占着内存。
pub fn load_or_build(
    tables: &[std::path::PathBuf],
    fp_extra: &[std::path::PathBuf],
    cache_file: &Path,
    normalize: impl Fn(&str) -> String,
) -> Option<std::sync::Arc<EmojiReader>> {
    use crate::{cache_fp, reader_pool};

    // 指纹覆盖「解析的源」+「影响解析结果的源」两部分，缺一都会让缓存该重建时不重建。
    let fp_sources: Vec<&Path> = tables
        .iter()
        .chain(fp_extra.iter())
        .map(std::path::PathBuf::as_path)
        .collect();

    // single-flight：同一缓存文件的构建区间互斥（同 `load_comment_source`）。
    let lock = reader_pool::file_lock(cache_file);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    if cache_fp::cache_is_fresh(cache_file, &fp_sources, cache_fp::EMOJI_TAG)
        && let Ok(r) = reader_pool::open_emoji(cache_file)
    {
        return Some(r);
    }

    let mut rows: Vec<(String, Vec<String>)> = Vec::new();
    for t in tables {
        if !t.exists() {
            continue; // 分类表可选，缺了不算失败
        }
        match std::fs::read_to_string(t) {
            Ok(s) => rows.extend(parse_upstream(&s, &normalize)),
            Err(e) => warn!("读取 emoji 表失败 {}: {}", t.display(), e),
        }
    }
    if rows.is_empty() {
        warn!("emoji 表为空或全部读取失败，扩展功能不可用");
        return None;
    }

    if let Err(e) = write_emoji_wemj(cache_file, &rows) {
        warn!("emoji 缓存构建失败 {}: {}", cache_file.display(), e);
        return None;
    }
    cache_fp::write_cache_fp(cache_file, &fp_sources, cache_fp::EMOJI_TAG);
    match reader_pool::open_emoji(cache_file) {
        Ok(r) => Some(r),
        Err(e) => {
            warn!("emoji 缓存写成但打不开 {}: {}", cache_file.display(), e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        v.iter()
            .map(|(t, es)| {
                (
                    t.to_string(),
                    es.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    fn write_read(v: &[(&str, &[&str])]) -> (tempdir::Guard, EmojiReader) {
        let g = tempdir::Guard::new("wemj");
        let p = g.path().join("t.wemj");
        write_emoji_wemj(&p, &rows(v)).unwrap();
        let r = EmojiReader::open(&p).unwrap();
        (g, r)
    }

    /// 写入 → mmap 读回 → 精确点查，三段来回一致。
    #[test]
    fn roundtrip_lookup() {
        let (_g, r) = write_read(&[("你好", &["😊", "👋"]), ("足球", &["⚽"])]);
        assert_eq!(r.entry_count(), 2);
        assert_eq!(r.lookup("你好"), Some("😊 👋"));
        assert_eq!(r.lookup("足球"), Some("⚽"));
        assert_eq!(r.lookup("没有"), None, "未收录的词返回 None");
    }

    /// ★ 同键**必须合并**，不是后者覆盖前者。
    ///
    /// 真实场景取自两张上游表的同键（`emoji_word.txt` × `emoji_category.txt`，实测交集
    /// 14 个键且内容全不同）：「奖项」在 word 表只有 `🏅`，在 category 表是一整组。
    /// 覆盖式实现会把其中一整组整个丢掉。
    ///
    /// ⚠️ 繁→简撞键那 12 组恰好是异体字指向同一 emoji，覆盖与合并结果相同——**别拿
    /// 那个场景来验这条**，它对错误实现没有判断力。
    #[test]
    fn colliding_keys_merge_not_overwrite() {
        let (_g, r) = write_read(&[("奖项", &["🏅"]), ("奖项", &["🏅", "🎖️", "🥇"])]);
        assert_eq!(r.entry_count(), 1, "同键合并成一条");
        assert_eq!(
            r.lookup("奖项"),
            Some("🏅 🎖️ 🥇"),
            "两侧 emoji 都在、保序、且重复项去掉"
        );
    }

    /// 合并保**序**：先到的排前面（与 `sort_by` 的稳定性一同构成「先到先得」）。
    #[test]
    fn merge_preserves_first_seen_order() {
        let (_g, r) = write_read(&[("x", &["1️⃣", "2️⃣"]), ("x", &["3️⃣", "1️⃣"])]);
        assert_eq!(r.lookup("x"), Some("1️⃣ 2️⃣ 3️⃣"));
    }

    /// 上游格式：值的首项是键本身，**必须按位置丢弃**。
    #[test]
    fn parse_upstream_drops_first_value() {
        let got = parse_upstream("一個人\t一個人 👤\nID\tID 🆔 🪪\n", str::to_string);
        assert_eq!(
            got,
            vec![
                ("一個人".to_string(), vec!["👤".to_string()]),
                ("ID".to_string(), vec!["🆔".to_string(), "🪪".to_string()]),
            ]
        );
    }

    /// ⚠️ 归一后键与首项不再相等 —— 若把「丢首项」误写成「过滤等于键的项」，
    /// 繁体首项会被当成 emoji 收进去。本例即那个错误实现的指纹。
    #[test]
    fn parse_upstream_drops_by_position_not_by_equality() {
        // 归一：把「個」换成「个」，于是键 = 一个人，而值首项仍是 一個人。
        let norm = |s: &str| s.replace('個', "个");
        let got = parse_upstream("一個人\t一個人 👤\n", norm);
        assert_eq!(got, vec![("一个人".to_string(), vec!["👤".to_string()])]);
    }

    /// 空行、注释、无 tab 行、无 emoji 行一律跳过，不产出空条目。
    #[test]
    fn parse_upstream_skips_malformed() {
        let got = parse_upstream(
            "\n# 注释\n没有制表符\n只有键\t只有键\n好\t好 🙂\n",
            str::to_string,
        );
        assert_eq!(got, vec![("好".to_string(), vec!["🙂".to_string()])]);
    }

    /// 空表可写可读（功能出厂关闭时的正常状态，不该是错误）。
    #[test]
    fn empty_table_is_valid() {
        let (_g, r) = write_read(&[]);
        assert!(r.is_empty());
        assert_eq!(r.lookup("任何"), None);
    }

    /// 截断的缓存文件不 panic：要么开不了，要么查不到，都不许崩。
    #[test]
    fn truncated_file_does_not_panic() {
        let g = tempdir::Guard::new("wemj-trunc");
        let p = g.path().join("t.wemj");
        write_emoji_wemj(&p, &rows(&[("你好", &["😊"])])).unwrap();
        let full = std::fs::read(&p).unwrap();
        // 砍掉字符串池的一半：头部与索引仍然自洽，但 row() 会越界。
        std::fs::write(&p, &full[..full.len() - 3]).unwrap();
        // 开不了也是可接受的降级；开得了就必须查不崩。
        if let Ok(r) = EmojiReader::open(&p) {
            let _ = r.lookup("你好");
            let _ = r.lookup("");
        }
    }

    // ── load_or_build（建缓存编排）─────────────────────────────────────────

    /// 多张表**按顺序**合并：精确词表的 emoji 排在分类表之前。
    #[test]
    fn build_merges_tables_in_order() {
        let g = tempdir::Guard::new("wemj-order");
        let (w, c) = (g.path().join("word.txt"), g.path().join("cat.txt"));
        std::fs::write(&w, "奖项\t奖项 🏅\n").unwrap();
        std::fs::write(&c, "奖项\t奖项 🎖️ 🥇\n").unwrap();
        let r =
            load_or_build(&[w, c], &[], &g.path().join("e.wemj"), str::to_string).expect("应建成");
        assert_eq!(
            r.lookup("奖项"),
            Some("🏅 🎖️ 🥇"),
            "word 表在前，分类表在后"
        );
    }

    /// 不存在的表跳过（分类表可选），不算失败。
    #[test]
    fn build_skips_missing_table() {
        let g = tempdir::Guard::new("wemj-missing");
        let w = g.path().join("word.txt");
        std::fs::write(&w, "好\t好 🙂\n").unwrap();
        let r = load_or_build(
            &[w, g.path().join("不存在.txt")],
            &[],
            &g.path().join("e.wemj"),
            str::to_string,
        )
        .expect("缺可选表不该失败");
        assert_eq!(r.lookup("好"), Some("🙂"));
    }

    /// 全部表都读不到 ⇒ None，且**不留下缓存文件**（不能让空表被后续当成新鲜缓存复用）。
    #[test]
    fn build_returns_none_when_no_source() {
        let g = tempdir::Guard::new("wemj-none");
        let cache = g.path().join("e.wemj");
        assert!(load_or_build(&[g.path().join("无.txt")], &[], &cache, str::to_string).is_none());
        assert!(!cache.exists(), "没建成就不该留下缓存文件");
    }

    /// ★★ 归一表必须参与指纹：`fp_extra` 变了就得重建。
    ///
    /// 这是本编排最容易漏的一条——归一表不是要解析的词表，很自然就不会被放进 sources，
    /// 于是 OpenCC 数据升级后，同样的 emoji_word.txt 会**永久复用按旧归一表建的缓存**。
    /// 症状是「换了转换表但某些词还是打不出 emoji」，而源文件确实没动过，无从查起。
    #[test]
    fn rebuild_when_normalizer_source_changes() {
        let g = tempdir::Guard::new("wemj-fp");
        let (w, norm_tbl) = (g.path().join("word.txt"), g.path().join("t2s.bin"));
        let cache = g.path().join("e.wemj");
        std::fs::write(&w, "個\t個 👤\n").unwrap();
        std::fs::write(&norm_tbl, b"v1").unwrap();

        // 第一次：归一为恒等 ⇒ 键是「個」。
        let r = load_or_build(
            std::slice::from_ref(&w),
            std::slice::from_ref(&norm_tbl),
            &cache,
            str::to_string,
        )
        .unwrap();
        assert_eq!(r.lookup("個"), Some("👤"));
        drop(r); // 释放 mmap，Windows 上 rename 覆盖才不会 Access Denied

        // 只改归一表内容（源词表一个字节没动），并换用真的会归一的闭包。
        std::fs::write(&norm_tbl, b"v2").unwrap();
        let r2 = load_or_build(&[w], &[norm_tbl], &cache, |s| s.replace('個', "个")).unwrap();
        assert_eq!(r2.lookup("个"), Some("👤"), "指纹变了 ⇒ 重建 ⇒ 归一生效");
        assert_eq!(r2.lookup("個"), None, "旧键不该还在");
    }

    /// 源与指纹都没变时**复用**缓存，不重建。
    ///
    /// ⚠️ 判据不能是「缓存文件大小/mtime 没变」——重建会写出内容与长度都完全相同的文件，
    /// 那种断言对「每次都重建」的实现毫无判断力。这里改成**把缓存内容偷偷换掉**：指纹只
    /// 按源文件算、不看缓存内容，所以缓存仍判新鲜；此时查到替换后的内容即证明走的是缓存，
    /// 查到源文件的内容则证明重建了。
    #[test]
    fn reuses_fresh_cache() {
        let g = tempdir::Guard::new("wemj-fresh");
        let w = g.path().join("word.txt");
        let cache = g.path().join("e.wemj");
        std::fs::write(&w, "好\t好 🙂\n").unwrap();
        let r = load_or_build(std::slice::from_ref(&w), &[], &cache, str::to_string).unwrap();
        assert_eq!(r.lookup("好"), Some("🙂"));
        drop(r); // 释放 mmap，Windows 上才能覆盖

        // 换成一份内容不同的合法缓存（源文件与 .fp 都不动）。
        write_emoji_wemj(&cache, &rows(&[("好", &["🎯"])])).unwrap();
        let r2 = load_or_build(&[w], &[], &cache, str::to_string).unwrap();
        assert_eq!(r2.lookup("好"), Some("🎯"), "走的是缓存，没有重建");
    }

    /// 极小的临时目录守卫：测试结束即删，避免污染工作区。
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

        static SEQ: AtomicU32 = AtomicU32::new(0);

        pub struct Guard(PathBuf);

        impl Guard {
            pub fn new(tag: &str) -> Self {
                let n = SEQ.fetch_add(1, Ordering::Relaxed);
                let p = std::env::temp_dir()
                    .join(format!("windinput-{tag}-{}-{n}", std::process::id()));
                std::fs::create_dir_all(&p).unwrap();
                Guard(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
