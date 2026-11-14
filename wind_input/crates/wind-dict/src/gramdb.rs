//! librime-octagram `.gram` 语法模型读取（darts-clone double-array trie）。
//!
//! 格式依据与实测结论全部记在 `docs/design/language-model-integration.md` §2.2，
//! 其中 §2.2.3a 是本文件位域运算的出处。**若本文件与文档不符，以实测为准并更新文档。**
//!
//! 只读、零解析：`mmap` 之后直接把文件尾部当成 double-array 镜像用，
//! 与 librime `GramDb::Load` 的 `trie_->set_array(array, size)` 等价。
//!
//! ## 为什么直接读 `.gram` 而不转成自有格式
//!
//! 查询只用到 `traverse` + `commonPrefixSearch` 两个只读操作，加起来不到 100 行；
//! 而转格式要**遍历整棵 trie 提取全部键值对**，比查询复杂得多。直接读还顺带保住了
//! 「换个数据文件即可切换模型」——rime 生态的 `.gram` 都能用。

use crate::mapping::Mapped;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::path::Path;

/// 值的定点缩放，对齐 `gram_db.cc` 的 `kValueScale`。
/// 存进 trie 的是 `int(ln(频次) * 10000)`（实测见设计文档 §2.2.3b）。
pub const VALUE_SCALE: f64 = 10_000.0;

/// 一次 `commonPrefixSearch` 最多取几个匹配，对齐 `GramDb::kMaxResults`。
pub const MAX_RESULTS: usize = 8;

/// 单次编码最多容纳几个 Unicode 字符，对齐 `gram_encoding.h::kMaxEncodedUnicode`。
pub const MAX_ENCODED_UNICODE: usize = 8;

/// 编码缓冲区字节数：每字符最多 4 字节（变长分支的最坏情况）。
const ENCODE_BUF_LEN: usize = MAX_ENCODED_UNICODE * 4;

/// `grammar::Metadata` 的大小：`format[32]` + `db_checksum` + `double_array_size`
/// + `OffsetPtr`（各 4 字节）。
const METADATA_SIZE: usize = 44;
/// `OffsetPtr` 字段自身的偏移；它存的是**相对自己地址**的位移。
const OFFSET_PTR_AT: usize = 40;

const FORMAT_PREFIX: &str = "Rime::Grammar/";

/// 定长编码缓冲：避免在解码内循环里为每次 query 分配 `Vec`。
pub struct EncodeBuf {
    buf: [u8; ENCODE_BUF_LEN],
    len: usize,
}

impl Default for EncodeBuf {
    fn default() -> Self {
        Self {
            buf: [0; ENCODE_BUF_LEN],
            len: 0,
        }
    }
}

impl EncodeBuf {
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    fn push(&mut self, b: u8) {
        if self.len < ENCODE_BUF_LEN {
            self.buf[self.len] = b;
            self.len += 1;
        }
    }
}

/// 复刻 `gram_encoding.cc::encode`：把最多 `max_chars` 个字符编码进 `out`。
///
/// CJK 主区（U+4000..U+A000）被压成 **2 字节**，这是 octagram 缩小 trie 键长的手段。
/// **单向映射**（`u == 0` 与 `(u & 0xFF) == 0` 都走转义），不要拿它做往返转换。
///
/// 返回实际编码了几个字符。
pub fn encode_chars<I: Iterator<Item = char>>(
    chars: I,
    max_chars: usize,
    out: &mut EncodeBuf,
) -> usize {
    out.len = 0;
    let mut n = 0;
    for ch in chars {
        if n >= max_chars || n >= MAX_ENCODED_UNICODE {
            break;
        }
        let u = ch as u32;
        if u < 0x80 {
            out.push(if u == 0 { 0xE0 } else { u as u8 });
        } else if (0x4000..0xA000).contains(&u) {
            if (u & 0xFF) == 0 {
                out.push(0xE1);
                out.push(((u >> 8) + 0x40) as u8);
            } else {
                out.push(((u >> 8) + 0x40) as u8);
                out.push((u & 0xFF) as u8);
            }
        } else {
            let mut bits = 32i32;
            let mut v = u;
            while bits > 0 && (v & 0xFE00_0000) == 0 {
                bits -= 7;
                v <<= 7;
            }
            let mut n_bytes = (bits + 6) / 7;
            out.push(0xE0 | n_bytes as u8);
            while n_bytes > 0 {
                n_bytes -= 1;
                out.push((((v >> 25) & 0x7F) | 0x80) as u8);
                v <<= 7;
            }
        }
        n += 1;
    }
    n
}

