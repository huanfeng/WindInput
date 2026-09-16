//! 双拼转换器与布局
//!
//! 布局以 TOML 三表分区声明（data/schemas/shuangpin/<id>.toml），与 Go
//! `wind_input/internal/engine/pinyin/shuangpin/` 对齐，但方案数据外置不硬编码。

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// 双拼布局：键位 → 声母/韵母/零声母映射。
#[derive(Debug, Clone)]
pub struct Layout {
    pub id: String,
    pub name: String,
    initials: HashMap<u8, String>,
    finals: HashMap<u8, Vec<String>>,
    zero_initials: HashMap<u8, Vec<String>>,
    /// 显式零声母键对映射（如 `ue → "e"`），优先于 zero_initials 三层查找。
    zero_pairs: HashMap<[u8; 2], String>,
}

#[derive(Deserialize)]
struct RawLayout {
    meta: RawMeta,
    #[serde(default)]
    initials: HashMap<String, String>,
    #[serde(default)]
    finals: HashMap<String, Vec<String>>,
    #[serde(default)]
    zero_initials: HashMap<String, Vec<String>>,
    #[serde(default)]
    zero_pairs: HashMap<String, String>,
}

#[derive(Deserialize)]
struct RawMeta {
    id: String,
    name: String,
}

/// 单字节键转换（布局键均为单 ASCII 字符）。
fn key_byte(s: &str) -> anyhow::Result<u8> {
    let b = s.as_bytes();
    if b.len() != 1 {
        anyhow::bail!("布局键必须为单字符: {:?}", s);
    }
    Ok(b[0])
}

impl Layout {
    pub fn from_toml(path: &Path) -> anyhow::Result<Layout> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取双拼布局 {} 失败: {}", path.display(), e))?;
        Self::from_toml_str(&text)
    }

    /// 解析双拼布局 TOML 文本。命名与 [`from_toml`](Self::from_toml) 对称（一个吃路径、
    /// 一个吃文本），不叫 `from_str`——那个名字属于 `FromStr` trait，而本函数的错误类型是
    /// `anyhow::Error`，实现 trait 反而会把调用点的错误语境挤掉。
    pub fn from_toml_str(toml_text: &str) -> anyhow::Result<Layout> {
        let raw: RawLayout = toml::from_str(toml_text)?;
        let mut initials: HashMap<u8, String> = HashMap::new();
        // 声母自映射补全：26 字母默认映射自身
        for c in b'a'..=b'z' {
            initials.insert(c, (c as char).to_string());
        }
        // 显式声母覆盖
        for (k, v) in raw.initials {
            initials.insert(key_byte(&k)?, v);
        }
        let mut finals = HashMap::new();
        for (k, v) in raw.finals {
            finals.insert(key_byte(&k)?, v);
        }
        let mut zero_initials = HashMap::new();
        for (k, v) in raw.zero_initials {
            zero_initials.insert(key_byte(&k)?, v);
        }
        let mut zero_pairs = HashMap::new();
        for (k, v) in raw.zero_pairs {
            let kb = k.as_bytes();
            if kb.len() != 2 {
                anyhow::bail!("zero_pairs 键必须为两字符: {:?}", k);
            }
            zero_pairs.insert([kb[0], kb[1]], v);
        }
        if finals.is_empty() {
            anyhow::bail!("双拼布局 {} 缺少 [finals]", raw.meta.id);
        }
        Ok(Layout {
            id: raw.meta.id,
            name: raw.meta.name,
            initials,
            finals,
            zero_initials,
            zero_pairs,
        })
    }

    pub fn initial_of(&self, key: u8) -> Option<&str> {
        self.initials.get(&key).map(|s| s.as_str())
    }
    pub fn finals_of(&self, key: u8) -> &[String] {
        self.finals.get(&key).map(|v| v.as_slice()).unwrap_or(&[])
    }
    pub fn zero_of(&self, key: u8) -> &[String] {
        self.zero_initials
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
    pub fn is_final_key(&self, key: u8) -> bool {
        self.finals.contains_key(&key)
    }
    pub fn has_zero_pairs(&self) -> bool {
        !self.zero_pairs.is_empty()
    }
    pub fn zero_pair(&self, key1: u8, key2: u8) -> Option<&str> {
        self.zero_pairs.get(&[key1, key2]).map(|s| s.as_str())
    }

    /// 返回所有韵母键的集合（供 EngineManager 缓存，对齐 Go IsShuangpinFinalKey）。
    pub fn final_key_set(&self) -> std::collections::HashSet<u8> {
        self.finals.keys().copied().collect()
    }

    /// 本布局的码元字符集：`a-z` + 布局用到的**非字母**键（微软/搜狗/紫光的 `;` = ing）。
    /// 全是字母（小鹤/自然码/abc/首道）时返回 `None` —— 与内置默认 `a-z` 等价，
    /// 让协调器走「默认集」快捷路径，零回归。
    ///
    /// ★ **首码集按「这个键出现在第几码」分**，不是「是不是字母」：
    /// 声母 / 零声母引导键（`initials`、`zero_initials`、`zero_pairs` 的**首**字节）是第一码
    /// → 进首码集；韵母键（`finals`、`zero_pairs` 的**次**字节）只能作第二码 → 不进。
    /// 这正是 `;` 既能打 `ying` 又不夺走空缓冲下 `;` 的快捷输入引导键的原因
    /// （见 `docs/design/codetable-input-chars.md`「首码集是仲裁者」）。
    ///
    /// 现有 7 个内置布局的第一码全是字母，故首码集实际恒为 `a-z`；按语义写而非硬编码，
    /// 是为了让「哪天有布局把符号配成声母键」时不必再改这里。
    pub fn code_char_set(&self) -> Option<wind_config::CodeCharSet> {
        // 第一码键：声母 + 零声母引导（zero_pairs 的首字节）。
        let leading_extra: Vec<u8> = self
            .initials
            .keys()
            .chain(self.zero_initials.keys())
            .copied()
            .chain(self.zero_pairs.keys().map(|p| p[0]))
            .filter(|k| !k.is_ascii_alphabetic())
            .collect();
        // 全集键：第一码 + 韵母（zero_pairs 的次字节同为第二码）。
        let all_extra: Vec<u8> = self
            .finals
            .keys()
            .copied()
            .chain(self.zero_pairs.keys().map(|p| p[1]))
            .filter(|k| !k.is_ascii_alphabetic())
            .chain(leading_extra.iter().copied())
            .collect();
        if all_extra.is_empty() {
            return None;
        }
        Some(wind_config::CodeCharSet::new(
            &charset_spec(&all_extra),
            &charset_spec(&leading_extra),
            &format!("双拼布局 {}", self.id),
        ))
    }
}

/// 把一组非字母键拼成 `CodeCharSet` 规格串（`a-z` + 这些字面字符）。
///
/// ⚠️ **`-` 必须排在末位**：规格串里 `-` 只在首/末位才是字面，夹在中间会被当范围符
/// （`"a-z-;"` → 解析 `z-;` → 端点逆序 → 整串解析失败 → 静默回落 `a-z`，
/// 于是布局里的符号键一个都进不了缓冲，而错误只在日志里）。
/// 非 ASCII / 不可打印键在此就地丢弃：它们过不了 `CodeCharSet::parse`，
/// 留着会让**整串**失败，把同布局里合法的符号键一起拖下水。
fn charset_spec(extra: &[u8]) -> String {
    let mut keys: Vec<u8> = extra
        .iter()
        .copied()
        .filter(|&k| k.is_ascii_graphic())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    // `-` 置尾（见上）。
    keys.sort_by_key(|&k| k == b'-');
    let mut spec = String::from("a-z");
    spec.extend(keys.into_iter().map(|k| k as char));
    spec
}

// ============================================================================
// ShuangpinConverter：双拼键序列 → 全拼（三层转换 + 模糊对偶声母兜底 + 位置映射）
//
// 忠实移植 Go `wind_input/internal/engine/pinyin/shuangpin/converter.go`：
//   - convert_pair  ↔ Go convertPair（零声母 a/b/c 三路径 → 常规声母+韵母 → 单键重复）
//   - convert       ↔ Go Convert（含奇数尾键 partial、无匹配键对原样回写）
//   - 模糊兜底       ↔ Go fuzzyInitialPartners（z/zh、c/ch、s/sh 三对，对偶变体并入候选末尾）
//   - map_consumed_length ↔ Go (*ConvertResult).MapConsumedLength（音节边界优先 + PositionMap 兜底）
//   - normalize_pinyin / extract_final / matches_final ↔ Go 同名函数
// 合法音节判定复用 SyllableTrie（与 Go validPinyinSyllables 对齐）。
// ============================================================================

use crate::pinyin::syllable::SyllableTrie;

/// 一个转换后的音节。
///
/// 原为本模块私有的 `ConvertedSyllable`（字段名 `sp_*` = shuangpin），现已提升为通用的
/// [`SylSpan`]——raw↔flat 的往返需求遍布全拼、分隔符、简拼各条路径，不是双拼专属。
/// 见 `super::interp` 的模块文档。
pub use super::interp::SylSpan;

