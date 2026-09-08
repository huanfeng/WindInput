//! 二进制词典格式 (wdb) + mmap 零拷贝查询
//!
//! 与 Go 版本 `wind_input/internal/dict/binformat/` 对齐。
//! 支持 V2（10字节条目）和 V3（14字节条目，含 Order 字段）。

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use tracing::info;

/// wdb 魔数 "WDIC"
const MAGIC: [u8; 4] = *b"WDIC";

/// 原子写临时文件序号：并发写同一目标时避免 `.tmp` 文件名冲突（对齐 Go atomicWriteSeq）。
static ATOMIC_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// wdb 文件头 (32 bytes)
#[derive(Debug, Clone)]
#[repr(C, packed)]
pub struct DictFileHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub key_count: u32,
    pub index_off: u32,
    pub data_off: u32,
    pub str_off: u32,
    pub abbrev_off: u32,
    pub meta_off: u32,
}

impl DictFileHeader {
    pub const SIZE: usize = 32;

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            magic: [buf[0], buf[1], buf[2], buf[3]],
            version: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            key_count: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            index_off: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            data_off: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            str_off: u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
            abbrev_off: u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]),
            meta_off: u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
        })
    }
}

/// 键索引条目 (12 bytes)
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct DictKeyIndex {
    pub code_off: u32,
    pub code_len: u16,
    pub entry_off: u32,
    pub entry_len: u16,
}

impl DictKeyIndex {
    pub const SIZE: usize = 12;

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            code_off: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            code_len: u16::from_le_bytes([buf[4], buf[5]]),
            entry_off: u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]),
            entry_len: u16::from_le_bytes([buf[10], buf[11]]),
        })
    }
}

/// 条目记录
#[derive(Debug, Clone)]
pub struct EntryRecord {
    pub text: String,
    pub weight: i32,
    pub order: i32,
}

/// 词典查询结果
#[derive(Debug, Clone)]
pub struct DictEntry {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub order: i32,
    /// 音节边界：`code` 中各音节**起始字节位**的 bitmask（bit i 置位 = 第 i 字节是音节开头）。
    /// 如「你好」code="nihao"、音节 ni|hao → 起始位 {0,2} → `0b101`。
    ///
    /// 语义来自 rime 源数据 `你好\tni hao` 里的空格——那个空格**就是**音节边界，是真值，
    /// 无需靠 DAG 猜（`xian` 到底是 xi'an 还是 xian，DAG 覆盖数相同、无从分辨，词典知道）。
    ///
    /// `0` = **无边界信息**（非拼音码如五笔、code 超 64 字节、旧格式回退），消费方须降级回 DAG。
    pub boundary: u64,
}

/// 二进制词典读取器（mmap 零拷贝）
pub struct DictReader {
    mmap: Mmap,
    header: DictFileHeader,
    entry_size: usize,
}

