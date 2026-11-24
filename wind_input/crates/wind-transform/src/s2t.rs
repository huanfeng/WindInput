//! 简繁转换 (S2T)
//!
//! 与 Go 版本 `wind_input/internal/transform/s2t/` 对齐。
//! 读取 OpenCC `.octrie` 二进制词典，按转换链做最长前缀匹配替换。
//!
//! .octrie 格式：Header(16B: Magic "WIOC", Version u32, Count u32, MaxKeyB u16, Reserved u16)
//! + Entries(Count×12B: KeyOff u32, KeyLen u16, ValOff u32, ValLen u16，按 key 升序)
//! + StringTable(UTF-8 字节池)。

use std::path::Path;

const MAGIC: &[u8; 4] = b"WIOC";
const HEADER_SIZE: usize = 16;
const ENTRY_SIZE: usize = 12;

struct Entry {
    key_off: u32,
    key_len: u16,
    val_off: u32,
    val_len: u16,
}

/// 单个 OpenCC 词典：紧凑字节池 + 有序 entry 数组，支持二分查找与最长前缀匹配。
pub struct Dict {
    entries: Vec<Entry>,
    strings: Vec<u8>,
    max_key_len: usize,
}

impl Dict {
    /// 从字节切片解析 .octrie。
    pub fn parse(data: &[u8]) -> Option<Dict> {
        if data.len() < HEADER_SIZE || &data[0..4] != MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(data[4..8].try_into().ok()?);
        if version != 1 {
            return None;
        }
        let count = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;
        let max_key = u16::from_le_bytes(data[12..14].try_into().ok()?) as usize;

        let entries_end = HEADER_SIZE + count * ENTRY_SIZE;
        if entries_end > data.len() {
            return None;
        }
        let mut entries = Vec::with_capacity(count);
        let mut off = HEADER_SIZE;
        for _ in 0..count {
            entries.push(Entry {
                key_off: u32::from_le_bytes(data[off..off + 4].try_into().ok()?),
                key_len: u16::from_le_bytes(data[off + 4..off + 6].try_into().ok()?),
                val_off: u32::from_le_bytes(data[off + 6..off + 10].try_into().ok()?),
                val_len: u16::from_le_bytes(data[off + 10..off + 12].try_into().ok()?),
            });
            off += ENTRY_SIZE;
        }
        let strings = data[entries_end..].to_vec();
        Some(Dict {
            entries,
            strings,
            max_key_len: max_key,
        })
    }

    /// 从文件加载。
    ///
    /// 分三段读，而不是整文件读进内存再 `parse`：后者让整份文件与切出的字符串池同时存活，
    /// 峰值翻倍。这里 entries 区解析完即释放，字符串区直接读进最终的 `Vec`，峰值等于常驻。
    pub fn load(path: &Path) -> Option<Dict> {
        use std::io::Read;

        let mut f = std::fs::File::open(path).ok()?;
        let total = f.metadata().ok()?.len() as usize;

        let mut header = [0u8; HEADER_SIZE];
        f.read_exact(&mut header).ok()?;
        if &header[0..4] != MAGIC {
            return None;
        }
        if u32::from_le_bytes(header[4..8].try_into().ok()?) != 1 {
            return None;
        }
        let count = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
        let max_key = u16::from_le_bytes(header[12..14].try_into().ok()?) as usize;

        // count 来自文件，可能损坏：先防乘法溢出，再校验不越界，避免据此分配巨量缓冲。
        let entries_len = count.checked_mul(ENTRY_SIZE)?;
        let entries_end = HEADER_SIZE.checked_add(entries_len)?;
        if entries_end > total {
            return None;
        }

        // 条目区：临时缓冲解析完立刻释放，不与字符串池并存。
        let mut buf = vec![0u8; entries_len];
        f.read_exact(&mut buf).ok()?;
        let mut entries = Vec::with_capacity(count);
        for chunk in buf.as_chunks::<ENTRY_SIZE>().0 {
            entries.push(Entry {
                key_off: u32::from_le_bytes(chunk[0..4].try_into().ok()?),
                key_len: u16::from_le_bytes(chunk[4..6].try_into().ok()?),
                val_off: u32::from_le_bytes(chunk[6..10].try_into().ok()?),
                val_len: u16::from_le_bytes(chunk[10..12].try_into().ok()?),
            });
        }
        drop(buf);

        // 余下全是字符串池，直接读进最终 Vec（预分配，省掉 read_to_end 的逐次扩容）。
        let mut strings = Vec::with_capacity(total - entries_end);
        f.read_to_end(&mut strings).ok()?;

        Some(Dict {
            entries,
            strings,
            max_key_len: max_key,
        })
    }