/// 手动音节分隔符。全拼与双拼共用同一个字符：协调器把用户配的那个键（`'` 或反引号，
/// 见 `manual_separator_key_of`）统一翻译成它再送进引擎，引擎侧只认这一个形态。
///
/// 它同时也是 `preedit_display` 里**自动**分段用的字符——两者刻意同形，用户看到的
/// `n'hc` 里那一撇既是他按的、也正好是系统会加的位置，视觉上不分叉。
const SEPARATOR: char = '\'';
/// [`SEPARATOR`] 的字节形态。双拼键序列按字节扫描（全为 ASCII），比较用这个。
const SEPARATOR_B: u8 = b'\'';

/// 双拼→全拼转换结果（对齐 Go ConvertResult）。
#[derive(Debug, Clone, Default)]
pub struct SpConvertResult {
    /// 转换后的完整全拼字符串（如 "nihao"）。
    /// 与 Go FullPinyin 一致：包含无匹配键对的原样回写，以及尾部 partial 声母前缀。
    pub full_pinyin: String,
    /// 已完成（或原样回写）的音节列表。
    pub syllables: Vec<SylSpan>,
    /// 未配对的最后一个键解析出的声母（无则 None）。
    pub partial_initial: Option<String>,
    /// 未配对的原始按键（无则 None）。
    pub partial_key: Option<u8>,
    /// 是否有未完成的输入。
    pub has_partial: bool,
    /// 全拼每字节 → 双拼原始字节偏移的映射（供 MapConsumedLength 回算）。
    pub position_map: Vec<usize>,
    /// 预编辑区显示文本（全拼 + `'` 分隔符），如 "ni'hao"。
    pub preedit_display: String,
}

impl SpConvertResult {
    /// 全拼字符串（与 Go FullPinyin 一致：含原样回写与 partial 前缀）。
    pub fn full_pinyin(&self) -> String {
        self.full_pinyin.clone()
    }

    /// 将全拼 ConsumedLength 回映射为双拼 ConsumedLength（对齐 Go MapConsumedLength）。
    /// `fp_consumed`：全拼引擎报告的已消耗字节数；返回双拼原始输入中对应的字节数。
    pub fn map_consumed_length(&self, fp_consumed: usize) -> usize {
        if fp_consumed == 0 {
            return 0;
        }
        // 优先通过音节边界精确映射。
        let mut fp_end = 0;
        for s in &self.syllables {
            fp_end += s.pinyin.len();
            if fp_end >= fp_consumed {
                return s.raw_end;
            }
        }
        // Fallback：使用位置映射表（覆盖 partial、无效键对/简拼等场景）。
        let fp_consumed = fp_consumed.min(self.position_map.len());
        if fp_consumed > 0 {
            self.position_map[fp_consumed - 1] + 1
        } else {
            0
        }
    }
}

/// 双拼→全拼转换器（对齐 Go Converter）。
pub struct ShuangpinConverter {
    layout: Layout,
    trie: SyllableTrie,
    /// 声母模糊音开关：z↔zh / c↔ch / s↔sh。
    fuzzy_zh_z: bool,
    fuzzy_ch_c: bool,
    fuzzy_sh_s: bool,
}