impl DictReader {
    /// 打开 wdb 文件
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };

        let header = DictFileHeader::from_bytes(&mmap)
            .ok_or_else(|| anyhow::anyhow!("invalid wdb file: too short"))?;

        if header.magic != MAGIC {
            anyhow::bail!("invalid wdb magic: expected WDIC, got {:?}", header.magic);
        }

        let version = header.version;
        let key_count = header.key_count;
        let file_len = mmap.len();

        let entry_size = match version {
            3 => 14,
            2 | 1 => 10,
            _ => anyhow::bail!("unsupported wdb version: {}", version),
        };

        // 校验 header 中各段偏移是否在文件范围内
        let index_off = header.index_off as usize;
        let index_end = index_off + key_count as usize * DictKeyIndex::SIZE;
        if index_end > file_len {
            anyhow::bail!(
                "wdb index section out of range: index_end={} > file_len={} \
                 (key_count={}, index_off={}). File may be from an incompatible format",
                index_end,
                file_len,
                key_count,
                index_off
            );
        }

        info!(
            "Opened wdb: {} ({} keys, v{})",
            path.display(),
            key_count,
            version
        );

        Ok(Self {
            mmap,
            header,
            entry_size,
        })
    }

    fn data(&self) -> &[u8] {
        &self.mmap
    }

    fn read_string(&self, off: u32, len: u16) -> &str {
        let start = self.header.str_off as usize + off as usize;
        let end = start + len as usize;
        if end > self.data().len() {
            return "";
        }
        std::str::from_utf8(&self.data()[start..end]).unwrap_or("")
    }

    fn read_key_index(&self, i: u32) -> Option<DictKeyIndex> {
        let offset = self.header.index_off as usize + (i as usize) * DictKeyIndex::SIZE;
        if offset + DictKeyIndex::SIZE > self.data().len() {
            return None;
        }
        DictKeyIndex::from_bytes(&self.data()[offset..])
    }

    fn read_entry(&self, entry_off: u32, entry_idx: u16) -> Option<EntryRecord> {
        let base = self.header.data_off as usize + entry_off as usize;
        let offset = base + (entry_idx as usize) * self.entry_size;
        let data = self.data();
        // 起点先于切片做边界校验：offset 可能已超出 mmap 长度（截断/半写的 .wdb），
        // 直接 &data[offset..] 会因起点越界 panic（range start index out of range）。
        if offset > data.len() || data.len() - offset < self.entry_size {
            return None;
        }
        let buf = &data[offset..];

        let text_off = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let text_len = u16::from_le_bytes([buf[4], buf[5]]);
        let weight = i32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);

        let order = if self.entry_size >= 14 {
            i32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]])
        } else {
            entry_idx as i32
        };

        let text = self.read_string(text_off, text_len).to_string();
        Some(EntryRecord {
            text,
            weight,
            order,
        })
    }

    /// 精确查找：二分搜索键索引
    pub fn search(&self, code: &str) -> Vec<DictEntry> {
        let key_count = self.header.key_count;
        if key_count == 0 {
            return Vec::new();
        }

        let mut lo = 0u32;
        let mut hi = key_count;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if let Some(idx) = self.read_key_index(mid) {
                let mid_code = self.read_string(idx.code_off, idx.code_len);
                if mid_code < code {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            } else {
                break;
            }
        }

        if lo < key_count
            && let Some(idx) = self.read_key_index(lo)
        {
            let found_code = self.read_string(idx.code_off, idx.code_len);
            if found_code == code {
                return self.collect_entries(&idx, code);
            }
        }
        Vec::new()
    }

    /// 前缀查找
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<DictEntry> {
        let key_count = self.header.key_count;
        if key_count == 0 {
            return Vec::new();
        }

        let mut lo = 0u32;
        let mut hi = key_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if let Some(idx) = self.read_key_index(mid) {
                let mid_code = self.read_string(idx.code_off, idx.code_len);
                if mid_code < prefix {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            } else {
                break;
            }
        }

        let mut results = Vec::new();
        let mut i = lo;
        while i < key_count && results.len() < limit {
            if let Some(idx) = self.read_key_index(i) {
                let code = self.read_string(idx.code_off, idx.code_len);
                if !code.starts_with(prefix) {
                    break;
                }
                results.extend(self.collect_entries(&idx, code));
            }
            i += 1;
        }

        results.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.order.cmp(&b.order)));
        results.truncate(limit);
        results
    }

    fn collect_entries(&self, idx: &DictKeyIndex, code: &str) -> Vec<DictEntry> {
        let mut entries = Vec::with_capacity(idx.entry_len as usize);
        for j in 0..idx.entry_len {
            if let Some(rec) = self.read_entry(idx.entry_off, j) {
                entries.push(DictEntry {
                    code: code.to_string(),
                    text: rec.text,
                    weight: rec.weight,
                    order: rec.order,
                    // wdb（旧格式，拼音侧已不用）无音节边界字段 → 0 表无信息，消费方降级回 DAG。
                    boundary: 0,
                });
            }
        }
        entries
    }

    pub fn key_count(&self) -> u32 {
        self.header.key_count
    }

    /// 遍历全部条目(供反查索引构建):对每个 (code, text, weight) 调用 `f`。
    /// 顺序为键索引顺序(码字典序);截断/半写的键索引处提前结束。
    pub fn for_each_entry(&self, f: &mut dyn FnMut(&str, &str, i32)) {
        for i in 0..self.header.key_count {
            let Some(idx) = self.read_key_index(i) else {
                break;
            };
            // code 借用 self.data();read_entry 同样借用,故先拷成 owned 避免借用冲突。
            let code = self.read_string(idx.code_off, idx.code_len).to_string();
            for j in 0..idx.entry_len {
                if let Some(rec) = self.read_entry(idx.entry_off, j) {
                    f(&code, &rec.text, rec.weight);
                }
            }
        }
    }
}