/// 沿编码串前进一个字符，返回其字节长度。对齐 `gram_encoding.cc::advance`。
pub fn encoded_char_len(b: u8) -> usize {
    if b & 0x80 == 0 {
        1
    } else if b & 0xF0 == 0xE0 {
        (b & 0x0F) as usize + 1
    } else {
        2
    }
}

/// 只读打开的 `.gram`。
pub struct GramDb {
    mmap: Mapped,
    da_offset: usize,
    n_units: usize,
}

// --- darts-clone 位域（见设计文档 §2.2.3a）---
#[inline]
fn has_leaf(u: u32) -> bool {
    (u >> 8) & 1 == 1
}
#[inline]
fn value_of(u: u32) -> i32 {
    (u & 0x7FFF_FFFF) as i32
}
#[inline]
fn offset_of(u: u32) -> u32 {
    (u >> 10) << ((u & 0x200) >> 6)
}
#[inline]
fn label_of(u: u32) -> u32 {
    u & 0x8000_00FF
}

impl GramDb {
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("打开语法模型失败: {}", path.display()))?;
        let mmap = crate::mapping::map_file(&file)
            .with_context(|| format!("mmap 语法模型失败: {}", path.display()))?;
        if mmap.len() < METADATA_SIZE {
            bail!("语法模型过小({} 字节)，不含完整 metadata", mmap.len());
        }

        let fmt_end = mmap[..32].iter().position(|&b| b == 0).unwrap_or(32);
        let format = std::str::from_utf8(&mmap[..fmt_end]).unwrap_or("");
        if !format.starts_with(FORMAT_PREFIX) {
            bail!("不是 rime 语法模型(format={format:?})");
        }

        let da_size = u32::from_le_bytes(mmap[36..40].try_into().expect("4 字节")) as usize;
        let ptr_off = i32::from_le_bytes(
            mmap[OFFSET_PTR_AT..OFFSET_PTR_AT + 4]
                .try_into()
                .expect("4 字节"),
        );
        let da_offset = (OFFSET_PTR_AT as i64 + ptr_off as i64) as usize;
        if da_offset >= mmap.len() {
            bail!("double-array 偏移越界: {da_offset} >= {}", mmap.len());
        }
        let avail = mmap.len() - da_offset;

        // ★ 这个等式本身就是格式判据：darts-clone 的 unit 是 **4 字节**
        // （不是原版 Darts 的 8 字节 `{int base; uint check;}`）。不成立说明
        // 文件损坏、或换了我们没读过的实现。
        if da_size.saturating_mul(4) != avail {
            bail!(
                "unit 大小校验失败: da_size*4={} 但可用 {avail} 字节",
                da_size * 4
            );
        }

        Ok(Self {
            mmap,
            da_offset,
            n_units: da_size,
        })
    }

    pub fn unit_count(&self) -> usize {
        self.n_units
    }

    #[inline]
    fn unit(&self, i: usize) -> u32 {
        let o = self.da_offset + i * 4;
        u32::from_le_bytes([
            self.mmap[o],
            self.mmap[o + 1],
            self.mmap[o + 2],
            self.mmap[o + 3],
        ])
    }

    /// 从 `node_pos` 沿 `key` 走，返回到达的节点；失配返回 `None`。
    pub fn traverse(&self, key: &[u8], node_pos: usize) -> Option<usize> {
        if node_pos >= self.n_units {
            return None;
        }
        let mut id = node_pos as u32;
        let mut u = self.unit(id as usize);
        for &b in key {
            id ^= offset_of(u) ^ b as u32;
            if id as usize >= self.n_units {
                return None;
            }
            u = self.unit(id as usize);
            if label_of(u) != b as u32 {
                return None;
            }
        }
        Some(id as usize)
    }

    /// 沿 `key` 走，收集途中**每个成词节点**的值。
    ///
    /// 注意这与「子树 top-K」是两回事：本函数只走一条路径，
    /// 「你好」查出的是「你」和「你好」，而不是「你好/你们/你的…」。
    ///
    /// 返回写入 `out` 的个数，元素为 `(定点值, 已匹配的字节数)`。
    pub fn common_prefix_search(
        &self,
        key: &[u8],
        node_pos: usize,
        out: &mut [(i32, usize); MAX_RESULTS],
    ) -> usize {
        if node_pos >= self.n_units {
            return 0;
        }
        let mut id = node_pos as u32;
        let mut u = self.unit(id as usize);
        let mut n = 0;
        for (i, &b) in key.iter().enumerate() {
            id ^= offset_of(u) ^ b as u32;
            if id as usize >= self.n_units {
                return n;
            }
            u = self.unit(id as usize);
            if label_of(u) != b as u32 {
                return n;
            }
            if has_leaf(u) {
                let leaf = (id ^ offset_of(u)) as usize;
                if leaf < self.n_units && n < MAX_RESULTS {
                    out[n] = (value_of(self.unit(leaf)), i + 1);
                    n += 1;
                }
            }
        }
        n
    }

    /// 编码串中前 `byte_len` 个字节对应几个字符。对齐 `grammar::unicode_length`。
    pub fn encoded_unicode_len(encoded: &[u8], byte_len: usize) -> usize {
        let mut i = 0;
        let mut n = 0;
        while i < byte_len && i < encoded.len() {
            i += encoded_char_len(encoded[i]);
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编码是最容易写错的一环（位运算 + 单向转义），用已知取值锁死。
    /// 期望值来自对 `gram_encoding.cc` 的逐行复算，并已在真实 `.gram` 上
    /// 用「查得到高频搭配」交叉验证过（设计文档 §2.2.3 记了 8/10 命中）。
    #[test]
    fn encode_cjk_main_block_to_two_bytes() {
        let mut buf = EncodeBuf::default();

        // 的 U+7684 落在 [0x4000,0xA000)：(0x76 + 0x40, 0x84)
        assert_eq!(encode_chars("的".chars(), 8, &mut buf), 1);
        assert_eq!(buf.as_slice(), &[0xB6, 0x84]);

        // 时 U+65F6 → (0x65 + 0x40, 0xF6)
        assert_eq!(encode_chars("时".chars(), 8, &mut buf), 1);
        assert_eq!(buf.as_slice(), &[0xA5, 0xF6]);

        // 多字符拼接：候 U+5019 → (0x90, 0x19)
        assert_eq!(encode_chars("时候".chars(), 8, &mut buf), 2);
        assert_eq!(buf.as_slice(), &[0xA5, 0xF6, 0x90, 0x19]);
    }

    /// ASCII 原样单字节——octagram 用 `$` 作句末标记，靠的就是这条。
    #[test]
    fn encode_ascii_stays_one_byte() {
        let mut buf = EncodeBuf::default();
        assert_eq!(encode_chars("$".chars(), 8, &mut buf), 1);
        assert_eq!(buf.as_slice(), b"$");
    }

    /// `max_chars` 与 `MAX_ENCODED_UNICODE` 双重截断，缓冲区不会溢出。
    #[test]
    fn encode_respects_char_limit() {
        let mut buf = EncodeBuf::default();
        assert_eq!(encode_chars("时候时候".chars(), 2, &mut buf), 2);
        assert_eq!(buf.as_slice(), &[0xA5, 0xF6, 0x90, 0x19]);

        let long: String = "字".repeat(50);
        let n = encode_chars(long.chars(), 999, &mut buf);
        assert_eq!(n, MAX_ENCODED_UNICODE, "不得超过 kMaxEncodedUnicode");
        assert!(buf.as_slice().len() <= ENCODE_BUF_LEN);
    }

    /// 编码串的「按字符前进」要与编码规则互逆（CJK 2 字节 / ASCII 1 字节）。
    #[test]
    fn encoded_unicode_len_counts_chars_not_bytes() {
        let mut buf = EncodeBuf::default();
        encode_chars("时候".chars(), 8, &mut buf);
        let s = buf.as_slice();
        assert_eq!(s.len(), 4);
        assert_eq!(GramDb::encoded_unicode_len(s, 4), 2);
        assert_eq!(GramDb::encoded_unicode_len(s, 2), 1);
    }
}
