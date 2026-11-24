//! 用户词频（redb）—— 与权重解耦
//!
//! 见 docs/redesign/frequency.md：词频只记真实数据 `{count, last_used}`，不加到 weight。
//! 作为排序独立维度：码表用 count（used-first），拼音用衰减后的位置提升
//! （`docs/design/freq-rerank-model.md`）。
//!
//! redb FREQ 表，key=`"{schema}\0{code}\0{text}"`（store.md §2，统一 \0 分隔），value 定长 12B
//! （count u32 + last_used i64）。
//!
//! 本文件另保留 legacy `FreqTracker`（文件式），目前只有它自己的单测在用。

use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// 分页词频查询结果：`(本页的 (code, text, 记录) 列表, 匹配总数)`。
pub type FreqPage = (Vec<(String, String, FreqRecord)>, usize);

/// 词频记录（解耦权重，只记真实使用数据）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreqRecord {
    pub count: u32,
    /// 最近使用（unix 秒）
    pub last_used: i64,
}

/// 词频参数：码表用 count 直接比较不需要它；拼音用其衰减打分。
#[derive(Debug, Clone, Copy)]
pub struct FreqProfile {
    pub base_scale: f64,
    /// 衰减半衰期（小时）
    pub half_life_hours: f64,
    /// 近用峰值加成：与使用次数无关，随半衰期自然消退；=0 退化为原公式
    pub recency_peak: f64,
}

impl Default for FreqProfile {
    fn default() -> Self {
        Self {
            base_scale: 100.0,
            half_life_hours: 72.0,
            recency_peak: 0.0,
        }
    }
}

impl FreqProfile {
    /// 半衰期衰减因子，落在 `(0, 1]`：刚用过 ≈ 1，久未用 → 0。
    ///
    /// 从 [`Self::pinyin_score`] 中拆出，供拼音的**位置提升**模型单独取用
    /// （`docs/design/freq-rerank-model.md`）——那边把衰减乘在**使用次数**上
    /// （「久未用 ⇒ 当初那些使用逐渐不算数」），与 `pinyin_score` 的 `log2` 打分形状无关，
    /// 但衰减这一维两者共用，故抽出以免公式落成两份。
    pub fn decay_factor(&self, rec: &FreqRecord, now: i64) -> f64 {
        let age_hours = (now - rec.last_used).max(0) as f64 / 3600.0;
        (-std::f64::consts::LN_2 * age_hours / self.half_life_hours).exp()
    }

    /// 当前生产路径不调用本函数（仅其自身单测使用）：拼音侧只取 [`Self::decay_factor`]
    /// 做位置提升。连同 `base_scale` / `recency_peak` 两个配置项暂留，删配置项要跨仓改
    /// `wind-setting` 的守门测试。
    ///
    /// 拼音词频衰减分（frequency.md §4）：
    /// `(base_scale * log2(count+1) + recency_peak) * exp(-ln2*age/half_life)`。
    /// 最近+高频 → 分高；久未用 → 衰减回落。count=0 返回 0。
    pub fn pinyin_score(&self, rec: &FreqRecord, now: i64) -> f64 {
        if rec.count == 0 {
            return 0.0;
        }
        (self.base_scale * ((rec.count + 1) as f64).log2() + self.recency_peak)
            * self.decay_factor(rec, now)
    }
}

/// value: count u32 + last_used i64 = 12 字节
pub(crate) fn enc_freq(count: u32, last_used: i64) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&count.to_le_bytes());
    b[4..12].copy_from_slice(&last_used.to_le_bytes());
    b
}

pub(crate) fn dec_freq(b: &[u8]) -> Option<FreqRecord> {
    if b.len() < 12 {
        return None;
    }
    Some(FreqRecord {
        count: u32::from_le_bytes(b[0..4].try_into().ok()?),
        last_used: i64::from_le_bytes(b[4..12].try_into().ok()?),
    })
}

/// 引擎词频 → 门面记录。
pub(crate) fn freq_of(r: wind_userdata::runtime::FreqRec) -> FreqRecord {
    FreqRecord {
        count: r.count,
        last_used: r.last_used,
    }
}

impl Store {
    /// 记录一次选词：count++、last_used=now（单写事务）。
    /// 注：当前同步写；如成为热点可改为内存累积 + 批量 flush（store.md §2 异步批量）。
    pub fn record_freq(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        self.runtime().record_freq(schema, code, text)?;
        self.after_write();
        Ok(())
    }