/// 二进制词典写入器（用于将 rime dict.yaml 转换为 .wdb）
pub struct DictWriter {
    keys: Vec<(String, Vec<(String, i32)>)>, // (code, [(text, weight)])
}

impl Default for DictWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl DictWriter {
    pub fn new() -> Self {
        Self { keys: Vec::new() }
    }

    /// 添加一个键及其条目
    pub fn add(&mut self, code: String, entries: Vec<(String, i32)>) {
        if !entries.is_empty() {
            self.keys.push((code, entries));
        }
    }

    /// 从键数估算文件大小
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// 写入 wdb 文件（原子：先写 `<path>.tmp.<seq>` 再 rename 覆盖，避免读到半文件）。
    pub fn write(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        use std::io::Write;
        let path = path.as_ref();

        // 按 code 排序
        let mut sorted_keys = self.keys.clone();
        sorted_keys.sort_by(|a, b| a.0.cmp(&b.0));

        // 构建字符串池
        let mut string_pool = Vec::new();
        let mut string_offsets: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        let get_string_offset = |pool: &mut Vec<u8>,
                                 offsets: &mut std::collections::HashMap<String, u32>,
                                 s: &str|
         -> u32 {
            if let Some(&off) = offsets.get(s) {
                return off;
            }
            let off = pool.len() as u32;
            pool.extend_from_slice(s.as_bytes());
            offsets.insert(s.to_string(), off);
            off
        };

        // 预计算所有字符串偏移
        for (code, entries) in &sorted_keys {
            get_string_offset(&mut string_pool, &mut string_offsets, code);
            for (text, _) in entries {
                get_string_offset(&mut string_pool, &mut string_offsets, text);
            }
        }

        // 计算各段偏移
        let header_size = DictFileHeader::SIZE;
        let index_size = sorted_keys.len() * DictKeyIndex::SIZE;

        let mut total_entries = 0usize;
        for (_, entries) in &sorted_keys {
            total_entries += entries.len();
        }
        let entry_size = 14usize; // V3
        let data_size = total_entries * entry_size;

        let index_off = header_size as u32;
        let data_off = (header_size + index_size) as u32;
        let str_off = (header_size + index_size + data_size) as u32;

        // 原子写：写入临时文件，成功后 rename 覆盖目标。
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // tmp 名含 pid + 进程内递增序号：既防同进程并发写同目标，也防多进程同时重建同一
        // 缓存时 `.tmp` 撞名（撞名会让一方截断另一方正在写的 tmp）。仅防「读到半文件」，
        // 不追求崩溃安全——缓存可重建：崩溃后内容指纹校验失败会自动重建。
        let seq = ATOMIC_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut tmp_os = path.as_os_str().to_os_string();
        tmp_os.push(format!(".tmp.{}.{seq}", std::process::id()));
        let tmp = std::path::PathBuf::from(tmp_os);
        let mut file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);

