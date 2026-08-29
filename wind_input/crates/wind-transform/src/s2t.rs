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
    /// 分三段读，而不是「整文件 `fs::read` 进内存再 `parse`」——后者会让整份文件与从中
    /// 切出来的字符串池（`parse` 里那次 `to_vec`）同时存活，峰值达常驻的两倍：
    /// STPhrases 实测常驻 1.42 MB（entries 0.56 + strings 0.86），而整文件另有 1.43 MB，
    /// 峰值 2.85 MB。
    ///
    /// 这里 entries 区读进临时缓冲、解析完即释放，字符串区直接读进最终的 `Vec`，
    /// 全程没有任何一份数据被同时持有两次，**峰值等于常驻**。
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
    /// 缺失时 `variants_of` 恒返回空——展开能力静默降级，不影响转换。
    variants: Option<Dict>,
}

impl Converter {
    /// 按变体从**单个** opencc 目录加载转换链。无可用词典返回 None。
    ///
    /// 多层（`data` / `data_custom` / 用户目录）场景用
    /// [`Self::load_variant_resolved`]——链里的每本词典各自逐层解析，见那里的说明。
    pub fn load_variant(opencc_dir: &Path, variant: &str) -> Option<Converter> {
        Self::load_variant_resolved(variant, |file| Some(opencc_dir.join(file)))
    }

    /// 按变体加载转换链，链里**每本词典的路径由 `resolve` 逐个给出**（入参是文件名，
    /// 如 `STPhrases.octrie`；返回 `None` 表示哪一层都没有）。
    ///
    /// # 为什么是逐文件解析，而不是「先选中一个目录再整份加载」
    ///
    /// 每个 `.octrie` 由 `gen_opencc` **各自独立生成**，`Converter` 的组合方式（组内取
    /// 最长匹配、组间串行）就是 OpenCC 自己的组合模型——跨来源按名取文件正是它设计上
    /// 支持的事。而「整份胜出」在本函数的加载语义下是**危险**的：下面那句
    /// `if !group.is_empty()` 只要求组内**至少一本**加载成功，于是定制者只放了一本
    /// `STPhrases.octrie`（只想改几个词组的繁体写法）时，「整份胜出」会拿一条只有词组表、
    /// 没有 `STCharacters` 的**残链**当胜利：输入「简体字转换测试」一个字都不转，日志上
    /// 只有一行「命中了定制层」，看不出链是残的。`STVariants.octrie`（以词定字的繁体
    /// 变体候选）同样会随之整份消失。
    ///
    /// 逐文件解析下，定制层放半套是**正常工作**的：缺的那本自动落回下一层。
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
            let variants = load("STVariants");
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

    /// 查询简体 `key` 的**全部**繁体变体（1对多，如「出」→ `["出", "齣"]`）。
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
        // s2t 标准
        _ => vec![vec!["STPhrases", "STCharacters"]],
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
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 真实 opencc 数据目录（`build_dev/data/opencc`）。
    ///
    /// ⚠️ 这里原本只写**两级** `../../build_dev/...`，解析到 `wind_input/build_dev/data/opencc`
    /// ——那个目录不存在，于是本文件所有依赖真实数据的用例长期静默走「跳过」分支、
    /// 计数照常绿，判据只有耗时。仓库根才是 `build_dev` 的位置（三级：
    /// crates/wind-transform → crates → wind_input → 仓库根）。同款坑见
    /// `wind-engine/tests/engine_manager.rs` 的同名函数，那边早已修正。
    /// 两处都试，取真的有数据的那一个。
    fn opencc_dir() -> PathBuf {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = base.join("../../../build_dev/data/opencc");
        [root.clone(), base.join("../../build_dev/data/opencc")]
            .into_iter()
            .find(|d| d.join("STCharacters.octrie").is_file())
            .unwrap_or(root)
    }

    #[test]
    fn test_s2t_standard_conversion() {
        let dir = opencc_dir();
        if !dir.join("STCharacters.octrie").exists() {
            eprintln!("跳过：缺少 opencc 数据");
            return;
        }
        let conv = Converter::load_variant(&dir, "s2t").expect("应加载 s2t 链");
        // 简体 → 繁体（字级）
        assert_eq!(conv.convert("汉字"), "漢字");
        assert_eq!(conv.convert("简体转换"), "簡體轉換");
        // 词级最长匹配（软件 → 軟件，标准 s2t 不转台湾习惯词）
        let r = conv.convert("计算机");
        assert!(r.chars().count() == 3, "长度应保持，实际: {}", r);
    }