impl ShuangpinConverter {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            trie: SyllableTrie::new(),
            fuzzy_zh_z: false,
            fuzzy_ch_c: false,
            fuzzy_sh_s: false,
        }
    }

    /// 配置声母模糊音开关（对齐 Go SetFuzzyInitials）。
    pub fn set_fuzzy(&mut self, zh_z: bool, ch_c: bool, sh_s: bool) {
        self.fuzzy_zh_z = zh_z;
        self.fuzzy_ch_c = ch_c;
        self.fuzzy_sh_s = sh_s;
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn is_final_key(&self, key: u8) -> bool {
        self.layout.is_final_key(key)
    }

    fn is_valid(&self, syl: &str) -> bool {
        self.trie.is_syllable(syl)
    }

    /// 返回 initial 在当前模糊开关下的对偶变体（不含自身），对齐 Go fuzzyInitialPartners。
    fn fuzzy_initial_partners(&self, initial: &str) -> Option<&'static str> {
        match initial {
            "z" if self.fuzzy_zh_z => Some("zh"),
            "zh" if self.fuzzy_zh_z => Some("z"),
            "c" if self.fuzzy_ch_c => Some("ch"),
            "ch" if self.fuzzy_ch_c => Some("c"),
            "s" if self.fuzzy_sh_s => Some("sh"),
            "sh" if self.fuzzy_sh_s => Some("s"),
            _ => None,
        }
    }

    /// 转换一对键为全拼音节候选列表（对齐 Go convertPair，逐分支同序）。
    fn convert_pair(&self, key1: u8, key2: u8) -> Vec<String> {
        self.convert_pair_inner(key1, key2, true)
    }

    /// [`Self::convert_pair`] 的本体，多一个「要不要并入模糊对偶声母」的开关。
    ///
    /// 参数化而不是让建表方自己抄一遍规则：模糊音只影响**一个分支的候选列表**
    /// （下方第 2 步），其余零声母三路径、`zero_pairs`、单韵母重复键全都与它无关。
    /// 反向表建表走 `use_fuzzy = false`，理由见 [`ShuangpinReverse`]。
    ///
    /// ★ 这是**预防**不是修复：当前唯一的生产建表方 `EngineManager::shuangpin_reverse`
    /// 现建一个从未调用 `set_fuzzy` 的转换器，所以两条路径眼下同值。参数留着是因为
    /// 「顺手复用引擎已有的 converter 来建表」是很现实的重构方向，那一天它就是错的了
    /// （守卫测试 `reverse_ignores_fuzzy_initials` 手动开模糊音，钉的正是那一天）。
    fn convert_pair_inner(&self, key1: u8, key2: u8, use_fuzzy: bool) -> Vec<String> {
        let mut results: Vec<String> = Vec::new();

        if self.layout.has_zero_pairs() {
            // 显式零声母键对映射（优先路径，与旧 zero_initials 互斥）。
            if let Some(syl) = self.layout.zero_pair(key1, key2)
                && self.is_valid(syl)
                && !results.iter().any(|r| r == syl)
            {
                results.push(syl.to_string());
            }
        } else {
            // 1. 零声母（旧路径：三子路径查找）
            let zero_syllables = self.layout.zero_of(key1);
            if !zero_syllables.is_empty() {
                // a) FinalMap 路径：仅接受同时在 zeroSyllables 中允许的合法音节。
                for f in self.layout.finals_of(key2) {
                    if !self.is_valid(f) {
                        continue;
                    }
                    if !zero_syllables.iter().any(|zs| zs == f) {
                        continue;
                    }
                    if !results.iter().any(|r| r == f) {
                        results.push(f.clone());
                    }
                }

                // b) 字面匹配：仅在 FinalMap 路径无命中时生效。
                if results.is_empty() {
                    let literal = format!("{}{}", key1 as char, key2 as char);
                    for syllable in zero_syllables {
                        if *syllable == literal && self.is_valid(syllable) {
                            results.push(syllable.clone());
                            break;
                        }
                    }
                }

                // c) matchesFinal 路径：兜底，处理方案特殊映射。
                for syllable in zero_syllables {
                    if results.iter().any(|r| r == syllable) {
                        continue;
                    }
                    if self.matches_final(syllable, key2) {
                        results.push(syllable.clone());
                    }
                }
            }
        }

        // 2. 常规声母+韵母（原始声母在前，模糊对偶声母在后）。
        if let Some(initial) = self.layout.initial_of(key1) {
            let mut initial_candidates: Vec<&str> = vec![initial];
            if use_fuzzy && let Some(alt) = self.fuzzy_initial_partners(initial) {
                initial_candidates.push(alt);
            }
            for init in initial_candidates {
                for f in self.layout.finals_of(key2) {
                    let syllable = normalize_pinyin(&format!("{}{}", init, f));
                    if self.is_valid(&syllable) && !results.contains(&syllable) {
                        results.push(syllable);
                    }
                }
            }
        }

        if !self.layout.has_zero_pairs() {
            // 3. 零声母特殊处理：单韵母重复键（aa→a, oo→o, ee→e）。
            //
            // ★ 只对**把该键配成零声母引导键、且允许该单韵母**的方案生效。这条规则属于
            //   「首字母引导」流派（小鹤/自然码）；「O 引导」流派（微软/搜狗/智能ABC）里
            //   `aa`/`ee` 不是零声母，`a` 只能打 `oa`。
            //   原先无条件放行，于是方案规则一半写在 TOML、一半写在引擎里：微软双拼即便把
            //   `[zero_initials]` 收敛成纯 `o` 引导，`aa`/`ee` 照样出「啊/额」，数据说了不算。
            if key1 == key2 {
                let single = (key1 as char).to_string();
                if self.is_valid(&single)
                    && self.layout.zero_of(key1).contains(&single)
                    && !results.contains(&single)
                {
                    results.push(single);
                }
            }
        }

        results
    }

    /// 检查一个完整音节是否匹配给定的韵母键（对齐 Go matchesFinal）。
    fn matches_final(&self, syllable: &str, final_key: u8) -> bool {
        let finals = self.layout.finals_of(final_key);
        if finals.is_empty() {
            return false;
        }
        let syllable_final = extract_final(syllable);
        finals.iter().any(|f| f == syllable_final)
    }

    /// 将双拼键序列转换为全拼（对齐 Go Convert）。
    /// `keys` 为小写字母序列（如小鹤方案下的 "nihc"），可含手动音节分隔符 `'`。
    ///
    /// ## 手动分隔符 `'` 是**配对的硬边界**
    ///
    /// 双拼每 2 键一音节，配对起点由「前面消耗了几个键」决定——一旦用户想让某个键单独
    /// 作简拼声母，其后**所有**键的配对都会错位：`nhc`（你 简 + 好 全）被读成
    /// `nh`→`nang` 加残码 `c`，全然不是用户敲的那两个字。这个歧义无法由打分挽回，
    /// 因为两种读法都是合法击键序列（见 `pinyin-mixed-abbrev.md` 记下的「双拼下 `xan`
    /// 是三声母还是 xa+残码，本身歧义，故意未处理」——本函数就是补上那个手段）。
    ///
    /// ⇒ `'` 处**重置配对起点**：`n'hc` → 段 `n`（落单，作声母）+ 段 `hc`（→`hao`），
    /// full 得 `nhao`，正是混合简拼（声母段 + 完整音节）认得的形状。
    ///
    /// ★ **段尾落单键与末尾落单键不是一回事**：后者是「还没打完」（`partial`，可能续打
    /// 下一键成对），前者是用户已用 `'` 明确宣告「它就到此为止」。故段尾落单键写进 full
    /// 却**不置 `has_partial`**——置了会让下游把已定案的简拼段当成待续输入。
    ///
    /// ★ 段尾落单键刻意**不进 `syllables`**：它不是音节。这样 `sp_boundary_mask` 会把它
    /// 当作「回写段」自动标出段起点（那里已有 `s.fp_start > cursor` 的空隙分支），
    /// 手动边界因此无需在 mask 里另开一路。
    ///
    /// `'` 自身不进 `full_pinyin`、不占 `position_map`；但 `raw_start`/`raw_end` 与
    /// `position_map` 的值取**含 `'` 的击键偏移**，故 `map_consumed_length` 回算出的
    /// 键数天然把分隔符算在内——用户确实按了那一下。
    pub fn convert(&self, keys: &str) -> SpConvertResult {
        let mut result = SpConvertResult::default();
        if keys.is_empty() {
            return result;
        }

        let input = keys.to_ascii_lowercase();
        let b = input.as_bytes();

        let mut full = String::new();
        let mut preedit = String::new();
        let mut fp_pos = 0usize;

        let mut i = 0usize;
        while i < b.len() {
            let key1 = b[i];

            // 分隔符自身不产出任何东西，只是让下一轮从这里重新起段。
            // 连续 `''` 与开头 `'` 都落在这里，各自空转一轮。
            if key1 == SEPARATOR_B {
                i += 1;
                continue;
            }

            // 段尾 = 输入到头，或下一个键是分隔符。
            let is_last_of_segment = i + 1 >= b.len() || b[i + 1] == SEPARATOR_B;
            if is_last_of_segment {
                // 落单键一律解析成声母写进 full；两种落单的区别只在要不要置 partial。
                let partial_str = match self.layout.initial_of(key1) {
                    Some(init) => init.to_string(),
                    None => (key1 as char).to_string(),
                };
                // 只有输入末尾那个才是「未配对、可能续打」。段尾的是用户已定案的简拼段。
                if i + 1 >= b.len() {
                    result.partial_initial = Some(partial_str.clone());
                    result.partial_key = Some(key1);
                    result.has_partial = true;
                }

                if !preedit.is_empty() {
                    preedit.push(SEPARATOR);
                }
                preedit.push_str(&partial_str);
                full.push_str(&partial_str);
                for _ in 0..partial_str.len() {
                    result.position_map.push(i);
                }
                fp_pos += partial_str.len();
                i += 1;
                continue;
            }

            let key2 = b[i + 1];
            let syllables = self.convert_pair(key1, key2);

            if !syllables.is_empty() {
                let best = syllables[0].clone();
                if !preedit.is_empty() {
                    preedit.push(SEPARATOR);
                }
                preedit.push_str(&best);
                full.push_str(&best);

                let best_len = best.len();
                result.syllables.push(SylSpan {
                    pinyin: best,
                    raw_start: i,
                    raw_end: i + 2,
                    fp_start: fp_pos,
                    fp_end: fp_pos + best_len,
                });

                // 位置映射：全拼前半字节映射回 key1（i），后半映射回 key2（i+1）。
                for j in 0..best_len {
                    if j < best_len / 2 {
                        result.position_map.push(i);
                    } else {
                        result.position_map.push(i + 1);
                    }
                }
                fp_pos += best_len;
            } else {
                // 无法匹配：两个键原样保留（简拼/无效键对）。
                let s = format!("{}{}", key1 as char, key2 as char);
                if !preedit.is_empty() {
                    preedit.push(SEPARATOR);
                }
                preedit.push_str(&s);
                full.push_str(&s);
                result.position_map.push(i);
                result.position_map.push(i + 1);
                fp_pos += 2;
            }

            i += 2;
        }

        result.full_pinyin = full;
        result.preedit_display = preedit;
        result
    }

    /// 建立音节 → 双拼键对的反向表（见 [`ShuangpinReverse`]）。
    ///
    /// 建表成本是 O(首码键 × 韵母键) 次 `convert_pair`（现有布局约 676 次，每次含若干
    /// trie 查询），**故必须缓存**，绝不在按键链路上现建。
    pub fn build_reverse(&self) -> ShuangpinReverse {
        // 首码键 = 声母键 ∪ 零声母引导键 ∪ `zero_pairs` 的首字节；
        // 次码键 = 韵母键 ∪ `zero_pairs` 的次字节。
        // 与 [`Layout::code_char_set`] 的「首码集按这个键出现在第几码分」是同一判据——
        // 那里回答「哪些键能起头」，这里回答「枚举时第一位该试哪些键」。
        let mut leading: Vec<u8> = self
            .layout
            .initials
            .keys()
            .chain(self.layout.zero_initials.keys())
            .copied()
            .chain(self.layout.zero_pairs.keys().map(|k| k[0]))
            .collect();
        leading.sort_unstable();
        leading.dedup();

        let mut trailing: Vec<u8> = self
            .layout
            .finals
            .keys()
            .copied()
            .chain(self.layout.zero_pairs.keys().map(|k| k[1]))
            .collect();
        trailing.sort_unstable();
        trailing.dedup();

        // ★ 排序后枚举，表才是**确定**的。直接遍历 HashMap 的话「哪个键对抢到某音节」
        // 会随哈希序漂移，而那恰是往返测试要钉住的东西——漂移后测试时绿时红，
        // 且真机上同一个字今天显示 `vh` 明天显示别的。
        let mut pairs: Vec<([u8; 2], Vec<String>)> =
            Vec::with_capacity(leading.len() * trailing.len());
        for &k1 in &leading {
            for &k2 in &trailing {
                // `use_fuzzy = false`：理由见 [`ShuangpinReverse`] 的「建表必须关掉模糊音」。
                let res = self.convert_pair_inner(k1, k2, false);
                if !res.is_empty() {
                    pairs.push(([k1, k2], res));
                }
            }
        }

        let mut map: HashMap<String, [u8; 2]> = HashMap::new();
        // 第一遍：只认 `results[0]`——敲这个键对**实际得到**的那个音节。
        for (keys, res) in &pairs {
            map.entry(res[0].clone()).or_insert(*keys);
        }
        // 第二遍：给「只能作次选出现」的音节兜底。合成一遍的话次选会抢在首选前面占位，
        // 反向表给出的键对敲下去得到的是另一个字。
        for (keys, res) in &pairs {
            for syl in res.iter().skip(1) {
                map.entry(syl.clone()).or_insert(*keys);
            }
        }

        ShuangpinReverse { map }
    }
}