    /// 用本表单独做一遍最长前缀替换，不走 [`Converter`] 的转换链。给不属于任何转换链、
    /// 也不该受 s2t variant 配置影响的场合用（如 emoji 扩展表的繁→简键归一，见
    /// `docs/design/emoji-suggestion.md` §3.1）。语义与链中的一步相同（同一个 `apply_step`）。
    pub fn convert_once(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        let out = apply_step(std::slice::from_ref(self), s.as_bytes());
        String::from_utf8(out).unwrap_or_else(|_| s.to_string())
    }

    fn val_of(&self, i: usize) -> &[u8] {
        let e = &self.entries[i];
        &self.strings[e.val_off as usize..e.val_off as usize + e.val_len as usize]
    }

    fn lookup(&self, key: &[u8]) -> Option<&[u8]> {
        match self.entries.binary_search_by(|e| {
            self.strings[e.key_off as usize..e.key_off as usize + e.key_len as usize].cmp(key)
        }) {
            Ok(i) => Some(self.val_of(i)),
            Err(_) => None,
        }
    }

    /// 在 input 起点找最长 key 命中，返回 (匹配字节数, value)。
    fn longest_prefix(&self, input: &[u8]) -> Option<(usize, &[u8])> {
        if input.is_empty() || self.max_key_len == 0 {
            return None;
        }
        let max_l = self.max_key_len.min(input.len());
        for l in (1..=max_l).rev() {
            if let Some(val) = self.lookup(&input[..l]) {
                return Some((l, val));
            }
        }
        None
    }
}

/// 转换器：串行多步，每步一组词典（OpenCC group 语义：组内取最长匹配）。
pub struct Converter {
    steps: Vec<Vec<Dict>>,
    /// ST 基础组（链中第一组）之后的步骤在 `steps` 中的起始索引。变体展开出的字
    /// 已处于 ST 组的输出域，只需再过后续地区变体步（TWVariants/HKVariants）。
    post_st_start: usize,
    /// 1对多变体表（STVariants.octrie：简体字 → 空格分隔的全部繁体变体）。
    /// 缺失时 `variants_of` 恒返回空：展开能力静默降级，不影响转换。
    variants: Option<Dict>,
}

impl Converter {
    /// 按变体从单个 opencc 目录加载转换链。无可用词典返回 None。
    ///
    /// 多层（`data` / `data_custom` / 用户目录）场景用
    /// [`Self::load_variant_resolved`]——链里的每本词典各自逐层解析，见那里的说明。
    pub fn load_variant(opencc_dir: &Path, variant: &str) -> Option<Converter> {
        Self::load_variant_resolved(variant, |file| Some(opencc_dir.join(file)))
    }

    /// 按变体加载转换链，链里每本词典的路径由 `resolve` 逐个给出（入参是文件名，
    /// 如 `STPhrases.octrie`；`None` 表示哪一层都没有）。
    ///
    /// 逐文件解析而不是「选中一个目录整份加载」：各 `.octrie` 独立生成，跨来源按名取文件符合
    /// OpenCC 的组合模型。而下面的 `if !group.is_empty()` 只要求组内至少一本成功，整份胜出时
    /// 定制层只放一本 `STPhrases.octrie` 就会得到缺 `STCharacters` 的残链（单字全不转）。
    /// 逐文件下定制层放半套也正常工作，缺的自动落回下一层。
    pub fn load_variant_resolved(
        variant: &str,
        resolve: impl Fn(&str) -> Option<std::path::PathBuf>,
    ) -> Option<Converter> {
        let load = |name: &str| -> Option<Dict> {
            let p = resolve(&format!("{name}.octrie"))?;
            Dict::load(&p)
        };
        let chain = chain_for(variant);
        let mut steps = Vec::new();
        let mut post_st_start = 0;
        for (gi, group_names) in chain.into_iter().enumerate() {
            let mut group = Vec::new();
            for name in group_names {
                if let Some(d) = load(name) {
                    group.push(d);
                }
            }
            if !group.is_empty() {
                steps.push(group);
            }
            // 链首组（ST 基础组）处理完后，无论其是否成功加载，后续组都从当前长度起。
            if gi == 0 {
                post_st_start = steps.len();
            }
        }
        if steps.is_empty() {
            None
        } else {
            // 变体表是方向性资产，只在简→繁方向有意义。t2s 下按名 load 也会成功，但对繁体输入
            // 恒不命中；静默恒空比明确的 None 更难查，故由链的方向决定加不加载。
            let variants = variants_table_for(variant).and_then(load);
            Some(Converter {
                steps,
                post_st_start,
                variants,
            })
        }
    }