        // Header
        let header = DictFileHeader {
            magic: MAGIC,
            version: 3,
            key_count: sorted_keys.len() as u32,
            index_off,
            data_off,
            str_off,
            abbrev_off: 0,
            meta_off: 0,
        };
        file.write_all(&header.magic)?;
        file.write_all(&header.version.to_le_bytes())?;
        file.write_all(&header.key_count.to_le_bytes())?;
        file.write_all(&header.index_off.to_le_bytes())?;
        file.write_all(&header.data_off.to_le_bytes())?;
        file.write_all(&header.str_off.to_le_bytes())?;
        file.write_all(&header.abbrev_off.to_le_bytes())?;
        file.write_all(&header.meta_off.to_le_bytes())?;

        // KeyIndex + EntryRecords（交错写入）
        // entry_off 是 EntryRecords 区内的【字节偏移】（= 累计条目数 × entry_size），
        // 与 Go binformat writer.go (`off := len(entryRecords) * DictEntryRecordSize`)
        // 及本文件 read_entry 的 `data_off + entry_off + idx*entry_size` 读取逻辑对齐。
        let mut entry_offset = 0u32;
        for (code, entries) in &sorted_keys {
            let code_off = string_offsets[code];
            let code_len = code.len() as u16;

            // KeyIndex
            file.write_all(&code_off.to_le_bytes())?;
            file.write_all(&code_len.to_le_bytes())?;
            file.write_all(&entry_offset.to_le_bytes())?;
            file.write_all(&(entries.len() as u16).to_le_bytes())?;

            entry_offset += entries.len() as u32 * entry_size as u32;
        }

        // EntryRecords
        let mut order = 0i32;
        for (_, entries) in &sorted_keys {
            for (text, weight) in entries {
                let text_off = string_offsets[text];
                let text_len = text.len() as u16;
                file.write_all(&text_off.to_le_bytes())?;
                file.write_all(&text_len.to_le_bytes())?;
                file.write_all(&weight.to_le_bytes())?;
                file.write_all(&order.to_le_bytes())?;
                order += 1;
            }
        }

        // StringPool
        file.write_all(&string_pool)?;