/// 音节 → 双拼键对的反向表：把 [`ShuangpinConverter`] 的解码规则**反演**出来。
///
/// 回答的是「这个词在当前双拼方案下要敲哪些键」，供候选注释的 `${code_schema}` 使用。
/// 与 `${code_rev}`（拿候选文本去码表词库查反向索引）不是一回事：那边是**查表**，
/// 这边是**算**——双拼编码不存在于任何词库里，双拼只是「全拼词库 + 一张键盘布局」。
///
/// # 为什么是枚举反演而不是手写反向规则
///
/// 双拼的取码规则——零声母三路径、`zero_pairs` 显式键对、单韵母重复键 `aa→a`、
/// j/q/x/y 的 ü 归一化、以及「哪个流派允许 `aa` 当零声母」这类方案差异——全部集中在
/// [`ShuangpinConverter::convert_pair`] 一个函数里。手写一份反向规则等于把同一套规则
/// 誊抄第二遍，而 `convert_pair` 里单韵母重复键那段注释记的正是上一次「方案规则一半
/// 写在 TOML、一半写在引擎里」留下的伤：微软双拼把 `[zero_initials]` 收敛成纯 `o` 引导，
/// `aa`/`ee` 照样出「啊/额」，数据说了不算。
///
/// 枚举反演没有这个问题：规则只有一处，反向表是它的**产物**而不是副本。
/// 新增布局、改 TOML、改取码规则，反向表自动跟上。
///
/// # 建表必须关掉模糊音
///
/// 开着模糊音时 `convert_pair` 会把对偶声母（z↔zh / c↔ch / s↔sh）的结果并进候选末尾。
/// 小鹤下 `zh` 这个键对，`z`+`uang` 不是合法音节被 trie 拒掉，模糊对偶 `zh`+`uang`
/// = `zhuang` 却合法，于是 `zhuang` 成了 `zh` 的 `results[0]`；而枚举序 `z` 在 `v`
/// 之前，`zhuang → zh` 会抢在真值 `vh`（小鹤 `v` = `zh`）之前登记。
///
/// 后果不是「显示得不够好」而是**显示的编码是错的**：用户照着 `zh` 敲下去得到的是
/// 「臓」而不是「装」。模糊音是输入时的宽容，不是这个字的打法。
pub struct ShuangpinReverse {
    map: HashMap<String, [u8; 2]>,
}

impl ShuangpinReverse {
    /// 单个音节 → 两个击键；表里没有返回 `None`。
    pub fn encode(&self, syllable: &str) -> Option<[u8; 2]> {
        self.map.get(syllable).copied()
    }

    /// 整串音节 → 击键串（`["ni", "hao"]` → `"nihc"`）。
    ///
    /// **任一音节查不到即整体返回 `None`**，不做「查得到的拼上、查不到的跳过」：
    /// 半截击键串是**错**的答案而不是不完整的答案，用户照着敲会得到别的字。
    /// 注释是装饰，这一次不显示即可。
    pub fn encode_all(&self, syllables: &[&str]) -> Option<String> {
        if syllables.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(syllables.len() * 2);
        for syl in syllables {
            let [a, b] = self.encode(syl)?;
            out.push(a as char);
            out.push(b as char);
        }
        Some(out)
    }

    /// 表里收录的音节数。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 提取拼音音节的韵母部分（对齐 Go extractFinal）。
fn extract_final(syllable: &str) -> &str {
    const INITIALS: &[&str] = &[
        "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x",
        "r", "z", "c", "s", "y", "w",
    ];
    for initial in INITIALS {
        if syllable.starts_with(initial) && syllable.len() > initial.len() {
            return &syllable[initial.len()..];
        }
    }
    // 零声母：整个音节就是韵母。
    syllable
}

/// 标准化拼音（处理 ü 相关规则，对齐 Go normalizePinyin）。
/// j/q/x/y 后：ve→ue、vn→un、v→u；其余声母（如 n/l）的 v 保留为 ü 占位（nv/lv）。
fn normalize_pinyin(pinyin: &str) -> String {
    if pinyin.starts_with('j')
        || pinyin.starts_with('q')
        || pinyin.starts_with('x')
        || pinyin.starts_with('y')
    {
        let mut p = pinyin.replacen("ve", "ue", 1);
        p = p.replacen("vn", "un", 1);
        if p.contains('v') {
            p = p.replacen('v', "u", 1);
        }
        p
    } else {
        pinyin.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XIAOHE: &str = r#"
[meta]
id = "xiaohe"
name = "小鹤双拼"
[initials]
v = "zh"
i = "ch"
u = "sh"
[finals]
o = ["uo", "o"]
k = ["uai", "ing"]
v = ["ui", "v"]
[zero_initials]
a = ["a", "ai", "an", "ang", "ao"]
"#;

    #[test]
    fn layout_parse_and_self_map() {
        let lay = Layout::from_toml_str(XIAOHE).unwrap();
        assert_eq!(lay.id, "xiaohe");
        assert_eq!(lay.name, "小鹤双拼");
        // 显式声母
        assert_eq!(lay.initial_of(b'v'), Some("zh"));
        assert_eq!(lay.initial_of(b'i'), Some("ch"));
        // 自映射补全：未在 [initials] 列出的普通声母键映射自身
        assert_eq!(lay.initial_of(b'b'), Some("b"));
        assert_eq!(lay.initial_of(b'p'), Some("p"));
        // 韵母多值
        assert_eq!(lay.finals_of(b'o'), &["uo".to_string(), "o".to_string()]);
        assert!(lay.is_final_key(b'k'));
        assert!(!lay.is_final_key(b'q')); // q 不在 finals
        // 零声母
        assert_eq!(lay.zero_of(b'a').len(), 5);
    }

    #[test]
    fn layout_symbol_key_as_final() {
        let t = "[meta]\nid=\"x\"\nname=\"x\"\n[finals]\n\";\" = [\"ing\"]\n";
        let lay = Layout::from_toml_str(t).unwrap();
        assert!(lay.is_final_key(b';'));
        assert_eq!(lay.finals_of(b';'), &["ing".to_string()]);
    }

    // --- 边界测试 6.1-1：from_toml 不存在的路径 → Err，不 panic ---
    #[test]
    fn from_toml_nonexistent_path_returns_err() {
        let p = std::path::Path::new("/nonexistent/path/does_not_exist.toml");
        let result = Layout::from_toml(p);
        assert!(result.is_err(), "不存在的文件应返回 Err，而非 panic");
    }

    // --- 边界测试 6.1-2a：from_str 缺少 [meta] → Err ---
    #[test]
    fn from_str_missing_meta_returns_err() {
        let bad = r#"
[finals]
k = ["ao"]
"#;
        let result = Layout::from_toml_str(bad);
        assert!(result.is_err(), "缺 [meta] 的 toml 应返回 Err");
    }

    // --- 边界测试 6.1-2b：from_str meta 缺少 id 字段 → Err ---
    #[test]
    fn from_str_meta_missing_id_returns_err() {
        let bad = r#"
[meta]
name = "无 id 方案"
[finals]
k = ["ao"]
"#;
        let result = Layout::from_toml_str(bad);
        assert!(result.is_err(), "meta 缺 id 字段应返回 Err");
    }

    // --- 边界测试 6.1-3：from_str 多字符键名 → key_byte 校验返回 Err ---
    #[test]
    fn from_str_multichar_key_returns_err() {
        // initials 里使用 2 字符键名 "zh"（而非单字符），应被 key_byte 拒绝
        let bad = r#"
[meta]
id = "bad"
name = "bad"
[initials]
zh = "x"
[finals]
k = ["ao"]
"#;
        let result = Layout::from_toml_str(bad);
        assert!(result.is_err(), "多字符键名应被 key_byte 校验拒绝");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("单字符") || err_msg.contains("zh"),
            "错误信息应提及多字符键: {err_msg}"
        );
    }

    #[test]
    fn builtin_layouts_load() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        for id in [
            "xiaohe", "ziranma", "mspy", "sogou", "abc", "ziguang", "shoudao",
        ] {
            let p = dir.join(format!("{id}.toml"));
            let lay = Layout::from_toml(&p).unwrap_or_else(|e| panic!("加载 {id} 失败: {e}"));
            assert_eq!(lay.id, id, "{id} 的 meta.id 不匹配");
            assert!(lay.is_final_key(b'k'), "{id} 应有 finals['k']");
        }
        // 差异点对拍：小鹤 ao=c，ai=d
        let xh = Layout::from_toml(&dir.join("xiaohe.toml")).unwrap();
        assert_eq!(
            xh.finals_of(b'c'),
            &["ao".to_string()],
            "小鹤 finals[c] 应为 ao"
        );
        assert_eq!(
            xh.finals_of(b'd'),
            &["ai".to_string()],
            "小鹤 finals[d] 应为 ai"
        );
        assert_eq!(xh.initial_of(b'v'), Some("zh"), "小鹤 initials[v] 应为 zh");

        // 自然码 ao=k，ai=l（与小鹤不同）
        let zrm = Layout::from_toml(&dir.join("ziranma.toml")).unwrap();
        assert_eq!(
            zrm.finals_of(b'k'),
            &["ao".to_string()],
            "自然码 finals[k] 应为 ao"
        );
        assert_eq!(
            zrm.finals_of(b'l'),
            &["ai".to_string()],
            "自然码 finals[l] 应为 ai"
        );

        // 微软 ;=ing
        let ms = Layout::from_toml(&dir.join("mspy.toml")).unwrap();
        assert!(ms.is_final_key(b';'), "微软双拼应有 finals[;]");
        assert_eq!(
            ms.finals_of(b';'),
            &["ing".to_string()],
            "微软 finals[;] 应为 ing"
        );

        // 智能ABC a=zh（声母），zero_initials 只有 o
        let abc = Layout::from_toml(&dir.join("abc.toml")).unwrap();
        assert_eq!(
            abc.initial_of(b'a'),
            Some("zh"),
            "智能ABC initials[a] 应为 zh"
        );
        assert_eq!(
            abc.initial_of(b'e'),
            Some("ch"),
            "智能ABC initials[e] 应为 ch"
        );
        assert_eq!(
            abc.initial_of(b'v'),
            Some("sh"),
            "智能ABC initials[v] 应为 sh"
        );
        assert!(
            abc.zero_of(b'a').is_empty(),
            "智能ABC zero_initials 不应有 a 键（a=zh 冲突）"
        );

        // 紫光 u=zh，i=sh，a=ch；zero_initials 无 a
        let zg = Layout::from_toml(&dir.join("ziguang.toml")).unwrap();
        assert_eq!(zg.initial_of(b'u'), Some("zh"), "紫光 initials[u] 应为 zh");
        assert_eq!(zg.initial_of(b'i'), Some("sh"), "紫光 initials[i] 应为 sh");
        assert_eq!(zg.initial_of(b'a'), Some("ch"), "紫光 initials[a] 应为 ch");
        assert!(
            zg.zero_of(b'a').is_empty(),
            "紫光 zero_initials 不应有 a 键（a=ch 冲突）"
        );
        assert!(zg.is_final_key(b';'), "紫光应有 finals[;]");
        assert_eq!(
            zg.finals_of(b';'),
            &["ing".to_string()],
            "紫光 finals[;] 应为 ing"
        );

        // 首道双拼：e=sh，使用 zero_pairs 而非 zero_initials
        let sd = Layout::from_toml(&dir.join("shoudao.toml")).unwrap();
        assert_eq!(sd.initial_of(b'e'), Some("sh"), "首道 initials[e] 应为 sh");
        assert!(sd.has_zero_pairs(), "首道双拼应使用 zero_pairs");
        assert_eq!(sd.zero_pair(b'u', b'e'), Some("e"), "首道 ue → e");
        assert_eq!(sd.zero_pair(b'u', b'i'), Some("ei"), "首道 ui → ei");
        assert_eq!(sd.zero_pair(b'u', b'f'), Some("eng"), "首道 uf → eng");
        assert_eq!(sd.zero_pair(b'a', b'a'), Some("a"), "首道 aa → a");
        assert_eq!(sd.zero_pair(b'o', b'o'), Some("o"), "首道 oo → o");
        assert!(
            sd.zero_of(b'a').is_empty(),
            "首道使用 zero_pairs 时 zero_initials 应为空"
        );
    }