    /// 执行一次完整链路转换。
    pub fn convert(&self, s: &str) -> String {
        if s.is_empty() || self.steps.is_empty() {
            return s.to_string();
        }
        let mut cur = s.as_bytes().to_vec();
        for group in &self.steps {
            cur = apply_step(group, &cur);
        }
        String::from_utf8(cur).unwrap_or_else(|_| s.to_string())
    }

    /// 查询简体 `key` 的全部繁体变体（1对多，如「出」→ `["出", "齣"]`）。
    ///
    /// 返回值已过完链中 ST 组之后的地区变体步（s2tw/s2hk 下变体字继续按台/港习惯归一），
    /// 顺序保持源词典定义序（首个即 OpenCC 默认转换结果）。表缺失或未命中返回空。
    /// 调用方自行过滤与默认转换结果重复的项。
    pub fn variants_of(&self, key: &str) -> Vec<String> {
        let Some(dict) = &self.variants else {
            return Vec::new();
        };
        let Some(val) = dict.lookup(key.as_bytes()) else {
            return Vec::new();
        };
        let Ok(joined) = std::str::from_utf8(val) else {
            return Vec::new();
        };
        joined
            .split(' ')
            .filter(|v| !v.is_empty())
            .map(|v| {
                let mut cur = v.as_bytes().to_vec();
                for group in &self.steps[self.post_st_start..] {
                    cur = apply_step(group, &cur);
                }
                String::from_utf8(cur).unwrap_or_else(|_| v.to_string())
            })
            .collect()
    }
}

/// 变体 → 转换链（词典名分组）。
fn chain_for(variant: &str) -> Vec<Vec<&'static str>> {
    match variant.to_lowercase().as_str() {
        "s2tw" | "tw" | "taiwan" => {
            vec![vec!["STPhrases", "STCharacters"], vec!["TWVariants"]]
        }
        "s2twp" | "twp" => vec![
            vec!["STPhrases", "STCharacters"],
            vec!["TWPhrases"],
            vec!["TWVariants"],
        ],
        "s2hk" | "hk" | "hongkong" => {
            vec![vec!["STPhrases", "STCharacters"], vec!["HKVariants"]]
        }
        // 繁 → 简（「繁入繁出」的反向：词库/内部候选是繁体，出口转简体）。
        // 链形与 s2t 标准镜像对称，词典由 OpenCC 官方同名文件提供。
        // 刻意只有标准一档：tw2s/hk2s 要自造 OpenCC 官方没有的反转表，繁入简出又是小众场景。
        "t2s" => vec![vec!["TSPhrases", "TSCharacters"]],
        // s2t 标准
        _ => vec![vec!["STPhrases", "STCharacters"]],
    }
}

/// 该转换链配套的 1对多变体表文件名（无则 `None`）。
///
/// 见 [`Converter::load_variant_resolved`] 里的说明：这张表只在简→繁方向存在。
fn variants_table_for(variant: &str) -> Option<&'static str> {
    match variant.to_lowercase().as_str() {
        "t2s" => None,
        _ => Some("STVariants"),
    }
}

/// 用一组词典做最长前缀匹配替换扫描。
fn apply_step(group: &[Dict], input: &[u8]) -> Vec<u8> {
    if group.is_empty() || input.is_empty() {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len() + 8);
    let mut i = 0;
    while i < input.len() {
        if let Some((n, val)) = group_longest_prefix(group, &input[i..]) {
            out.extend_from_slice(val);
            i += n;
        } else {
            let step = utf8_step(input[i]);
            let end = (i + step).min(input.len());
            out.extend_from_slice(&input[i..end]);
            i = end;
        }
    }
    out
}

/// 组内各词典取最长匹配，跨成员选最长。
fn group_longest_prefix<'a>(group: &'a [Dict], input: &[u8]) -> Option<(usize, &'a [u8])> {
    let mut best: Option<(usize, &'a [u8])> = None;
    for d in group {
        if let Some((n, val)) = d.longest_prefix(input)
            && best.is_none_or(|(bl, _)| n > bl)
        {
            best = Some((n, val));
        }
    }
    best
}

fn utf8_step(b: u8) -> usize {
    // < 0x80 是 ASCII；0x80..0xC0 是本不该出现在此的续字节，也按 1 跳过以免死循环。
    if b < 0xC0 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
#[path = "s2t_tests.rs"]
mod tests;
