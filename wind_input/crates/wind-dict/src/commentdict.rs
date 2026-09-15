//! `.wcmt`：mmap 零拷贝的候选注释表（词 → 注释）
//!
//! 注释库由用户自行挂载，容量**不可预知**——可能是几百条 emoji 名称，也可能是十万条级
//! 的英汉词典或字义库。全解析进内存意味着容量直接变成常驻内存，而注释是个可选的展示
//! 功能，不该为它付这份代价。故与 `.wdat` 同样走 mmap：页按需载入，常驻内存与库大小
//! 基本无关（索引结构不同，见下）。
//!
//! # 与 `.wdat` 的区别：为什么这里没有 DAT
//!
//! 主词库要按**前缀**逐键检索百万条，所以建双数组 trie（构建以秒计，这也正是 wdat 缓存
//! 存在的理由）。注释只做**精确点查**，且每次只查当前页那 5~9 条候选，排序数组 + 二分
//! 就够，构建成本因此低两个数量级。两者共用「mmap + 内容指纹缓存」的骨架，索引结构不同。
//!
//! # 文件布局
//!
//! - Header (24B)：magic `WCMT` + version u32 + entry_count u32 + index_off u32
//!   + str_off u32 + reserved u32
//! - Entry[entry_count] (12B 每条，**按 text 升序稳定排序**)：
//!   off u32（相对 str_off）+ text_len u16 + code_len u16 + comment_len u32
//! - StringPool：每条连续存 `text | comment | code` 三段
//!
//! 同 text 的多条相邻，组内保持**挂载顺序**（供 code 消歧与「先到先得」）。

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use tracing::{info, warn};

const MAGIC: [u8; 4] = *b"WCMT";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 12;

/// 注释表 mmap 读取器。
pub struct CommentReader {
    mmap: Mmap,
    entry_count: u32,
    index_off: u32,
    str_off: u32,
}

/// 一条注释记录的三段文本（借用自 mmap，零拷贝）。
struct Row<'a> {
    text: &'a str,
    comment: &'a str,
    code: &'a str,
}

impl CommentReader {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < HEADER_SIZE {
            anyhow::bail!("comment wcmt too short");
        }
        if mmap[0..4] != MAGIC {
            anyhow::bail!("invalid comment magic");
        }
        let version = u32::from_le_bytes(mmap[4..8].try_into().unwrap());
        if version != VERSION {
            anyhow::bail!("unsupported comment version: {}", version);
        }
        let entry_count = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        let index_off = u32::from_le_bytes(mmap[12..16].try_into().unwrap());
        let str_off = u32::from_le_bytes(mmap[16..20].try_into().unwrap());

