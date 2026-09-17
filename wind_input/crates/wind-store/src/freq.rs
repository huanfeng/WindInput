//! 用户词频（redb）—— 与权重**彻底解耦**的新模型
//!
//! 见 docs/redesign/frequency.md：词频只记真实数据 `{count, last_used}`，不再加到 weight、
//! 不再有 streak/boost。作为**排序独立维度**：码表用 count（used-first），拼音用衰减分。
//!
//! redb FREQ 表，key=`"{schema}\0{code}\0{text}"`（store.md §2，统一 \0 分隔），value 定长 12B
//! （count u32 + last_used i64）。
//!
//! 说明：本文件另保留 legacy `FreqTracker`（文件式，供 coordinator 过渡期使用），将在
//! coordinator 接通 redb 词频时移除。

use crate::store::{FREQ, Store};
use crate::user_words::{enc_key, now_secs};
use redb::ReadableTable;
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

    /// ⚠️ **当前生产路径不再调用本函数**（仅其自身单测使用）。
    ///
    /// 拼音侧已改为**位置提升**模型（`docs/design/freq-rerank-model.md`），只取
    /// [`Self::decay_factor`]，不做打分。本函数连同它依赖的 `base_scale` / `recency_peak`
    /// 两个配置项一并成为死链——保留而非删除，是因为删配置项要跨仓改 `wind-setting`
    /// 的五道守门测试，且将来若恢复打分模型可直接复用。
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
fn enc_freq(count: u32, last_used: i64) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&count.to_le_bytes());
    b[4..12].copy_from_slice(&last_used.to_le_bytes());
    b
}

fn dec_freq(b: &[u8]) -> Option<FreqRecord> {
    if b.len() < 12 {
        return None;
    }
    Some(FreqRecord {
        count: u32::from_le_bytes(b[0..4].try_into().ok()?),
        last_used: i64::from_le_bytes(b[4..12].try_into().ok()?),
    })
}

impl Store {
    /// 记录一次选词：count++、last_used=now（单写事务）。
    /// 注：当前同步写；如成为热点可改为内存累积 + 批量 flush（store.md §2 异步批量）。
    pub fn record_freq(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        let key = enc_key(schema, code, text);
        let now = now_secs();
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(FREQ)?;
                let count = t
                    .get(key.as_str())?
                    .and_then(|g| dec_freq(g.value()))
                    .map(|r| r.count)
                    .unwrap_or(0);
                t.insert(
                    key.as_str(),
                    enc_freq(count.saturating_add(1), now).as_slice(),
                )?;
            }
            txn.commit()?;
            Ok(())
        })
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
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(FREQ)?;
            let mut out = Vec::with_capacity(keys.len());
            for (schema, code, text) in keys {
                let key = enc_key(schema, code, text);
                out.push(t.get(key.as_str())?.and_then(|g| dec_freq(g.value())));
            }
            Ok(out)
        })
    }

    /// 取词频记录（不存在返回 None）。
    pub fn get_freq(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<Option<FreqRecord>> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(FREQ)?;
            Ok(t.get(key.as_str())?.and_then(|g| dec_freq(g.value())))
        })
    }

    /// 列举某方案的词频记录（设置页用）：按 code 前缀过滤，分页。
    /// 返回 `(本页 [(code,text,记录)], 总数)`。limit=0 表示不限。
    pub fn list_freq_paged(
        &self,
        schema: &str,
        prefix: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<FreqPage> {
        let scan = format!("{schema}\u{0}{prefix}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(FREQ)?;
            let mut all = Vec::new();
            for item in t.range(scan.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&scan) {
                    break;
                }
                if let (Some((_, code, text)), Some(rec)) =
                    (crate::user_words::split_key(key), dec_freq(v.value()))
                {
                    all.push((code.to_string(), text.to_string(), rec));
                }
            }
            let total = all.len();
            let page: Vec<_> = all
                .into_iter()
                .skip(offset)
                .take(if limit == 0 { usize::MAX } else { limit })
                .collect();
            Ok((page, total))
        })
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
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(FREQ)?;
                for (code, word, count, last_used) in &rows {
                    let key = enc_key(schema, code, word);
                    let merged = match t.get(key.as_str())?.and_then(|g| dec_freq(g.value())) {
                        Some(old) => (old.count.max(*count), old.last_used.max(*last_used)),
                        None => (*count, *last_used),
                    };
                    t.insert(key.as_str(), enc_freq(merged.0, merged.1).as_slice())?;
                }
            }
            txn.commit()?;
            Ok(())
        })?;
        Ok((imported, skipped))
    }

    /// 从 `FreqIo` 行导入词频（单写事务；Merge=已存在取 max(count)/max(last_used)）。返回导入条数。
    pub fn import_freq_rows(
        &self,
        schema: &str,
        rows: &[crate::wdict::FreqIo],
    ) -> anyhow::Result<usize> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(FREQ)?;
                for r in rows {
                    // code 列可能带音节空格（导出端与 words/temp_words 同形），而词频表 key
                    // 是扁平的。**不拆就会写进一条永不匹配任何候选的死键**——查询侧拿的是
                    // 候选的扁平 code，那条记录再也读不到，用户只会看到「调频不生效」。
                    let (code, _) = crate::wdict::split_spaced_code(&r.code);
                    let key = enc_key(schema, &code, &r.text);
                    let merged = match t.get(key.as_str())?.and_then(|g| dec_freq(g.value())) {
                        Some(old) => (old.count.max(r.count), old.last_used.max(r.last_used)),
                        None => (r.count, r.last_used),
                    };
                    t.insert(key.as_str(), enc_freq(merged.0, merged.1).as_slice())?;
                }
            }
            txn.commit()?;
            Ok(rows.len())
        })
    }

    /// 删除一条词频（不存在静默成功）。
    pub fn delete_freq(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(FREQ)?;
                t.remove(key.as_str())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 清空某方案全部词频，返回删除条数。
    pub fn clear_freq(&self, schema: &str) -> anyhow::Result<usize> {
        let scan = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(FREQ)?;
                let keys: Vec<String> = {
                    let mut ks = Vec::new();
                    for item in t.range(scan.as_str()..)? {
                        let (k, _) = item?;
                        let key = k.value();
                        if !key.starts_with(&scan) {
                            break;
                        }
                        ks.push(key.to_string());
                    }
                    ks
                };
                n = keys.len();
                for k in keys {
                    t.remove(k.as_str())?;
                }
            }
            txn.commit()?;
            Ok(n)
        })
    }
}