    /// 7 个内置布局的码元集：只有 finals 里带 `;` 的三家产出非默认集，其余为 None。
    ///
    /// ⚠️ 这条断言是「`;` 能不能打出 ing」的**唯一**结构性守卫。布局 TOML 里写了
    /// `";" = ["ing"]` 只是数据；数据要变成行为，必须经由本集告诉协调器「`;` 是码元」。
    /// 引擎的 `convert("n;") == "ning"` 全绿而用户打不出，正是因为这段接线曾经不存在。
    #[test]
    fn builtin_layouts_code_char_set() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let load = |id: &str| Layout::from_toml(&dir.join(format!("{id}.toml"))).unwrap();

        for id in ["mspy", "sogou", "ziguang"] {
            let cs = load(id)
                .code_char_set()
                .unwrap_or_else(|| panic!("{id} 的 finals 含 `;`，应产出非默认码元集"));
            assert!(cs.contains(';'), "{id}：`;` 应是码元");
            assert!(
                !cs.contains_leading(';'),
                "{id}：`;` 是韵母（第二码），不得进首码集"
            );
            assert!(
                cs.contains('a') && cs.contains_leading('a'),
                "{id}：字母不受影响"
            );
            assert!(!cs.contains('['), "{id}：布局没用到的符号不得混入");
            assert!(
                cs.has_non_leading(),
                "{id}：应存在「是码元但不能起头」的字符"
            );
        }

        // 韵母键全是字母的布局 → None，协调器回落内置 a-z（零回归）。
        for id in ["xiaohe", "ziranma", "abc", "shoudao"] {
            assert!(
                load(id).code_char_set().is_none(),
                "{id} 的键全是字母，应回落默认集而非构造一份等价副本"
            );
        }
    }

    /// 非字母键出现在**第一码**（声母/零声母引导）时必须进首码集——判据是「第几码」，
    /// 不是「是不是字母」。内置布局都没这么配，故只能构造布局来锁住这条语义。
    #[test]
    fn code_char_set_leading_follows_key_position() {
        let t = r#"
[meta]
id = "x"
name = "x"
[initials]
"/" = "zh"
[finals]
";" = ["ing"]
"#;
        let cs = Layout::from_toml_str(t).unwrap().code_char_set().unwrap();
        assert!(
            cs.contains('/') && cs.contains_leading('/'),
            "声母键应可起头"
        );
        assert!(
            cs.contains(';') && !cs.contains_leading(';'),
            "韵母键不可起头"
        );
    }

    /// `-` 作码元键时必须排到规格串末位，否则 `"a-z-;"` 里的 `z-;` 被当范围符、
    /// 端点逆序 → 整串解析失败 → 静默回落 `a-z`，同布局里合法的 `;` 一起失效。
    #[test]
    fn code_char_set_hyphen_key_does_not_break_spec() {
        let t = r#"
[meta]
id = "x"
name = "x"
[finals]
"-" = ["ang"]
";" = ["ing"]
"#;
        let cs = Layout::from_toml_str(t).unwrap().code_char_set().unwrap();
        assert!(cs.contains('-'), "`-` 应是码元");
        assert!(cs.contains(';'), "`;` 不得被 `-` 的解析失败连累");
        assert!(cs.contains('a') && cs.contains('z'), "a-z 范围应完好");
    }

    #[test]
    fn zero_pairs_invalid_key_length() {
        let bad = r#"
[meta]
id = "bad"
name = "bad"
[finals]
k = ["ao"]
[zero_pairs]
abc = "a"
"#;
        let result = Layout::from_toml_str(bad);
        assert!(result.is_err(), "zero_pairs 三字符键应被拒绝");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("两字符"),
            "错误信息应提及两字符: {err_msg}"
        );
    }
}

// ============================================================================
// ShuangpinConverter 测试：逐条转写自 Go converter_test.go / converter_fuzzy_test.go。
// 真值以 Go 为准（含原样回写、partial 含声母前缀、PositionMap、MapConsumedLength）。
// ============================================================================
#[cfg(test)]
mod converter_tests {
    use super::*;