        // 落盘后原子替换：flush → 关闭 → rename。rename 失败则清理临时文件。
        file.flush()?;
        drop(file);
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }

        info!(
            "Wrote wdb: {} keys, {} entries, {} bytes string pool",
            sorted_keys.len(),
            total_entries,
            string_pool.len()
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 往返测试：写入多 key、每个 key 多条目，确保【非首 key】也能读对。
    /// 这是 entry_off 字节偏移语义的回归保护——历史 bug 是写入器把 entry_off
    /// 写成了累计条目数（count）而非字节偏移（count × entry_size），导致除首
    /// key 外的所有词条读到错误位置（text 为空/乱码），五笔与拼音候选全废。
    #[test]
    fn test_writer_reader_roundtrip_multi_key() {
        let tmp = std::env::temp_dir().join("wind_dict_roundtrip_test.wdb");

        let mut writer = DictWriter::new();
        // 故意让第一个 key 有 2 条，使后续 key 的 entry_off > 0（暴露 off-by-size bug）
        writer.add(
            "a".to_string(),
            vec![("工".to_string(), 9999), ("戈".to_string(), 100)],
        );
        writer.add(
            "ni".to_string(),
            vec![("你".to_string(), 800), ("尼".to_string(), 50)],
        );
        writer.add("nihao".to_string(), vec![("你好".to_string(), 1200)]);
        writer.add("zhongguo".to_string(), vec![("中国".to_string(), 2000)]);
        writer.write(&tmp).expect("write wdb");

        let reader = DictReader::open(&tmp).expect("open wdb");
        assert_eq!(reader.key_count(), 4);

        // 首 key（entry_off=0）
        let a = reader.search("a");
        assert_eq!(a.len(), 2);
        assert!(a.iter().any(|e| e.text == "工" && e.weight == 9999));

        // 非首 key（entry_off > 0）——历史 bug 在此读到乱码
        let nihao = reader.search("nihao");
        assert_eq!(nihao.len(), 1, "nihao 应有 1 条候选");
        assert_eq!(nihao[0].text, "你好");
        assert_eq!(nihao[0].weight, 1200);

        let ni = reader.search("ni");
        assert_eq!(ni.len(), 2);
        assert!(ni.iter().any(|e| e.text == "你" && e.weight == 800));
        assert!(ni.iter().any(|e| e.text == "尼"));

        let zg = reader.search("zhongguo");
        assert_eq!(zg.len(), 1);
        assert_eq!(zg[0].text, "中国");

        // 前缀查找：ni 前缀应命中 ni / nihao
        let prefix = reader.search_prefix("ni", 10);
        assert!(prefix.iter().any(|e| e.text == "你好"));
        assert!(prefix.iter().any(|e| e.text == "你"));

        let _ = std::fs::remove_file(&tmp);
    }

    /// **词条文本的尾随空白必须原样穿过缓存**。
    ///
    /// 解析层已改为保留末列 text 的尾随空格（蒙古文以空格作词间分隔，toli 词库整库如此，
    /// 见 `codetable::trim_line_end`）；但源文件解析对了、缓存这一段再吃掉，用户看到的
    /// 依然是丢空格——而且只在第二次启动（命中缓存）时复现，最难排查。
    ///
    /// 本格式用长度前缀存串（非 NUL 结尾、非按空白切分），故理应无损；此处把它钉死。
    #[test]
    fn test_roundtrip_preserves_trailing_whitespace_in_text() {
        let tmp = std::env::temp_dir().join("wind_dict_trailing_ws.wdb");

        let mut writer = DictWriter::new();
        // 蒙古文形态：PUA 码位 + 内部空格（词间分隔）+ 尾随空格
        writer.add(
            "aabn".to_string(),
            vec![("\u{e226}\u{e2f4} \u{e341}\u{e2ca} ".to_string(), 0)],
        );
        // 同一 code 下「有尾随空格」与「无尾随空格」是两条不同词条，不得被并成一条
        writer.add(
            "ka".to_string(),
            vec![("甲 ".to_string(), 10), ("甲".to_string(), 5)],
        );
        writer.write(&tmp).expect("write wdb");

        let reader = DictReader::open(&tmp).expect("open wdb");
        let m = reader.search("aabn");
        assert_eq!(m.len(), 1);
        assert_eq!(
            m[0].text, "\u{e226}\u{e2f4} \u{e341}\u{e2ca} ",
            "内部空格与尾随空格都须原样穿过缓存"
        );

        let ka = reader.search("ka");
        assert_eq!(ka.len(), 2, "差一个尾随空格的两条词条须各自独立");
        assert!(ka.iter().any(|e| e.text == "甲 " && e.weight == 10));
        assert!(ka.iter().any(|e| e.text == "甲" && e.weight == 5));

        let _ = std::fs::remove_file(&tmp);
    }

    /// for_each_entry 应枚举全部 (code,text,weight),供反查索引构建(mmap 路径)。
    #[test]
    fn test_for_each_entry_enumerates_all() {
        let tmp = std::env::temp_dir().join("wind_dict_foreach_test.wdb");
        let mut writer = DictWriter::new();
        writer.add(
            "a".to_string(),
            vec![("工".to_string(), 9999), ("戈".to_string(), 100)],
        );
        writer.add("aaaa".to_string(), vec![("工".to_string(), 50)]);
        writer.write(&tmp).expect("write wdb");

        let reader = DictReader::open(&tmp).expect("open wdb");
        let mut got: Vec<(String, String, i32)> = Vec::new();
        reader.for_each_entry(&mut |c, t, w| got.push((c.to_string(), t.to_string(), w)));
        assert_eq!(got.len(), 3, "应枚举 3 条");
        assert!(got.contains(&("a".to_string(), "工".to_string(), 9999)));
        assert!(got.contains(&("a".to_string(), "戈".to_string(), 100)));
        assert!(got.contains(&("aaaa".to_string(), "工".to_string(), 50)));

        let _ = std::fs::remove_file(&tmp);
    }
}