    /// **批量**取词频：一次读事务查多个 `(schema, code, text)`，按入参顺序返回。
    ///
    /// 逐个调 [`Self::get_freq`] 会为**每个候选**开一次 redb 读事务——五笔单字母下有
    /// 78+ 个候选，即 78 次 `begin_read` + `open_table`。事务本身要取读快照、走锁，
    /// 单次不贵但乘以候选数就成了每键的固定开销，与方案 TOML 的重复解析叠加后可感知。
    ///
    /// 三元组带 schema 而非统一一个：混输按候选来源分流归属 id（码表半边与拼音半边
    /// 落在不同 schema），单一入参表达不了。
    pub fn get_freq_batch(
        &self,
        keys: &[(String, String, String)],
    ) -> anyhow::Result<Vec<Option<FreqRecord>>> {
        Ok(self
            .runtime()
            .freq_batch(keys)
            .into_iter()
            .map(|r| r.map(freq_of))
            .collect())
    }

    /// 取词频记录（不存在返回 None）。
    pub fn get_freq(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<Option<FreqRecord>> {
        Ok(self.runtime().freq(schema, code, text).map(freq_of))
    }

    /// 流式遍历某方案下以 `prefix` 开头的词频记录。回调返回 `false` 即停。
    ///
    /// 与 [`Store::for_each_user_word`] / [`Store::for_each_temp_word`] 同构：`code` /
    /// `text` 直接借 redb 的页，不物化。**本模块「哪些记录算数」的唯一一份判据**，
    /// [`Self::list_freq_paged`] 与设置页的搜索都走它。
    pub fn for_each_freq(
        &self,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(&str, &str, FreqRecord) -> bool,
    ) -> anyhow::Result<()> {
        self.runtime()
            .for_each_freq(schema, prefix, &mut |c, t, r| f(c, t, freq_of(r)));
        Ok(())
    }

    /// 列举某方案的词频记录（设置页用）：按 code 前缀过滤，分页。
    /// 返回 `(本页 [(code,text,记录)], 总数)`。limit=0 表示不限。
    ///
    /// **只物化本页**。从前是「先全收进 Vec 再 `skip/take`」，而词频表与用户词表同量级
    /// （每选一次词就记一条），19 万条时翻第一页要先造 38 万个 `String`。
    ///
    /// ⚠️ 仍然扫完整表、不能早退：`total` 要精确（分页器显示「共 N 条」）。省的是物化，
    /// 不是扫描；redb 的读缓存该填还是填，见 [`Store::drop_page_cache`]。
    pub fn list_freq_paged(
        &self,
        schema: &str,
        prefix: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<FreqPage> {
        let (rows, total) = self
            .runtime()
            .list_freq_paged(schema, prefix, offset, limit);
        Ok((
            rows.into_iter()
                .map(|(c, t, r)| (c, t, freq_of(r)))
                .collect(),
            total,
        ))
    }

    /// 导出某方案全部词频为 jsonl（每行 {"code","text","count","last_used"}）。
    pub fn export_freq_jsonl(&self, schema: &str) -> anyhow::Result<String> {
        let (rows, _total) = self.list_freq_paged(schema, "", 0, 0)?;
        let mut out = String::new();
        for (code, text, rec) in rows {
            out.push_str(&serde_json::to_string(&serde_json::json!({
                "code": code, "text": text, "count": rec.count, "last_used": rec.last_used,
            }))?);
            out.push('\n');
        }
        Ok(out)
    }

    /// 从 jsonl 导入词频（单写事务；Merge=已存在取 max(count)/max(last_used)）。
    /// 返回 (imported, skipped)；非法行跳过计数。
    pub fn import_freq_jsonl(&self, schema: &str, text: &str) -> anyhow::Result<(usize, usize)> {
        self.check_learned_not_held(wind_userdata::record::Target::Freq, schema)?;
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        let mut rows: Vec<(String, String, u32, i64)> = Vec::new();
        let mut skipped = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                skipped += 1;
                continue;
            };
            let (Some(code), Some(word)) = (
                v.get("code").and_then(|x| x.as_str()),
                v.get("text").and_then(|x| x.as_str()),
            ) else {
                skipped += 1;
                continue;
            };
            let count = v.get("count").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let last_used = v.get("last_used").and_then(|x| x.as_i64()).unwrap_or(0);
            rows.push((code.to_string(), word.to_string(), count, last_used));
        }
        let imported = rows.len();
        let rows: Vec<(String, String, wind_userdata::runtime::FreqRec)> = rows
            .into_iter()
            .map(|(c, t, count, last_used)| {
                (c, t, wind_userdata::runtime::FreqRec { count, last_used })
            })
            .collect();
        self.runtime().import_freq_rows(schema, &rows)?;
        Ok((imported, skipped))
    }