    fn schema_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin")
    }

    fn conv(id: &str) -> ShuangpinConverter {
        let p = schema_dir().join(format!("{id}.toml"));
        let layout = Layout::from_toml(&p).unwrap_or_else(|e| panic!("加载 {id} 失败: {e}"));
        ShuangpinConverter::new(layout)
    }

    // --- TestXiaoheBasic ---
    #[test]
    fn xiaohe_basic() {
        let c = conv("xiaohe");
        let cases = [
            ("ni", "ni", false),
            ("nihc", "nihao", false),
            ("womf", "women", false),
            ("n", "n", true),
            ("", "", false),
        ];
        for (input, want, want_partial) in cases {
            let r = c.convert(input);
            assert_eq!(r.full_pinyin(), want, "convert({input:?}).full_pinyin");
            assert_eq!(
                r.has_partial, want_partial,
                "convert({input:?}).has_partial"
            );
        }
    }

    // --- TestXiaoheSyllables ---
    #[test]
    fn xiaohe_syllables() {
        let c = conv("xiaohe");
        let r = c.convert("nihc");
        assert_eq!(r.syllables.len(), 2);
        assert_eq!(r.syllables[0].pinyin, "ni");
        assert_eq!(r.syllables[1].pinyin, "hao");
        assert_eq!((r.syllables[0].raw_start, r.syllables[0].raw_end), (0, 2));
        assert_eq!((r.syllables[1].raw_start, r.syllables[1].raw_end), (2, 4));
    }

    // --- TestXiaoheZhChSh ---
    #[test]
    fn xiaohe_zh_ch_sh() {
        let c = conv("xiaohe");
        let cases = [
            ("vs", "zhong"),
            ("ig", "cheng"),
            ("uf", "shen"),
            ("vv", "zhui"),
            ("dv", "dui"),
            ("gv", "gui"),
            ("go", "guo"),
            ("ho", "huo"),
            ("xp", "xie"),
            ("bp", "bie"),
            ("zz", "zou"),
            ("dz", "dou"),
            ("nv", "nv"),
            ("lv", "lv"),
        ];
        for (input, want) in cases {
            assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
        }
    }

    // --- TestXiaoheZeroInitial ---
    #[test]
    fn xiaohe_zero_initial() {
        let c = conv("xiaohe");
        let cases = [
            ("aa", "a"),
            ("oo", "o"),
            ("ee", "e"),
            ("ai", "ai"),
            ("an", "an"),
            ("ei", "ei"),
            ("en", "en"),
            ("ou", "ou"),
        ];
        for (input, want) in cases {
            assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
        }
    }

    // --- TestConsumedLengthMapping ---
    #[test]
    fn consumed_length_mapping() {
        let c = conv("xiaohe");
        let r = c.convert("nihc");
        assert_eq!(r.map_consumed_length(0), 0);
        assert_eq!(r.map_consumed_length(2), 2);
        assert_eq!(r.map_consumed_length(5), 4);
    }

    // --- TestConsumedLengthAbbrev ---
    #[test]
    fn consumed_length_abbrev() {
        let c = conv("xiaohe");
        let r = c.convert("bzd");
        assert_eq!(r.map_consumed_length(3), 3);

        let r2 = c.convert("nihcbzd");
        assert_eq!(r2.map_consumed_length(8), 7);
        assert_eq!(r2.map_consumed_length(5), 4);
    }

    // --- TestPartialInput ---
    #[test]
    fn partial_input() {
        let c = conv("xiaohe");
        let r = c.convert("nih");
        assert_eq!(r.syllables.len(), 1);
        assert!(r.has_partial);
        assert_eq!(r.partial_initial.as_deref(), Some("h"));
    }

    // --- TestZiranmaVKey ---
    #[test]
    fn ziranma_v_key() {
        let c = conv("ziranma");
        let cases = [("dv", "dui"), ("gv", "gui"), ("nv", "nv"), ("lv", "lv")];
        for (input, want) in cases {
            assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
        }
    }

    // --- TestSogouVKey ---
    #[test]
    fn sogou_v_key() {
        let c = conv("sogou");
        let cases = [("dv", "dui"), ("gv", "gui"), ("ny", "nv"), ("ly", "lv")];
        for (input, want) in cases {
            assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
        }
    }

    // --- TestZiguangScheme ---
    #[test]
    fn ziguang_scheme() {
        let c = conv("ziguang");
        let cases = [
            ("ut", "zheng"),
            ("ux", "zhua"),
            ("ir", "shan"),
            ("ik", "shei"),
            ("aq", "chao"),
            ("nb", "niao"),
            ("mw", "men"),
            ("ds", "dang"),
            ("gh", "gong"),
            ("jj", "jiu"),
            ("lk", "lei"),
            ("ll", "luan"),
            ("xy", "xin"),
            ("gz", "gou"),
            ("nn", "nve"),
            ("ln", "lve"),
        ];
        for (input, want) in cases {
            assert_eq!(c.convert(input).full_pinyin(), want, "convert({input:?})");
        }
    }

    // --- TestZeroInitialAo ---
    #[test]
    fn zero_initial_ao() {
        let cases = [
            ("xiaohe", "ac", "ao"),
            ("xiaohe", "aa", "a"),
            ("xiaohe", "ai", "ai"),
            ("xiaohe", "an", "an"),
            ("xiaohe", "ah", "ang"),
            ("ziranma", "ak", "ao"),
            ("ziranma", "aa", "a"),
            ("ziranma", "al", "ai"),
            ("ziranma", "aj", "an"),
            ("ziranma", "ah", "ang"),
            // 微软/搜狗零声母以 `o` 引导（不是首字母引导——那是自然码/小鹤的规则）。
            ("mspy", "ok", "ao"),
            ("mspy", "oa", "a"),
            ("mspy", "ol", "ai"),
            ("mspy", "oj", "an"),
            ("mspy", "oh", "ang"),
            ("sogou", "ok", "ao"),
            ("sogou", "oa", "a"),
            ("sogou", "ol", "ai"),
            ("sogou", "oj", "an"),
            ("sogou", "oh", "ang"),
        ];
        for (scheme, input, want) in cases {
            let c = conv(scheme);
            assert_eq!(
                c.convert(input).full_pinyin(),
                want,
                "{scheme} convert({input:?})"
            );
        }
    }

    // --- TestZeroInitialLiteralAo ---
    // 测的是零声母的「字面匹配」路径（convert_pair 路径 b）：击键本身就是音节全拼。
    // ⚠️ 只对**首字母引导**的方案成立。微软/搜狗改为纯 `o` 引导后其 `ao` 走的是常规
    // 声母路径（a 自映射声母 + o 键的 "o" 韵母），结果同样是 "ao" 但性质不同 ——
    // 留在这里会假绿地暗示它们的零声母字面路径还在，故移出，改由官方击键表覆盖。
    #[test]
    fn zero_initial_literal_ao() {
        for scheme in ["xiaohe", "ziranma"] {
            let c = conv(scheme);
            assert_eq!(
                c.convert("ao").full_pinyin(),
                "ao",
                "{scheme} convert(\"ao\")"
            );
        }
    }

    // --- TestPreeditDisplay ---
    #[test]
    fn preedit_display() {
        let c = conv("xiaohe");
        let r = c.convert("nihc");
        assert_eq!(r.preedit_display, "ni'hao");
    }

    // ===== converter_fuzzy_test.go =====

    // --- TestXiaoheFuzzy_SLNeedsFuzzyToShuang ---
    #[test]
    fn xiaohe_fuzzy_sl_needs_fuzzy_to_shuang() {
        let mut c = conv("xiaohe");
        // fuzzy 关：s+l 无合法 → fallback "sl"
        assert_eq!(c.convert("sl").full_pinyin(), "sl");
        // fuzzy s↔sh 开启：s+l 应被 sh+l 模糊补救 → "shuang"
        c.set_fuzzy(false, false, true);
        assert_eq!(c.convert("sl").full_pinyin(), "shuang");
        // 整段 slpb → shuangpin
        assert_eq!(c.convert("slpb").full_pinyin(), "shuangpin");
    }

    // --- TestXiaoheFuzzy_OriginalLegalNotShadowed ---
    #[test]
    fn xiaohe_fuzzy_original_legal_not_shadowed() {
        let mut c = conv("xiaohe");
        c.set_fuzzy(true, true, true);
        assert_eq!(c.convert("zi").full_pinyin(), "zi");
        assert_eq!(c.convert("zisi").full_pinyin(), "zisi");
    }

    // --- TestXiaoheFuzzy_Bidirectional ---
    #[test]
    fn xiaohe_fuzzy_bidirectional() {
        let mut c = conv("xiaohe");
        c.set_fuzzy(true, false, false);
        let results = c.convert_pair(b'v', b'd');
        assert!(results.iter().any(|s| s == "zhai"), "缺 zhai: {results:?}");
        assert!(
            results.iter().any(|s| s == "zai"),
            "应含 zai 候选: {results:?}"
        );
        assert_eq!(results[0], "zhai", "原始声母合法时应排首位: {results:?}");
    }

    // --- TestXiaoheFuzzy_DisabledSwitch ---
    #[test]
    fn xiaohe_fuzzy_disabled_switch() {
        let c = conv("xiaohe");
        let results = c.convert_pair(b's', b'l');
        assert!(results.is_empty(), "fuzzy 关时 s+l 应为空: {results:?}");
    }

    // --- 边界测试 6.1-4：符号键 `;` e2e 转换（mspy 布局，;=ing）---
    // mspy 布局中 `;` 映射 ing，输入 "n;" 应转换为 "ning"（n 声母 + ; 韵母 ing）。
    // 真值依据：mspy.toml 显式声明 ";" = ["ing"]，ning 为合法拼音音节；
    //   与 Go converter 行为对齐（finals 路径：initial_of('n')="n" + finals_of(';')=["ing"] → "ning"）。
    #[test]
    fn mspy_semicolon_final_e2e() {
        let c = conv("mspy");
        // 单音节：n + ; → ning
        let r = c.convert("n;");
        assert_eq!(r.full_pinyin(), "ning", "mspy: n; 应转换为 ning");
        assert_eq!(r.syllables.len(), 1, "应有 1 个音节");
        assert_eq!(r.syllables[0].pinyin, "ning");
        assert_eq!((r.syllables[0].raw_start, r.syllables[0].raw_end), (0, 2));

        // 多音节：n; + ni → ning + ni（第二对是普通双拼）
        let r2 = c.convert("n;ni");
        assert_eq!(r2.full_pinyin(), "ningni", "mspy: n;ni 应转换为 ningni");
        assert_eq!(r2.syllables.len(), 2);
        assert_eq!(r2.syllables[0].pinyin, "ning");
        assert_eq!(r2.syllables[1].pinyin, "ni");
    }

    // ===== 首道双拼（zero_pairs）=====

    // 零声母：12 条显式映射全覆盖
    #[test]
    fn shoudao_zero_pairs_all() {
        let c = conv("shoudao");
        let cases = [
            ("aa", "a"),
            ("ai", "ai"),
            ("an", "an"),
            ("ay", "ang"),
            ("ao", "ao"),
            ("ue", "e"),
            ("ui", "ei"),
            ("en", "en"),
            ("uf", "eng"),
            ("er", "er"),
            ("oo", "o"),
            ("ou", "ou"),
        ];
        for (input, want) in cases {
            assert_eq!(
                c.convert(input).full_pinyin(),
                want,
                "首道 convert({input:?})"
            );
        }
    }

    // 常规声母+韵母不受 zero_pairs 影响
    #[test]
    fn shoudao_regular_initials() {
        let c = conv("shoudao");
        let cases = [
            ("vs", "zhou"),  // v=zh, s=ou
            ("ef", "sheng"), // e=sh, f=eng
            ("id", "chao"),  // i=ch, d=ao
            ("ni", "ni"),    // n=n, i=i
            ("hd", "hao"),   // h=h, d=ao
        ];
        for (input, want) in cases {
            assert_eq!(
                c.convert(input).full_pinyin(),
                want,
                "首道 convert({input:?})"
            );
        }
    }

    // ef 只产出 sheng，不应有零声母 eng 污染
    #[test]
    fn shoudao_ef_no_eng_pollution() {
        let c = conv("shoudao");
        let results = c.convert_pair(b'e', b'f');
        assert_eq!(results, vec!["sheng"], "ef 应仅产出 sheng: {results:?}");
    }

    // ee 走常规路径 sh+e=she，不产出零声母 e
    #[test]
    fn shoudao_ee_is_she() {
        let c = conv("shoudao");
        assert_eq!(c.convert("ee").full_pinyin(), "she", "首道 ee → she");
    }

    // 多音节混合
    #[test]
    fn shoudao_multi_syllable() {
        let c = conv("shoudao");
        // ni + ue(→e) → nie? no: ni=ni, ue=e → "nie"? Let me think...
        // convert processes in pairs: (n,i)=ni, (u,e)=e → "nie"
        let r = c.convert("niue");
        assert_eq!(r.full_pinyin(), "nie", "首道 niue → ni+e");
        assert_eq!(r.syllables.len(), 2);
        assert_eq!(r.syllables[0].pinyin, "ni");
        assert_eq!(r.syllables[1].pinyin, "e");

        // hd + uf(→eng) → hao+eng
        let r2 = c.convert("hduf");
        assert_eq!(r2.full_pinyin(), "haoeng", "首道 hduf → hao+eng");
        assert_eq!(r2.syllables.len(), 2);
    }

    // 奇数尾键 partial
    #[test]
    fn shoudao_partial() {
        let c = conv("shoudao");
        let r = c.convert("aal");
        assert_eq!(r.syllables.len(), 1);
        assert_eq!(r.syllables[0].pinyin, "a");
        assert!(r.has_partial);
        assert_eq!(r.partial_initial.as_deref(), Some("l"));
    }

    // consumed_length 回映射
    #[test]
    fn shoudao_consumed_length() {
        let c = conv("shoudao");
        // ue → "e"（全拼 1 字节 → 双拼 2 字节）
        let r = c.convert("ue");
        assert_eq!(r.map_consumed_length(1), 2);

        // aa + ue → "a" + "e"（全拼各 1 字节）
        let r2 = c.convert("aaue");
        assert_eq!(r2.map_consumed_length(1), 2); // "a" 消耗双拼 2 字节
        assert_eq!(r2.map_consumed_length(2), 4); // "ae" 消耗双拼 4 字节
    }

    // 旧方案 zero_initials 不受影响（回归）
    #[test]
    fn xiaohe_zero_initials_unchanged() {
        let c = conv("xiaohe");
        let layout = c.layout();
        assert!(!layout.has_zero_pairs(), "小鹤不应有 zero_pairs");
        let cases = [
            ("aa", "a"),
            ("oo", "o"),
            ("ee", "e"),
            ("ac", "ao"),
            ("ah", "ang"),
            ("ew", "ei"),
            ("ef", "en"),
        ];
        for (input, want) in cases {
            assert_eq!(
                c.convert(input).full_pinyin(),
                want,
                "小鹤 convert({input:?}) 回归"
            );
        }
    }

    // ========================================================================
    // 反向表（音节 → 双拼键对）
    // ========================================================================

    /// 全部内置布局。新增布局文件后这里会自动带上——`schema_dir` 下的每个 `.toml`。
    fn all_builtin_ids() -> Vec<String> {
        let mut ids: Vec<String> = std::fs::read_dir(schema_dir())
            .expect("读取双拼布局目录")
            .filter_map(|e| {
                let p = e.ok()?.path();
                (p.extension()? == "toml").then(|| p.file_stem()?.to_str().map(str::to_string))?
            })
            .collect();
        ids.sort();
        assert!(ids.len() >= 8, "内置布局应至少 8 个，实得 {}", ids.len());
        ids
    }

    /// ★ 反向表的**正确性定义**：表里说音节 S 敲 `[k1,k2]`，那么敲下这个键对就必须
    /// 能得到 S —— 即 S ∈ `convert_pair(k1, k2)`。
    ///
    /// 用「包含」而非「恒等」作全量判据，是因为建表第二遍会给「只能作次选出现」的
    /// 冷僻音节兜底（它们在该键对的候选列表里排第二及以后）。对这些音节，用户敲出
    /// 键对后要往下翻一格才选得到，但编码本身没有错。恒等那一档由下面的
    /// `reverse_common_syllables_are_first_choice` 单独钉。
    #[test]
    fn reverse_table_is_decodable_for_all_builtin_layouts() {
        for id in all_builtin_ids() {
            let c = conv(&id);
            let rev = c.build_reverse();
            assert!(!rev.is_empty(), "{id}：反向表不应为空");
            // 实测全部内置布局恒为 411 个音节。留一点余量给将来的布局差异，
            // 但掉到 400 以下就不是差异而是枚举漏了键。
            assert!(
                rev.len() >= 400,
                "{id}：反向表只收录了 {} 个音节，疑似枚举漏键",
                rev.len()
            );
            for (syl, keys) in &rev.map {
                let decoded = c.convert_pair(keys[0], keys[1]);
                assert!(
                    decoded.iter().any(|d| d == syl),
                    "{id}：反向表说 {syl:?} 敲 {:?}，但该键对解出的是 {decoded:?}",
                    std::str::from_utf8(keys).unwrap()
                );
            }
        }
    }

    /// 常用音节必须是**首选**（敲完即得，不用往下翻）。
    ///
    /// 上一条只保证「选得到」，这条保证「一敲就是它」。注释里显示的编码若要用户
    /// 翻一格才对得上，用户会认为编码显示错了。
    #[test]
    fn reverse_common_syllables_are_first_choice() {
        // 覆盖各条取码路径：常规声母韵母、zh/ch/sh 独立键、ü 归一化、零声母、整体认读。
        const COMMON: &[&str] = &[
            "ni", "hao", "wo", "men", "zhong", "guo", "shi", "chi", "zhuang", "chuang", "shuang",
            "nv", "lv", "ju", "qu", "xu", "yu", "jue", "que", "xue", "yue", "jun", "qun", "xun",
            "a", "e", "o", "ai", "ei", "ou", "an", "en", "ang", "eng", "er", "ao", "ying", "yin",
        ];
        for id in all_builtin_ids() {
            let c = conv(&id);
            let rev = c.build_reverse();
            for syl in COMMON {
                let Some(keys) = rev.encode(syl) else {
                    panic!("{id}：常用音节 {syl:?} 不在反向表里");
                };
                let decoded = c.convert_pair(keys[0], keys[1]);
                assert_eq!(
                    decoded.first().map(String::as_str),
                    Some(*syl),
                    "{id}：{syl:?} 编码为 {:?}，但该键对的首选是 {:?}",
                    std::str::from_utf8(&keys).unwrap(),
                    decoded.first()
                );
            }
        }
    }

    /// 整串往返：`encode_all` 出来的击键串喂回 `convert`，必须还原成同一串全拼。
    ///
    /// 这一条比逐音节判据更接近真机——它连带验证了「每音节恒 2 键」这个隐含前提，
    /// 若哪天有布局的零声母要敲 3 键，这里会红。
    #[test]
    fn reverse_roundtrip_through_convert() {
        const WORDS: &[(&[&str], &str)] = &[
            (&["ni", "hao"], "nihao"),
            (&["wo", "men"], "women"),
            (&["zhong", "guo"], "zhongguo"),
            (&["shu", "ru", "fa"], "shurufa"),
            (&["a", "yi"], "ayi"),
        ];
        for id in all_builtin_ids() {
            let c = conv(&id);
            let rev = c.build_reverse();
            for (syls, want_full) in WORDS {
                let keys = rev
                    .encode_all(syls)
                    .unwrap_or_else(|| panic!("{id}：{syls:?} 编码失败"));
                assert_eq!(
                    keys.len(),
                    syls.len() * 2,
                    "{id}：{syls:?} 应得 {} 键，实得 {keys:?}",
                    syls.len() * 2
                );
                assert_eq!(
                    c.convert(&keys).full_pinyin(),
                    *want_full,
                    "{id}：{syls:?} → {keys:?} → 回解不符"
                );
            }
        }
    }

    /// ★ 建表必须无视模糊音开关。
    ///
    /// `chua` 在小鹤下的真值键对是 `ix`（`i` = ch，`x` = ua）。但开着 ch↔c 模糊音时，
    /// `cx` 这个键对的 `c`+`ua` = "cua" 不是合法音节被 trie 拒掉，模糊对偶 `ch`+`ua`
    /// = "chua" 却合法，于是 "chua" 成了 `cx` 的首选；而枚举序 `c` 在 `i` 之前，
    /// 若建表跟随模糊开关，"chua" 就会被登记成 `cx`。
    ///
    /// 那不是「显示得不够好」，是**显示的编码是错的**：模糊音是输入时的宽容，
    /// 不是这个字的打法。
    #[test]
    fn reverse_ignores_fuzzy_initials() {
        let plain = conv("xiaohe").build_reverse();

        let mut fuzzy_conv = conv("xiaohe");
        fuzzy_conv.set_fuzzy(true, true, true);
        let fuzzy = fuzzy_conv.build_reverse();

        for syl in [
            "chua", "shua", "zhuang", "chuang", "shuang", "zhi", "chi", "shi",
        ] {
            assert_eq!(
                plain.encode(syl),
                fuzzy.encode(syl),
                "{syl:?}：模糊音开关不应影响反向表"
            );
        }
        // 钉死具体值，免得两边一起错还互相印证。
        assert_eq!(plain.encode("chua"), Some(*b"ix"), "小鹤 chua 应为 ix");
        assert_eq!(plain.encode("shua"), Some(*b"ux"), "小鹤 shua 应为 ux");
        assert_eq!(plain.encode("zhuang"), Some(*b"vl"), "小鹤 zhuang 应为 vl");
    }

    /// 三个零声母流派各自按自己的规矩出码，数据说了算。
    ///
    /// 小鹤=首字母引导(`aa`)、微软=O 引导(`oa`)、首道=显式 `zero_pairs`(`aa`)。
    /// 这条同时守着 `convert_pair` 里「单韵母重复键只对把该键配成零声母引导键的方案
    /// 生效」那道闸——若它退回无条件放行，微软的 `a` 会变成 `aa`，这里立刻红。
    #[test]
    fn reverse_zero_initial_schools() {
        assert_eq!(
            conv("xiaohe").build_reverse().encode("a"),
            Some(*b"aa"),
            "小鹤：首字母引导"
        );
        assert_eq!(
            conv("mspy").build_reverse().encode("a"),
            Some(*b"oa"),
            "微软：O 引导"
        );
        assert_eq!(
            conv("shoudao").build_reverse().encode("a"),
            Some(*b"aa"),
            "首道：显式 zero_pairs"
        );
    }

    /// 查不到的音节返回 `None`；整串里**任一**音节查不到即整串 `None`。
    ///
    /// 不做「查得到的拼上、查不到的跳过」：半截击键串是错的答案而不是不完整的答案。
    #[test]
    fn reverse_rejects_unknown_syllables() {
        let rev = conv("xiaohe").build_reverse();
        assert_eq!(rev.encode("zzz"), None);
        assert_eq!(rev.encode(""), None);
        assert_eq!(rev.encode_all(&[]), None, "空串没有编码可言");
        assert_eq!(
            rev.encode_all(&["ni", "zzz"]),
            None,
            "一个音节查不到，整串就不该给答案"
        );
        assert!(rev.encode_all(&["ni", "hao"]).is_some());
    }

    /// 建表第二遍（给「只能作次选出现」的音节兜底）的**唯一**受益者：`lo`。
    ///
    /// `o` 键在全部 8 个内置布局下都配着 `["uo", "o"]` 两个韵母，于是 `l` + `o` 解出
    /// 两个都合法的音节 `["luo", "lo"]`——「罗」占了首选，「咯」只能从第二遍拿到
    /// 同一个键对。实测这是全部内置布局中**有且仅有**的一个多解键对。
    ///
    /// ⚠️ 没有这条测试，第二遍扫描就是一段**无法被任何测试区分**的代码：删掉它，
    /// 反向表从 411 个音节掉到 410，而其余每一条断言照常全绿。这一点是实测出来的
    /// （变异检验：合并两遍扫描后 6 条测试全过），不是推演。
    #[test]
    fn reverse_second_pass_rescues_secondary_syllables() {
        for id in all_builtin_ids() {
            let c = conv(&id);
            assert_eq!(
                c.convert_pair(b'l', b'o'),
                vec!["luo".to_string(), "lo".to_string()],
                "{id}：`lo` 键对应解出两个音节，luo 在前"
            );
            assert_eq!(
                c.build_reverse().encode("lo"),
                Some(*b"lo"),
                "{id}：次选音节 lo 也要能反查到键对"
            );
        }
    }

    /// 两遍扫描的**顺序**保障：同一个音节既是 A 键对的次选、又是 B 键对的首选时，
    /// 反向表必须给 B。
    ///
    /// ⚠️ 现有 8 个内置布局**触发不了**这个差异——实测它们各自只有一个多解键对
    /// （`lo → [luo, lo]`），而 `lo` 在别处并不是谁的首选，故两遍与一遍结果完全相同。
    /// 所以这里用一份构造布局把差异造出来，否则「先扫首选、再扫次选」这个顺序就是
    /// 一段无法被任何测试区分的代码（变异检验实测：合并两遍后其余测试全绿）。
    ///
    /// 差异的实际后果：用户照着注释显示的编码敲下去，出来的是**另一个字**，
    /// 要往下翻一格才是他要的那个。
    #[test]
    fn reverse_prefers_first_choice_pair_over_secondary() {
        // `a` 键配两个韵母（ang 在前、a 在后），`z` 键只配 `a`。
        // 于是 "ba" 是 `ba` 键对的**次选**（首选是 "bang"），却是 `bz` 键对的首选。
        // 而枚举序里 `ba` 在 `bz` 之前。
        const DUAL: &str = r#"
[meta]
id = "test_dual"
name = "测试夹具：同一音节既是次选又是首选"

[finals]
a = ["ang", "a"]
z = ["a"]
"#;
        let c = ShuangpinConverter::new(Layout::from_toml_str(DUAL).unwrap());
        assert_eq!(
            c.convert_pair(b'b', b'a'),
            vec!["bang".to_string(), "ba".to_string()],
            "夹具前提：`ba` 键对解出两个音节，ba 排第二"
        );
        assert_eq!(
            c.convert_pair(b'b', b'z'),
            vec!["ba".to_string()],
            "夹具前提：`bz` 键对只解出 ba，是首选"
        );

        let rev = c.build_reverse();
        assert_eq!(
            rev.encode("ba"),
            Some(*b"bz"),
            "ba 应取「一敲即得」的 bz，而不是要翻一格的 ba"
        );
        assert_eq!(rev.encode("bang"), Some(*b"ba"), "bang 本就是 ba 的首选");
    }

    /// 枚举必须按**排序后**的键走，否则反向表随哈希序漂移。
    ///
    /// 实测每个内置布局都有 5~10 个音节被多个键对同时当作首选。最典型的是 ü 归一化
    /// 造成的 `ju ← [ju, jv]`（`normalize_pinyin` 把 j/q/x/y 后的 `v` 转成 `u`，
    /// 于是 `j`+`u` 与 `j`+`v` 殊途同归）；微软/搜狗那边是 `[ju, jy]`。
    /// 零声母也有：abc 的 `ou ← [ob, ou]`、微软的 `ai ← [ai, ol]`。
    ///
    /// 这些音节取哪个键对**完全由枚举顺序决定**。不排序就是由 `HashMap` 的哈希序
    /// 决定，而那个序**跨进程随机** —— 后果不是测试时绿时红那么轻：同一个字，
    /// 用户今天看到编码是 `ju`，重启输入法后变成 `jv`。编码显示不稳定，用户就没法信它。
    #[test]
    fn reverse_table_is_deterministic_under_first_choice_conflicts() {
        for id in all_builtin_ids() {
            let rev = conv(&id).build_reverse();
            for syl in ["ju", "qu", "xu", "yu"] {
                let keys = rev
                    .encode(syl)
                    .unwrap_or_else(|| panic!("{id}：{syl} 不在反向表里"));
                let initial = syl.as_bytes()[0];
                assert_eq!(keys[0], initial, "{id}：{syl} 的首码应是声母键本身");
                // 排序后字典序小者胜：`u` < `v`（abc/佳佳/首道）、`u` < `y`（微软/搜狗）。
                // 这也正是各方案文档里写的标准打法。
                assert_eq!(
                    keys[1], b'u',
                    "{id}：{syl} 应取 {}u，而不是 v/y 键那条等价路径",
                    initial as char
                );
            }
        }
    }
}