// ───────────────────────── legacy（过渡期，待 coordinator 接通后移除）─────────────────────────

/// 运行时词频跟踪器（文件式，简化模型；coordinator 过渡期使用，新代码请用 Store 的 redb 词频）。
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
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn freq_jsonl_roundtrip_merge_max() {
        let path = tmp("wind_freq_io.redb");
        let s = Store::open(&path).unwrap();
        s.record_freq("wb", "a", "工").unwrap();
        s.record_freq("wb", "a", "工").unwrap(); // count=2
        let text = s.export_freq_jsonl("wb").unwrap();
        assert!(text.contains("\"count\":2"));

        // 导入到已有更高 count 的库：Merge 取 max，不回退
        let path2 = tmp("wind_freq_io2.redb");
        let s2 = Store::open(&path2).unwrap();
        for _ in 0..5 {
            s2.record_freq("wb", "a", "工").unwrap(); // count=5
        }
        let (imported, skipped) = s2.import_freq_jsonl("wb", &text).unwrap();
        assert_eq!((imported, skipped), (1, 0));
        assert_eq!(
            s2.get_freq("wb", "a", "工").unwrap().unwrap().count,
            5,
            "max 合并不回退"
        );

        // 导入到空库：原值落库
        let path3 = tmp("wind_freq_io3.redb");
        let s3 = Store::open(&path3).unwrap();
        s3.import_freq_jsonl("wb", &text).unwrap();
        assert_eq!(s3.get_freq("wb", "a", "工").unwrap().unwrap().count, 2);
        // 坏行跳过
        let (_, sk) = s3.import_freq_jsonl("wb", "not json\n").unwrap();
        assert_eq!(sk, 1);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
        let _ = std::fs::remove_file(&path3);
    }

    /// 批量查询必须与逐个 `get_freq` **逐项等价**，且保持入参顺序。
    ///
    /// 改批量是为了性能（五笔单字母 78+ 候选 = 78 次读事务），但顺序或缺失项对不齐就会让
    /// 词频张冠李戴——调用方是按下标 zip 回候选文本的。
    #[test]
    fn freq_batch_matches_individual_lookups() {
        let path = tmp("wind_freq_batch.redb");
        let s = Store::open(&path).unwrap();
        s.record_freq("wb", "de", "有").unwrap();
        s.record_freq("wb", "def", "有").unwrap();
        s.record_freq("wb", "def", "有").unwrap(); // count=2
        s.record_freq("py", "de", "的").unwrap();

        // 含命中、未命中、跨 schema、重复项，且顺序刻意打乱
        let keys: Vec<(String, String, String)> = [
            ("wb", "def", "有"),
            ("wb", "zzz", "无"),
            ("py", "de", "的"),
            ("wb", "de", "有"),
            ("py", "de", "有"), // 同 code 不同 schema：不得串
        ]
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect();

        let batch = s.get_freq_batch(&keys).unwrap();
        assert_eq!(batch.len(), keys.len(), "必须逐项对齐，缺一不可");
        for (i, (sc, code, text)) in keys.iter().enumerate() {
            let one = s.get_freq(sc, code, text).unwrap();
            assert_eq!(
                batch[i].map(|r| r.count),
                one.map(|r| r.count),
                "第 {i} 项 ({sc},{code},{text}) 批量与逐个结果不一致"
            );
        }
        // 钉住具体值，防止「两边同时错」
        assert_eq!(batch[0].unwrap().count, 2);
        assert!(batch[1].is_none(), "未命中项必须留 None 占位，不能被跳过");
        assert_eq!(batch[2].unwrap().count, 1);
        assert_eq!(batch[3].unwrap().count, 1);
        assert!(batch[4].is_none(), "schema 隔离：py 下没有「有」");

        assert!(
            s.get_freq_batch(&[]).unwrap().is_empty(),
            "空入参不得开事务"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_redb_record_and_get_freq() {
        let path = tmp("wind_freq_redb.redb");
        let s = Store::open(&path).unwrap();
        assert!(s.get_freq("wb", "a", "工").unwrap().is_none());
        s.record_freq("wb", "a", "工").unwrap();
        s.record_freq("wb", "a", "工").unwrap();
        let r = s.get_freq("wb", "a", "工").unwrap().unwrap();
        assert_eq!(r.count, 2);
        assert!(r.last_used > 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_freq_list_delete_clear() {
        let path = tmp("wind_freq_listops.redb");
        let s = Store::open(&path).unwrap();
        s.record_freq("py", "de", "的").unwrap();
        s.record_freq("py", "shi", "是").unwrap();
        s.record_freq("py", "shi", "是").unwrap();
        s.record_freq("wb", "a", "工").unwrap(); // 另一方案隔离

        let (page, total) = s.list_freq_paged("py", "", 0, 50).unwrap();
        assert_eq!(total, 2);
        assert_eq!(page.len(), 2);
        // 前缀过滤
        let (page2, total2) = s.list_freq_paged("py", "sh", 0, 50).unwrap();
        assert_eq!(total2, 1);
        assert_eq!(page2[0].1, "是");
        assert_eq!(page2[0].2.count, 2);
        // 分页
        let (page3, _) = s.list_freq_paged("py", "", 1, 1).unwrap();
        assert_eq!(page3.len(), 1);

        // 删除
        s.delete_freq("py", "de", "的").unwrap();
        assert_eq!(s.list_freq_paged("py", "", 0, 0).unwrap().1, 1);
        // 清空（按方案隔离，wb 不受影响）
        assert_eq!(s.clear_freq("py").unwrap(), 1);
        assert_eq!(s.list_freq_paged("py", "", 0, 0).unwrap().1, 0);
        assert_eq!(s.list_freq_paged("wb", "", 0, 0).unwrap().1, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recency_peak_boosts_fresh_record() {
        let base = FreqProfile {
            base_scale: 100.0,
            half_life_hours: 72.0,
            recency_peak: 0.0,
        };
        let peaked = FreqProfile {
            recency_peak: 500.0,
            ..base
        };
        let now = 1_000_000i64;
        let fresh = FreqRecord {
            count: 1,
            last_used: now,
        };
        let diff = peaked.pinyin_score(&fresh, now) - base.pinyin_score(&fresh, now);
        assert!(
            (diff - 500.0).abs() < 1e-6,
            "刚用过的记录应获得完整峰值加成，got {diff}"
        );

        let stale = FreqRecord {
            count: 1,
            last_used: now - 30 * 24 * 3600,
        };
        let stale_diff = peaked.pinyin_score(&stale, now) - base.pinyin_score(&stale, now);
        assert!(stale_diff < 1.0, "峰值加成应随衰减消退，got {stale_diff}");
    }

    #[test]
    fn test_pinyin_decay_score() {
        let p = FreqProfile::default();
        let now = 1_000_000_000i64;
        let fresh = FreqRecord {
            count: 9,
            last_used: now,
        };
        let old = FreqRecord {
            count: 9,
            last_used: now - 72 * 3600,
        }; // 一个半衰期前
        let s_fresh = p.pinyin_score(&fresh, now);
        let s_old = p.pinyin_score(&old, now);
        assert!(s_fresh > 0.0);
        // 一个半衰期 → 约半衰
        assert!((s_old / s_fresh - 0.5).abs() < 0.05, "半衰期处应≈半衰");
        assert_eq!(
            p.pinyin_score(
                &FreqRecord {
                    count: 0,
                    last_used: now
                },
                now
            ),
            0.0
        );
    }

    #[test]
    fn test_freq_tracker_save_load_roundtrip() {
        let tmp = tmp("wind_freq_legacy.tsv");
        let a = FreqTracker::new();
        a.record_selection("你好");
        a.record_selection("你好");
        a.save_to_file(&tmp).unwrap();
        let b = FreqTracker::new();
        b.load_from_file(&tmp).unwrap();
        assert_eq!(b.get_count("你好"), 2);
        assert!(b.get_boost("你好") > 0.0);
        let _ = std::fs::remove_file(&tmp);
    }
}