        let index_end = index_off as usize + entry_count as usize * ENTRY_SIZE;
        if index_end > mmap.len() || str_off as usize > mmap.len() {
            anyhow::bail!("comment wcmt offsets out of range");
        }
        info!(
            "Opened comment dict: {} ({} entries)",
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

    /// 读第 `i` 条的三段文本。越界 / UTF-8 损坏 → None（当作该条不存在，不 panic：
    /// 缓存文件可能被外部破坏，功能降级好过崩进程）。
    fn row(&self, i: u32) -> Option<Row<'_>> {
        let off = self.index_off as usize + i as usize * ENTRY_SIZE;
        if off + ENTRY_SIZE > self.mmap.len() {
            return None;
        }
        let str_start = u32::from_le_bytes(self.mmap[off..off + 4].try_into().ok()?) as usize;
        let text_len = u16::from_le_bytes(self.mmap[off + 4..off + 6].try_into().ok()?) as usize;
        let code_len = u16::from_le_bytes(self.mmap[off + 6..off + 8].try_into().ok()?) as usize;
        let comment_len =
            u32::from_le_bytes(self.mmap[off + 8..off + 12].try_into().ok()?) as usize;

        let base = self.str_off as usize + str_start;
        let text_end = base + text_len;
        let comment_end = text_end + comment_len;
        let code_end = comment_end + code_len;
        if code_end > self.mmap.len() {
            return None;
        }
        Some(Row {
            text: std::str::from_utf8(&self.mmap[base..text_end]).ok()?,
            comment: std::str::from_utf8(&self.mmap[text_end..comment_end]).ok()?,
            code: std::str::from_utf8(&self.mmap[comment_end..code_end]).ok()?,
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

    /// 遍历 `text` 对应的连续条目组（按挂载顺序）。
    fn group(&self, text: &str) -> impl Iterator<Item = Row<'_>> {
        let start = self.lower_bound(text);
        (start..self.entry_count)
            .map(move |i| self.row(i))
            .take_while(move |r| matches!(r, Some(x) if x.text == text))
            .flatten()
    }

    /// 该词在本库的首条注释（组内挂载顺序在前者）。空注释视为未命中。
    pub fn lookup_first(&self, text: &str) -> Option<&str> {
        self.group(text).map(|r| r.comment).find(|c| !c.is_empty())
    }

    /// 该词中 `code` **精确匹配**的注释。用于方案内消歧（注释库声明了 `code` 列时）。
    pub fn lookup_by_code(&self, text: &str, code: &str) -> Option<&str> {
        if code.is_empty() {
            return None;
        }
        self.group(text)
            .find(|r| r.code == code)
            .map(|r| r.comment)
            .filter(|c| !c.is_empty())
    }
}

/// 构建 `.wcmt`：排序 + 同 `(text, code)` 去重 + 写盘（tmp + rename 原子替换）。
///
/// **入参顺序即优先级**：同 `(text, code)` 重复时保留**首次**出现的那条，于是同一个库内
/// 靠前的行胜出。排序用稳定的 `sort_by`——组内相对顺序若不稳定，「先到先得」会随输入
/// 规模抖动（`sort_unstable_by` 在小数组走插入排序恰好稳定，大数组不稳定，测试因此
/// 抓不到）。
pub fn write_comment_wcmt(
    path: impl AsRef<Path>,
    rows: &[(String, String, String)],
) -> anyhow::Result<()> {
    use std::io::Write;

    let mut sorted: Vec<&(String, String, String)> = rows.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut pool: Vec<u8> = Vec::new();
    let mut index: Vec<u8> = Vec::with_capacity(sorted.len() * ENTRY_SIZE);
    let mut written = 0usize;
    let mut skipped = 0usize;

    let mut i = 0usize;
    while i < sorted.len() {
        // 同 text 组：[i, j)
        let mut j = i;
        while j < sorted.len() && sorted[j].0 == sorted[i].0 {
            j += 1;
        }
        // 组内按 code 去重，保留首次出现者
        let mut seen_codes: Vec<&str> = Vec::new();
        for (text, comment, code) in sorted[i..j].iter().copied() {
            if seen_codes.contains(&code.as_str()) {
                continue;
            }
            seen_codes.push(code);
            if text.len() > u16::MAX as usize || code.len() > u16::MAX as usize {
                skipped += 1;
                continue;
            }
            if pool.len() > u32::MAX as usize {
                anyhow::bail!("comment dict string pool exceeds 4GB");
            }
            let off = pool.len() as u32;
            pool.extend_from_slice(text.as_bytes());
            pool.extend_from_slice(comment.as_bytes());
            pool.extend_from_slice(code.as_bytes());
            index.extend_from_slice(&off.to_le_bytes());
            index.extend_from_slice(&(text.len() as u16).to_le_bytes());
            index.extend_from_slice(&(code.len() as u16).to_le_bytes());
            index.extend_from_slice(&(comment.len() as u32).to_le_bytes());
            written += 1;
        }
        i = j;
    }
    if skipped > 0 {
        warn!("注释库有 {} 条词条/编码超长（>64KB），已跳过", skipped);
    }

    let index_off = HEADER_SIZE as u32;
    let str_off = index_off + index.len() as u32;

    let tmp = path.as_ref().with_extension("wcmt.tmp");
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
    // 见 `reader_pool::replacing`：池中可能还有指向替换前数据的 mmap reader。
    let replacing = crate::reader_pool::replacing(path.as_ref());
    let renamed = std::fs::rename(&tmp, path.as_ref());
    drop(replacing);
    renamed?;
    info!("Wrote comment dict: {} entries", written);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
        v.iter()
            .map(|(t, c, k)| (t.to_string(), c.to_string(), k.to_string()))
            .collect()
    }

    fn build(tag: &str, v: &[(&str, &str, &str)]) -> (std::path::PathBuf, CommentReader) {
        let p = std::env::temp_dir().join(format!("wind_wcmt_{}_{}.wcmt", std::process::id(), tag));
        write_comment_wcmt(&p, &rows(v)).unwrap();
        let r = CommentReader::open(&p).unwrap();
        (p, r)
    }

    #[test]
    fn roundtrip_and_binary_search() {
        // 乱序输入，验证写入端排序 + 读取端二分
        let (p, r) = build(
            "basic",
            &[
                ("龘", "dá 龙飞的样子", ""),
                ("一", "yī 数词", ""),
                ("中国", "China", ""),
                ("啊", "叹词", ""),
            ],
        );
        assert_eq!(r.entry_count(), 4);
        assert_eq!(r.lookup_first("中国"), Some("China"));
        assert_eq!(r.lookup_first("一"), Some("yī 数词"));
        assert_eq!(r.lookup_first("龘"), Some("dá 龙飞的样子"));
        assert_eq!(r.lookup_first("不存在"), None);
        // 边界：小于最小键 / 大于最大键
        assert_eq!(r.lookup_first("\u{1}"), None);
        assert_eq!(r.lookup_first("\u{10FFFF}"), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn code_disambiguation_and_fallback() {
        let (p, r) = build(
            "code",
            &[
                ("行", "háng 行列", "tfhh"),
                ("行", "xíng 行走", "hang"),
                ("好", "hǎo", ""),
            ],
        );
        assert_eq!(r.lookup_by_code("行", "tfhh"), Some("háng 行列"));
        assert_eq!(r.lookup_by_code("行", "hang"), Some("xíng 行走"));
        // code 对不上 → 由调用方回落 lookup_first（跨方案挂同一份库是常态）
        assert_eq!(r.lookup_by_code("行", "wubi_no_such"), None);
        assert_eq!(r.lookup_first("行"), Some("háng 行列"), "回落取组内首条");
        // 空 code 不参与消歧
        assert_eq!(r.lookup_by_code("好", ""), None);
        let _ = std::fs::remove_file(&p);
    }

    /// 「先到先得」：同 (text, code) 重复时保留首次出现者，且必须在**大数组**上也成立
    /// （小数组下 sort_unstable 恰好稳定，只测三五条抓不到排序不稳定的 bug）。
    #[test]
    fn duplicate_keeps_first_at_scale() {
        let mut v: Vec<(String, String, String)> = Vec::new();
        for i in 0..2000 {
            v.push((format!("词{i:04}"), format!("第{i}条"), String::new()));
        }
        // 在末尾追加与前面全部重复的行（后到者），它们不应覆盖已有注释
        for i in 0..2000 {
            v.push((format!("词{i:04}"), "后到的".into(), String::new()));
        }
        let p = std::env::temp_dir().join(format!("wind_wcmt_dup_{}.wcmt", std::process::id()));
        write_comment_wcmt(&p, &v).unwrap();
        let r = CommentReader::open(&p).unwrap();
        assert_eq!(r.entry_count(), 2000, "同词同码只留一条");
        assert_eq!(r.lookup_first("词0000"), Some("第0条"));
        assert_eq!(r.lookup_first("词1999"), Some("第1999条"));
        let _ = std::fs::remove_file(&p);
    }

    /// 多字节键的二分：比较按 UTF-8 字节序，与写入端 `String::cmp` 必须一致。
    /// 混入扩展区汉字（4 字节）——若哪一端按 char 数或按 UTF-16 比较，这里会错位。
    #[test]
    fn multibyte_keys_compare_consistently() {
        let (p, r) = build(
            "multibyte",
            &[
                ("𠮷", "土 + 口（扩展 B）", ""),
                ("吉", "吉利", ""),
                ("a", "拉丁", ""),
                ("Ω", "欧米伽", ""),
                ("🀄", "麻将", ""),
            ],
        );
        for (k, want) in [
            ("𠮷", "土 + 口（扩展 B）"),
            ("吉", "吉利"),
            ("a", "拉丁"),
            ("Ω", "欧米伽"),
            ("🀄", "麻将"),
        ] {
            assert_eq!(r.lookup_first(k), Some(want), "键 {k} 应可查到");
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn empty_dict_is_valid() {
        let (p, r) = build("empty", &[]);
        assert!(r.is_empty());
        assert_eq!(r.lookup_first("任意"), None);
        let _ = std::fs::remove_file(&p);
    }

    /// 非 wcmt 文件必须被拒绝而非当成空表——否则用户把路径写错时毫无提示。
    #[test]
    fn rejects_foreign_file() {
        let p = std::env::temp_dir().join(format!("wind_wcmt_bad_{}.wcmt", std::process::id()));
        std::fs::write(&p, b"not a wcmt file at all, padding to exceed header size").unwrap();
        assert!(CommentReader::open(&p).is_err());
        // 过短文件同样拒绝
        std::fs::write(&p, b"WCMT").unwrap();
        assert!(CommentReader::open(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