    /// 从 `FreqIo` 行导入词频（单写事务；Merge=已存在取 max(count)/max(last_used)）。返回导入条数。
    pub fn import_freq_rows(
        &self,
        schema: &str,
        rows: &[crate::wdict::FreqIo],
    ) -> anyhow::Result<usize> {
        self.check_learned_not_held(wind_userdata::record::Target::Freq, schema)?;
        let rows: Vec<(String, String, wind_userdata::runtime::FreqRec)> = rows
            .iter()
            .map(|r| {
                let (code, _) = crate::wdict::split_spaced_code(&r.code);
                (
                    code,
                    r.text.clone(),
                    wind_userdata::runtime::FreqRec {
                        count: r.count,
                        last_used: r.last_used,
                    },
                )
            })
            .collect();
        self.runtime().import_freq_rows(schema, &rows)
    }

    /// 删除一条词频（不存在静默成功）。
    pub fn delete_freq(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        self.check_learned_not_held(wind_userdata::record::Target::Freq, schema)?;
        self.runtime().delete_freq(schema, code, text)
    }

    /// 一批 [`Self::delete_freq`]（不存在的跳过）：引擎一次 `apply_batch`。
    pub fn delete_freqs(&self, schema: &str, keys: &[(String, String)]) -> anyhow::Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.check_learned_not_held(wind_userdata::record::Target::Freq, schema)?;
        self.runtime().delete_freqs(schema, keys)
    }

    /// 清空某方案全部词频，返回删除条数。
    pub fn clear_freq(&self, schema: &str) -> anyhow::Result<usize> {
        self.check_learned_not_held(wind_userdata::record::Target::Freq, schema)?;
        self.runtime().clear_freq(schema)
    }
}

// ───────────────────────── legacy（无生产调用方）─────────────────────────

/// 运行时词频跟踪器（文件式，简化模型；无生产调用方，新代码请用 Store 的 redb 词频）。
pub struct FreqTracker {
    freq_map: RwLock<HashMap<String, u32>>,
    profile: FreqProfile,
}

impl Default for FreqTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FreqTracker {
    pub fn new() -> Self {
        Self {
            freq_map: RwLock::new(HashMap::new()),
            profile: FreqProfile::default(),
        }
    }

    pub fn record_selection(&self, word: &str) {
        let mut map = self.freq_map.write().unwrap_or_else(|e| e.into_inner());
        *map.entry(word.to_string()).or_insert(0) += 1;
    }

    pub fn get_boost(&self, word: &str) -> f64 {
        let map = self.freq_map.read().unwrap_or_else(|e| e.into_inner());
        let count = *map.get(word).unwrap_or(&0);
        if count == 0 {
            return 0.0;
        }
        ((count + 1) as f64).log2() * self.profile.base_scale * 0.1
    }

    pub fn get_count(&self, word: &str) -> u32 {
        let map = self.freq_map.read().unwrap_or_else(|e| e.into_inner());
        *map.get(word).unwrap_or(&0)
    }

    pub fn contains(&self, word: &str) -> bool {
        self.freq_map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(word)
    }

    pub fn export_records(&self) -> Vec<(String, u32)> {
        let map = self.freq_map.read().unwrap_or_else(|e| e.into_inner());
        map.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    pub fn import_records(&self, records: &[(String, u32)]) {
        let mut map = self.freq_map.write().unwrap_or_else(|e| e.into_inner());
        for (word, count) in records {
            map.insert(word.clone(), *count);
        }
    }

    pub fn clear(&self) {
        self.freq_map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// 从文件加载词频（`word\tcount` 每行一条）。文件不存在静默忽略。
    pub fn load_from_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // 行尾规整：`word\tcount` 是逐行格式，孤立 \r 会让整份文件算成一行 → 只认出
        // 一条（还是带 \r 的垃圾词）。这个入口被守卫的**文件粒度**放过了——同文件的
        // `import_freq_jsonl` 调了 normalize_input，整个 freq.rs 就被判成「已覆盖」。
        let content = wind_utils::text::normalize_input(&content);
        let mut records = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            if let (Some(w), Some(c)) = (it.next(), it.next())
                && let Ok(count) = c.trim().parse::<u32>()
                && !w.is_empty()
                && count > 0
            {
                records.push((w.to_string(), count));
            }
        }
        self.import_records(&records);
        Ok(())
    }

    /// 保存词频到文件（原子写）。
    pub fn save_to_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = String::new();
        {
            let map = self.freq_map.read().unwrap_or_else(|e| e.into_inner());
            for (word, count) in map.iter() {
                if *count > 0 {
                    out.push_str(word);
                    out.push('\t');
                    out.push_str(&count.to_string());
                    out.push('\n');
                }
            }
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.freq_map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
#[path = "freq_tests.rs"]
mod tests;
