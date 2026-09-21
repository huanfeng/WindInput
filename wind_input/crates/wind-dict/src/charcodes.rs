//! `.wscc`：单字全码表（单字 → 该字的全码）的序列化格式。
//!
//! # 为什么它需要落盘
//!
//! 这张表与反查索引同源——同一批词库、同一次 `for_each_entry` 全表扫描
//! （[`crate::cached::build_single_char_full_codes_from`]），代价也在同一量级。
//! 反查索引早已落成 `.wridx`，它却一直是纯内存产物：每次启动都要重新
//! `load_dicts_individually` 加载整组词库再全量扫一遍，结果只活在进程内存里的一份缓存中。
//!
//! 后果不在内存而在**时间**：自动造词的就绪闸是「反查索引 ∧ 单字全码表」两者的与
//! （`EngineManager::single_char_codes_ready`），于是 `.wridx` 命中缓存省下的那几秒，
//! 被这张没有缓存的表原样吃了回去——开机后的头几次造词照样整次跳过。
//!
//! # 为什么这里没有 mmap（与 `.wridx` 的分工）
//!
//! `.wridx` 走 mmap 是因为它在大词库上是 95 MB 量级的长尾灾难。这张表不是：它只收
//! **单字**条目，条目数是词库里的汉字数（万级，不随词条数增长），每条又只有一个
//! 短编码。feihuzj2 那种 251 万词的方案，反查索引 95.4 MB，这张表仍是 MB 以内。
//!
//! 为 MB 级数据付 mmap 的代价（缺页、文件句柄、只能顺序走的布局）是净亏，
//! 所以本格式**只求解析快**：顺序读一遍直接灌进 `HashMap`，读端形态与从词库现建时
//! 完全一致，消费侧一行都不用改。
//!
//! # 文件布局
//!
//! - Header (16 B)：magic `WSCC` + version u32 + entry_count u32 + reserved u32
//! - Entry[entry_count]：char u32（Unicode 标量值）+ code_len u8 + code 字节
//!
//! 条目**不排序**：读端是 `HashMap`，顺序没有语义，排序只会给写端平添一次 n log n。
//! 这与 `.wridx` 刻意按 text 字节序排恰好相反——那边的前缀顺扫依赖有序，这边不依赖。
//!
//! 编码长度用 1 字节：它受方案的 `max_code_length` 约束，实际是个位数，
//! 而 u8 的 255 上限远在任何码表方案的量程之外。

use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 4] = b"WSCC";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 16;

/// 单条编码的字节上限（`code_len` 是 u8）。超限的条目在序列化时**整条丢弃**。
///
/// 丢弃而非截断：截断出来的是一个能被查到、却打不出那个字的错码，
/// 与 `build_single_char_full_codes_from` 「码源与词库同源」的前提直接冲突。
const MAX_CODE_LEN: usize = u8::MAX as usize;

/// 序列化成 `.wscc` 镜像。
pub fn serialize(map: &HashMap<char, String>) -> Vec<u8> {
    let mut entries: Vec<u8> = Vec::with_capacity(map.len() * 8);
    let mut count: u32 = 0;
    for (ch, code) in map {
        let bytes = code.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_CODE_LEN {
            continue;
        }
        entries.extend_from_slice(&(*ch as u32).to_le_bytes());
        entries.push(bytes.len() as u8);
        entries.extend_from_slice(bytes);
        count += 1;
    }
    let mut out = Vec::with_capacity(HEADER_LEN + entries.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&entries);
    out
}