    /// `load` 改成分段读之后，必须与「整文件读入再 parse」逐字段等价。
    /// 这是本次改动唯一可能出错的地方：偏移算错会让整张表静默错位。
    #[test]
    fn load_is_equivalent_to_read_then_parse() {
        let p = opencc_dir().join("STPhrases.octrie");
        if !p.exists() {
            eprintln!("跳过：缺少 opencc 数据");
            return;
        }
        let via_load = Dict::load(&p).expect("load 应成功");
        let via_parse = Dict::parse(&std::fs::read(&p).unwrap()).expect("parse 应成功");

        assert_eq!(via_load.max_key_len, via_parse.max_key_len);
        assert_eq!(
            via_load.strings, via_parse.strings,
            "字符串池必须逐字节一致"
        );
        assert_eq!(via_load.entries.len(), via_parse.entries.len());
        for (i, (a, b)) in via_load
            .entries
            .iter()
            .zip(via_parse.entries.iter())
            .enumerate()
        {
            assert_eq!(
                (a.key_off, a.key_len, a.val_off, a.val_len),
                (b.key_off, b.key_len, b.val_off, b.val_len),
                "第 {i} 条 entry 不一致"
            );
        }
    }

    /// 损坏的 count 不得据此分配巨量缓冲（分段读要先自己校验，不再有 parse 的
    /// `entries_end > data.len()` 兜底）。
    #[test]
    fn load_rejects_corrupt_count() {
        let dir = std::env::temp_dir().join(format!("wind-s2t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.octrie");

        let mut d = Vec::new();
        d.extend_from_slice(MAGIC);
        d.extend_from_slice(&1u32.to_le_bytes()); // version
        d.extend_from_slice(&u32::MAX.to_le_bytes()); // count：远超文件实际长度
        d.extend_from_slice(&4u16.to_le_bytes()); // max_key
        d.extend_from_slice(&[0u8; 2]); // 补齐 HEADER_SIZE
        assert_eq!(d.len(), HEADER_SIZE);
        std::fs::write(&p, &d).unwrap();

        assert!(
            Dict::load(&p).is_none(),
            "count 越界时应直接拒绝，而不是尝试分配 48GB"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 1对多变体表：多值字返回全部变体（定义序，首个=默认转换结果），单值字返回空。
    #[test]
    fn variants_of_returns_all_traditional_forms() {
        let dir = opencc_dir();
        if !dir.join("STVariants.octrie").exists() {
            eprintln!("跳过：缺少 STVariants 数据");
            return;
        }
        let conv = Converter::load_variant(&dir, "s2t").expect("应加载 s2t 链");
        // 「出」：默认不转（首值=自身），变体含「齣」。
        let v = conv.variants_of("出");
        assert_eq!(v, vec!["出", "齣"], "出 的变体应为 [出, 齣]");
        // 「发」：默认转「發」，变体另含「髮」。
        let v = conv.variants_of("发");
        assert!(v.contains(&"發".to_string()) && v.contains(&"髮".to_string()));
        assert_eq!(v[0], conv.convert("发"), "首个变体应与默认转换一致");
        // 单值字不在变体表：返回空（展开层据此跳过）。
        assert!(conv.variants_of("汉").is_empty());
        assert!(conv.variants_of("x").is_empty());
    }

    /// 造一份最小 .octrie（格式见文件头）：entries 按 key 字节序排，offset 相对字符串池起点。
    fn build_octrie(pairs: &[(&str, &str)]) -> Vec<u8> {
        let mut rows: Vec<(&str, &str)> = pairs.to_vec();
        rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut strings: Vec<u8> = Vec::new();
        let mut entries: Vec<u8> = Vec::new();
        let mut max_key = 0usize;
        for (k, v) in &rows {
            let ko = strings.len() as u32;
            strings.extend_from_slice(k.as_bytes());
            let vo = strings.len() as u32;
            strings.extend_from_slice(v.as_bytes());
            entries.extend_from_slice(&ko.to_le_bytes());
            entries.extend_from_slice(&(k.len() as u16).to_le_bytes());
            entries.extend_from_slice(&vo.to_le_bytes());
            entries.extend_from_slice(&(v.len() as u16).to_le_bytes());
            max_key = max_key.max(k.len());
        }
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        out.extend_from_slice(&(max_key as u16).to_le_bytes());
        out.extend_from_slice(&[0u8; 2]);
        out.extend_from_slice(&entries);
        out.extend_from_slice(&strings);
        out
    }

    /// ★ 定制层只放一本 `STPhrases.octrie` 时，链的其余部分必须落回出厂那一层。
    ///
    /// 这是实测出来的静默失效：`load_variant` 的组内判据是「至少一本加载成功」
    /// (`if !group.is_empty()`)，故「先选中一个目录再整份加载」会拿一条只有词组表的
    /// **残链**当胜利——输入「简体字转换测试」一个字都不转，而日志上只有一行「命中定制层」。
    /// 定制者只想改几个词组的繁体写法，放一本 STPhrases 是最自然的做法。
    ///
    /// 夹具全部自造（不依赖 build_dev 的真实 opencc 数据），故本用例永远真的在跑。
    #[test]
    fn resolved_chain_falls_back_per_file_not_per_directory() {
        let base = std::env::temp_dir().join(format!("wind-s2t-layers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let data = base.join("data/opencc");
        let custom = base.join("custom/opencc");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&custom).unwrap();

        // data 层：字表 + 变体表（出厂自带的全套）
        std::fs::write(
            data.join("STCharacters.octrie"),
            build_octrie(&[
                ("简", "簡"),
                ("体", "體"),
                ("转", "轉"),
                ("换", "換"),
                ("测", "測"),
                ("试", "試"),
            ]),
        )
        .unwrap();
        std::fs::write(
            data.join("STVariants.octrie"),
            build_octrie(&[("发", "發 髮")]),
        )
        .unwrap();
        // data 层也有词组表，定制层要能盖住它（证明「命中靠前层」这一半也成立）
        std::fs::write(
            data.join("STPhrases.octrie"),
            build_octrie(&[("转换测试", "轉換測試")]),
        )
        .unwrap();
        // custom 层：**只放一本**词组表
        std::fs::write(
            custom.join("STPhrases.octrie"),
            build_octrie(&[("转换测试", "轉換測試〔定制〕")]),
        )
        .unwrap();

        let layers = [custom.clone(), data.clone()];
        let resolve = |file: &str| -> Option<PathBuf> {
            layers
                .iter()
                .map(|d| d.join(file))
                .find(|p| p.is_file())
                .or(None)
        };
        let conv = Converter::load_variant_resolved("s2t", resolve).expect("应能建出链");

        assert_eq!(
            conv.convert("简体字转换测试"),
            "簡體字轉換測試〔定制〕",
            "定制层的词组表要生效，而字表必须仍从 data 层取——整份胜出时这里会原样不转"
        );
        assert_eq!(
            conv.convert("简体"),
            "簡體",
            "定制层没有字表，不得因此丢掉出厂字表"
        );
        assert_eq!(
            conv.variants_of("发"),
            vec!["發", "髮"],
            "STVariants 只在 data 层，同样不得被定制层的存在挤掉"
        );

        // 反面对照：只看定制层（= 旧的「整份胜出」在这台机器上的实际效果），
        // 链只剩词组表，字表整个消失。
        let only_custom = Converter::load_variant(&custom, "s2t").expect("残链也能建出来");
        assert_eq!(
            only_custom.convert("简体"),
            "简体",
            "对照：只有 STPhrases 的残链一个字都不转——这正是本用例要挡住的现象"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn test_s2t_preserves_non_chinese() {
        let dir = opencc_dir();
        if !dir.join("STCharacters.octrie").exists() {
            return;
        }
        let conv = Converter::load_variant(&dir, "s2t").unwrap();
        assert_eq!(conv.convert("abc123"), "abc123");
        assert_eq!(conv.convert("hello 世界"), "hello 世界");
    }
}