/// 解析 `.wscc` 镜像。格式不符、截断、或含非法标量值/非 UTF-8 编码时返回 `Err`。
///
/// **宁可整份拒绝，不做部分恢复**：这张表的每一条都是「这个字怎么打出来」，
/// 少一条就是那个字所在的词整词取码失败（`encode_word` 的 `MissingCode`）。
/// 半份表会把「缓存坏了」变成「某些词莫名其妙造不出来」，那是最难查的一类故障。
/// 调用方拿到 `Err` 直接重建即可，代价只是这一次启动慢一点。
pub fn parse(bytes: &[u8]) -> anyhow::Result<HashMap<char, String>> {
    if bytes.len() < HEADER_LEN {
        anyhow::bail!("wscc: 文件短于文件头（{} B）", bytes.len());
    }
    if &bytes[0..4] != MAGIC {
        anyhow::bail!("wscc: magic 不符");
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into()?);
    if version != VERSION {
        anyhow::bail!("wscc: 版本 {version} 不是 {VERSION}");
    }
    let count = u32::from_le_bytes(bytes[8..12].try_into()?) as usize;
    let mut map = HashMap::with_capacity(count);
    let mut p = HEADER_LEN;
    for _ in 0..count {
        if p + 5 > bytes.len() {
            anyhow::bail!("wscc: 条目在第 {} 条处截断", map.len());
        }
        let cp = u32::from_le_bytes(bytes[p..p + 4].try_into()?);
        let Some(ch) = char::from_u32(cp) else {
            anyhow::bail!("wscc: 非法 Unicode 标量值 {cp:#x}");
        };
        let len = bytes[p + 4] as usize;
        p += 5;
        if p + len > bytes.len() {
            anyhow::bail!("wscc: 编码在第 {} 条处截断", map.len());
        }
        let code = std::str::from_utf8(&bytes[p..p + len])?.to_string();
        p += len;
        map.insert(ch, code);
    }
    Ok(map)
}

/// 读并解析 `path`。
pub fn read(path: &Path) -> anyhow::Result<HashMap<char, String>> {
    parse(&std::fs::read(path)?)
}

/// 把镜像写到 `path`（tmp + rename 原子替换）。
///
/// 失败不是正确性问题——调用方直接用内存里那份即可，只是下次启动还要再建一遍。
/// 同 [`crate::reverseidx::write_wridx`]。
pub fn write(path: &Path, image: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, image)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HashMap<char, String> {
        [('工', "aaaa"), ('王', "ggg"), ('㐀', "xyzw"), ('𠀀', "q")]
            .into_iter()
            .map(|(c, s)| (c, s.to_string()))
            .collect()
    }

    /// 基本盘：非 BMP 字符（`𠀀` U+20000）必须原样回来。
    ///
    /// 用 `char as u32` 存标量值而不是 UTF-8 字节，正是为了让扩展区汉字与 BMP 同价——
    /// 码表里的生僻字大量落在 Ext-B 以上，它们恰恰是最需要靠这张表取码的那批。
    #[test]
    fn roundtrip_preserves_every_entry_including_astral_chars() {
        let m = sample();
        let back = parse(&serialize(&m)).unwrap();
        assert_eq!(back, m);
        assert!(back.contains_key(&'𠀀'), "非 BMP 字符不能丢");
    }

    #[test]
    fn empty_table_roundtrips() {
        let m = HashMap::new();
        assert_eq!(parse(&serialize(&m)).unwrap(), m);
    }

    /// 截断的文件必须整份拒绝，不能交出半张表。
    #[test]
    fn truncated_file_is_rejected_whole() {
        let img = serialize(&sample());
        for cut in [HEADER_LEN - 1, HEADER_LEN + 3, img.len() - 1] {
            assert!(
                parse(&img[..cut]).is_err(),
                "截到 {cut} B 仍被接受 —— 半份表会变成「某些字莫名取不到码」"
            );
        }
    }

    #[test]
    fn foreign_or_future_files_are_rejected() {
        let mut img = serialize(&sample());
        let good = img.clone();
        img[0] = b'X';
        assert!(parse(&img).is_err(), "magic 不符必须拒绝");
        let mut img = good;
        img[4] = 0xFF;
        assert!(parse(&img).is_err(), "未来版本必须拒绝而不是照旧解析");
    }

    /// 空编码与超 255 字节的编码在写端就被丢掉，不写进文件。
    #[test]
    fn unrepresentable_codes_are_dropped_at_write_time() {
        let m: HashMap<char, String> = [
            ('空', String::new()),
            ('长', "a".repeat(MAX_CODE_LEN + 1)),
            ('好', "vb".to_string()),
        ]
        .into_iter()
        .collect();
        let back = parse(&serialize(&m)).unwrap();
        assert_eq!(back.len(), 1, "只有合法的那条该留下");
        assert_eq!(back.get(&'好').map(String::as_str), Some("vb"));
    }

    #[test]
    fn write_then_read_from_disk() {
        let dir = std::env::temp_dir().join("wind_wscc_rt");
        let p = dir.join("t.wscc");
        let _ = std::fs::remove_file(&p);
        write(&p, &serialize(&sample())).unwrap();
        assert_eq!(read(&p).unwrap(), sample());
        let _ = std::fs::remove_file(&p);
    }
}
