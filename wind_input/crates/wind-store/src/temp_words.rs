//! 临时词存储（redb）—— 自动学习的临时词库
//!
//! 与 Go 版本 `wind_input/internal/store/temp_words.go` 对齐。复用 user_words 的 key/value 编码。
//! 权重上限 `TEMP_WORD_MAX_WEIGHT=10000`。淘汰（evict）在**单写事务**内完成（修 Go 的 view→update
//! TOCTOU，store.md §7.4）。晋升条件（count≥promote_count）由调用方（dict StoreTempLayer）判定。
//!
//! 词频重构后 count 与权重解耦：count 只用于晋升判定（不再随复选驱动权重增长），
//! 临时词权重固定为写入时的初值；晋升进用户词库时统一取 `PROMOTED_WEIGHT`（与已有
//! 用户词取 max，不覆盖手动加词的更高权重）。

use crate::abbrev_index;
use crate::store::{Store, TEMP_ABBREV, TEMP_WORDS, USER_ABBREV, USER_WORDS};
use crate::user_words::{
    UserWordRecord, dec_val, dec_val_ordered, enc_key, enc_val, enc_val_ordered, now_secs,
};
use crate::wdict;
use redb::ReadableTable;

/// 临时词权重硬上限（仅约束写入时的初值，权重不再随复选累加）
pub const TEMP_WORD_MAX_WEIGHT: i32 = 10000;

/// 自动学习词晋升入用户词库时的统一权重（与已存在的用户词权重取 max，不覆盖手动加词）
pub const PROMOTED_WEIGHT: i32 = 1000;

impl Store {
    /// 学习临时词：新词 weight=min(add_weight,MAX)/count=1；已存在只 count++，权重不变
    /// （count 只用于晋升判定，不再驱动权重增长）。返回新的 count（调用方据此与
    /// promote_count 比较决定是否晋升）。
    ///
    /// `boundary`：该 code 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`），
    /// 0=无信息。已存在的记录**沿用旧 boundary**——同 (schema,code,text) 的切分是确定的，
    /// 不因再次学习而变；且旧值可能来自更可靠的来源。
    pub fn learn_temp_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> anyhow::Result<u32> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let new_count;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                let existing = t.get(key.as_str())?.and_then(|g| dec_val(g.value()));
                let (w, c, ca, b) = match existing {
                    // 旧记录 boundary 为 0（v1 遗留）时用新算出的补上，否则沿用。
                    Some((ow, oc, oca, ob)) => {
                        (ow, oc + 1, oca, if ob != 0 { ob } else { boundary })
                    }
                    None => (
                        add_weight.min(TEMP_WORD_MAX_WEIGHT),
                        1,
                        now_secs(),
                        boundary,
                    ),
                };
                new_count = c;
                t.insert(key.as_str(), enc_val(w, c, ca, b).as_slice())?;
                // count++ 不影响索引（value 空），但**新增**与**边界补齐**都要落索引。
                let old_b = existing.map(|(_, _, _, ob)| ob);
                abbrev_index::shift(
                    &mut txn.open_table(TEMP_ABBREV)?,
                    schema,
                    code,
                    text,
                    old_b,
                    b,
                )?;
            }
            txn.commit()?;
            Ok(new_count)
        })
    }

    /// 点查临时词当前累积 count（无记录返回 None）。供协调器选词路径判断晋升。
    pub fn get_temp_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<Option<u32>> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(TEMP_WORDS)?;
            Ok(t.get(key.as_str())?
                .and_then(|g| dec_val(g.value()))
                .map(|(_, c, _, _)| c))
        })
    }

    /// 仅当已存在时计数 +1（不创建新条目、权重不变）。返回 (是否存在, 新count)。
    pub fn increment_temp_if_exists(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<(bool, u32)> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let result;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                let existing = t.get(key.as_str())?.and_then(|g| dec_val(g.value()));
                match existing {
                    Some((w, c, ca, b)) => {
                        let nc = c + 1;
                        t.insert(key.as_str(), enc_val(w, nc, ca, b).as_slice())?;
                        result = (true, nc);
                    }
                    None => result = (false, 0),
                }
            }
            txn.commit()?;
            Ok(result)
        })
    }

    /// 精确取某 code 下的所有临时词
    pub fn get_temp_words(&self, schema: &str, code: &str) -> anyhow::Result<Vec<UserWordRecord>> {
        let prefix = format!("{schema}\u{0}{code}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(TEMP_WORDS)?;
            let mut out = Vec::new();
            for item in t.range(prefix.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&prefix) {
                    break;
                }
                let text = &key[prefix.len()..];
                if let Some((w, c, ca, b)) = dec_val(v.value()) {
                    out.push(UserWordRecord {
                        code: code.to_string(),
                        text: text.to_string(),
                        weight: w,
                        count: c,
                        created_at: ca,
                        boundary: b,
                        // 临时词库不参与「导入文件词序」那套排序（它的顺序由造词先后与 count 决定）。
                        order: 0,
                    });
                }
            }
            Ok(out)
        })
    }

    /// 某方案下的临时词条数。只数不抄。
    ///
    /// 口径与 [`Self::search_temp_words_prefix`]`(schema, "", 0).len()` 逐条一致——
    /// 设置页拿它显示条数，与列表对不上就是 bug。守门见
    /// `tests/counts_match_listings.rs`。
    ///
    /// ⚠️ 它省的是 Rust 侧的物化峰值（19 万条 × 两个 `String`），**不省 redb 读缓存**：
    /// range 迭代照样把走过的叶子页读进缓存。见 [`Store::drop_page_cache`]。
    pub fn count_temp_words(&self, schema: &str) -> anyhow::Result<usize> {
        let scan = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(TEMP_WORDS)?;
            let mut n = 0usize;
            for item in t.range(scan.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&scan) {
                    break;
                }
                // 与 `search_temp_words_prefix` 同口径：key 拆不开或 value 解不出的都不算。
                if crate::user_words::split_key(key).is_some() && dec_val(v.value()).is_some() {
                    n += 1;
                }
            }
            Ok(n)
        })
    }

    /// 前缀检索临时词（跨 code）。limit<=0 不限。
    pub fn search_temp_words_prefix(
        &self,
        schema: &str,
        prefix: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<UserWordRecord>> {
        let scan = format!("{schema}\u{0}{prefix}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(TEMP_WORDS)?;
            let mut out = Vec::new();
            for item in t.range(scan.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&scan) {
                    break;
                }
                if let (Some((_, code, text)), Some((w, c, ca, b))) =
                    (crate::user_words::split_key(key), dec_val(v.value()))
                {
                    out.push(UserWordRecord {
                        code: code.to_string(),
                        text: text.to_string(),
                        weight: w,
                        count: c,
                        created_at: ca,
                        boundary: b,
                        // 临时词库不参与「导入文件词序」那套排序（它的顺序由造词先后与 count 决定）。
                        order: 0,
                    });
                }
                if limit > 0 && out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
    }

    /// 淘汰：保留 max_keep 条，删除其余（按 `(count, created_at, weight)` 升序淘汰）。
    /// 单写事务完成（先在事务内收集快照再删除，无 TOCTOU）。返回淘汰条数。
    ///
    /// **排序键必须以 `count` 打头**：自动造词写入的 weight 恒为 `LEARN_ADD_WEIGHT`
    /// （协调器 `coordinator.rs`），只按 weight 排会让整库并列，实际顺序退化成 redb 的
    /// key 字典序 —— 用过 500 次的词与造出来没用过的词被淘汰的概率一样。`count` 是真正的
    /// 使用计数：造词写入记 1，之后每次选中该临时词由 `handle_candidate` 再 `learn_temp_word`
    /// 一次（count++）。
    ///
    /// 次键取 `created_at` 而非 FREQ 表的 `last_used`，**是为了不退化**：淘汰的主力是
    /// `count == 1`（造出来从未被选中）那批，它们在 FREQ 里根本没有记录，跨表读只会回落到
    /// 同一个默认值、重新并列。`created_at` 对每条都是确定值，先淘汰最老的没用过的词。
    pub fn evict_temp_words(&self, schema: &str, max_keep: usize) -> anyhow::Result<usize> {
        let scan = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let mut deleted = 0usize;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                // 1) 事务内收集本方案全部 (key, count, created_at, weight, boundary)
                //    boundary 一并带出：删索引要按**删除前**的边界算分组键（见 abbrev_index::remove）。
                let mut all: Vec<(String, u32, i64, i32, u64)> = Vec::new();
                for item in t.range(scan.as_str()..)? {
                    let (k, v) = item?;
                    let key = k.value();
                    if !key.starts_with(&scan) {
                        break;
                    }
                    let (w, c, ca, b) = dec_val(v.value()).unwrap_or((0, 0, 0, 0));
                    all.push((key.to_string(), c, ca, w, b));
                }
                // 2) 超出 max_keep 则删除排序最靠前的若干条
                if all.len() > max_keep {
                    all.sort_by_key(|(_, c, ca, w, _)| (*c, *ca, *w)); // 升序：最该淘汰的在前
                    let to_delete = all.len() - max_keep;
                    let mut idx = txn.open_table(TEMP_ABBREV)?;
                    for (key, _, _, _, b) in all.iter().take(to_delete) {
                        t.remove(key.as_str())?;
                        if let Some((_, code, text)) = crate::user_words::split_key(key) {
                            abbrev_index::remove(&mut idx, schema, code, text, *b)?;
                        }
                        deleted += 1;
                    }
                }
            }
            txn.commit()?;
            Ok(deleted)
        })
    }

    /// 导出某方案全部临时词为 wdict 文本（code/text/weight；count 属晋升进度不流转）。
    pub fn export_temp_words_wdict(
        &self,
        schema: &str,
        exported_at: &str,
    ) -> anyhow::Result<String> {
        let recs = self.search_temp_words_prefix(schema, "", 0)?;
        // code 列输出带空格的音节码，边界随之流出（同 collect_user_word_rows）。
        let rows: Vec<wdict::WordIo> = recs
            .into_iter()
            .map(|r| wdict::WordIo {
                code: wdict::join_code_by_boundary(&r.code, r.boundary),
                text: r.text,
                weight: r.weight,
                count: 0,
                boundary: None,
            })
            .collect();
        Ok(wdict::export_words_wdict(&rows, exported_at))
    }

    /// 从 wdict 文本导入临时词（Merge：learn_temp_word，已存在 count++）。返回 (imported, skipped)。
    pub fn import_temp_words_wdict(
        &self,
        schema: &str,
        text: &str,
    ) -> anyhow::Result<(usize, usize)> {
        let (rows, skipped) = wdict::parse_words_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
        for r in &rows {
            // code 列可能是带空格的音节码 → 拆成扁平码 + 边界；无空格则 boundary=0。
            // `WordIo::boundary` 优先，理由同 `import_temp_word_rows`。
            let (code, spaced_b) = wdict::split_spaced_code(&r.code);
            self.learn_temp_word(
                schema,
                &code,
                &r.text,
                r.weight,
                r.boundary.unwrap_or(spaced_b),
            )?;
        }
        Ok((rows.len(), skipped))
    }

    /// 从 wdict `WordIo` 行导入临时词，**保留 count 晋升进度**（Merge：weight 取 max、count 取 max；
    /// 权重仍受 `TEMP_WORD_MAX_WEIGHT` 约束）。返回导入条数。
    /// 与 `import_temp_words_wdict`（走 learn_temp_word，count 归 1）不同，本方法用于多段词库导入以保真 count。
    pub fn import_temp_word_rows(
        &self,
        schema: &str,
        rows: &[wdict::WordIo],
    ) -> anyhow::Result<usize> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                let mut idx = txn.open_table(TEMP_ABBREV)?;
                for r in rows {
                    // code 列可能是带空格的音节码 → 拆成扁平 key + 边界。
                    // ★ `WordIo::boundary` 优先于空格载体：导入闸口的求解结果走这个字段送进来，
                    // 而空格表达不了单音节（`xian` 的 0b1 join 后无空格，split 回来是 0）。
                    // 只认空格的话，补齐的边界会被静默丢掉——统计上还照样记成「已补齐」。
                    let (code, spaced_b) = wdict::split_spaced_code(&r.code);
                    let in_b = r.boundary.unwrap_or(spaced_b);
                    let key = enc_key(schema, &code, &r.text);
                    let cap = r.weight.min(TEMP_WORD_MAX_WEIGHT);
                    // 已存在且旧 boundary 非 0 则沿用（切分不因导入而变），为 0 时用导入行补齐。
                    let existing = t.get(key.as_str())?.and_then(|g| dec_val(g.value()));
                    let (w, c, ca, b) = match existing {
                        Some((ow, oc, oca, ob)) => (
                            ow.max(cap),
                            oc.max(r.count),
                            oca,
                            if ob != 0 { ob } else { in_b },
                        ),
                        None => (cap, r.count, now_secs(), in_b),
                    };
                    t.insert(key.as_str(), enc_val(w, c, ca, b).as_slice())?;
                    let old_b = existing.map(|(_, _, _, ob)| ob);
                    abbrev_index::shift(&mut idx, schema, &code, &r.text, old_b, b)?;
                }
            }
            txn.commit()?;
            Ok(rows.len())
        })
    }

    /// 清空某方案全部临时词（单写事务），返回删除条数。
    pub fn clear_temp_words(&self, schema: &str) -> anyhow::Result<usize> {
        let prefix = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                let keys: Vec<String> = {
                    let mut ks = Vec::new();
                    for item in t.range(prefix.as_str()..)? {
                        let (k, _) = item?;
                        let key = k.value();
                        if !key.starts_with(&prefix) {
                            break;
                        }
                        ks.push(key.to_string());
                    }
                    ks
                };
                n = keys.len();
                for k in &keys {
                    t.remove(k.as_str())?;
                }
            }
            abbrev_index::clear_schema(&mut txn.open_table(TEMP_ABBREV)?, schema)?;
            txn.commit()?;
            Ok(n)
        })
    }

    /// 删除临时词（不存在静默成功）。
    pub fn remove_temp_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                // 先读边界才能算出索引键——删主表之后就查不到了，顺序不可调换。
                let b = t
                    .get(key.as_str())?
                    .and_then(|g| dec_val(g.value()))
                    .map(|(_, _, _, b)| b);
                t.remove(key.as_str())?;
                if let Some(b) = b {
                    abbrev_index::remove(&mut txn.open_table(TEMP_ABBREV)?, schema, code, text, b)?;
                }
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 晋升：把临时词移入用户词库，并从临时库删除。返回是否发生晋升。
    /// 权重统一取 `PROMOTED_WEIGHT`（与已存在的用户词权重取 max，不覆盖手动加词的更高权重，
    /// 不再沿用临时词自身权重）；count=temp+user，created_at 优先保留 user 旧值。
    pub fn promote_temp_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<bool> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let promoted;
            {
                let mut temp_t = txn.open_table(TEMP_WORDS)?;
                let temp = temp_t.get(key.as_str())?.and_then(|g| dec_val(g.value()));
                match temp {
                    None => promoted = false,
                    Some((_tw, tc, tca, tb)) => {
                        {
                            let mut user_t = txn.open_table(USER_WORDS)?;
                            // boundary 随词一起晋升：临时词由造词算得（有边界），用户词侧若已有
                            // 非 0 值则沿用（同 code/text 的切分确定，且旧值来源未必更差）。
                            let existing = user_t
                                .get(key.as_str())?
                                .and_then(|g| dec_val_ordered(g.value()));
                            // order：已在用户词表里的沿用原号，新晋升的领一个新号。
                            // 不可用 24B 的 `enc_val` 写这里 —— 那会把导入词条既有的入库序号
                            // 抹成 0，把它顶到该 code 下所有词之前，且「用过一阵子」才显形。
                            let (nw, nc, nca, nb, no) = match existing {
                                Some((uw, uc, uca, ub, uo)) => (
                                    uw.max(PROMOTED_WEIGHT),
                                    tc + uc,
                                    uca,
                                    if ub != 0 { ub } else { tb },
                                    uo,
                                ),
                                None => (
                                    PROMOTED_WEIGHT,
                                    tc,
                                    tca,
                                    tb,
                                    crate::user_words::take_word_orders(&txn, 1)?,
                                ),
                            };
                            user_t.insert(
                                key.as_str(),
                                enc_val_ordered(nw, nc, nca, nb, no).as_slice(),
                            )?;
                            // ⚠️ **本文件里唯一写用户词表的路径**。按文件名去数用户词的写路径
                            // 必漏这一处——晋升住在临时词模块里。漏了它，自动学习晋升上来的词
                            // 就永远进不了简拼索引，且要「用一段时间后」才显形。
                            abbrev_index::shift(
                                &mut txn.open_table(USER_ABBREV)?,
                                schema,
                                code,
                                text,
                                existing.map(|(_, _, _, ub, _)| ub),
                                nb,
                            )?;
                        }
                        temp_t.remove(key.as_str())?;
                        abbrev_index::remove(
                            &mut txn.open_table(TEMP_ABBREV)?,
                            schema,
                            code,
                            text,
                            tb,
                        )?;
                        promoted = true;
                    }
                }
            }
            txn.commit()?;
            Ok(promoted)
        })
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

    /// 临时词导入必须认 `WordIo::boundary`，而不只认 code 里的空格。
    ///
    /// ★★ 样本是**单音节**，这不是随手挑的：`join_code_by_boundary("xian", 0b1)` 产出的
    /// 仍是无空格的 `xian`，`split_spaced_code` 拿回来就是 0 —— 空格载体从原理上表达不了
    /// 单音节边界。只认空格的实现会把导入闸口求解出的边界**静默丢掉**，而闸口那边照样
    /// 把它记成「已补齐」。对照组 `gong` 锁住另一半：不给显式边界时行为逐字节不变。
    #[test]
    fn temp_word_import_honors_explicit_boundary() {
        let path = tmp("wind_tw_explicit_b.redb");
        let s = Store::open(&path).unwrap();
        let row = |code: &str, text: &str, b: Option<u64>| wdict::WordIo {
            code: code.into(),
            text: text.into(),
            weight: 50,
            count: 1,
            boundary: b,
        };
        s.import_temp_word_rows(
            "pinyin",
            &[
                row("xian", "先", Some(0b1)),
                row("gong", "工", None),
                row("nihao", "你好", Some(0b101)),
            ],
        )
        .unwrap();
        let b = |code: &str, text: &str| {
            s.get_temp_words("pinyin", code)
                .unwrap()
                .into_iter()
                .find(|r| r.text == text)
                .unwrap()
                .boundary
        };
        assert_eq!(b("xian", "先"), 0b1, "单音节边界必须存活");
        assert_eq!(
            b("gong", "工"),
            0,
            "不给显式边界 ⇒ 空格载体推不出单音节，退化为 0（旧路径不变）"
        );
        assert_eq!(b("nihao", "你好"), 0b101);
        let _ = std::fs::remove_file(&path);
    }

    /// **临时词的每一条写路径都要维护索引。**
    ///
    /// 主表写了、索引没写 ⇒ 那个词简拼静默召不回。六条路径逐一验证。
    #[test]
    fn every_temp_write_path_maintains_the_index() {
        let p = tmp("wind_temp_abbrev_paths.redb");
        let s = Store::open(&p).unwrap();
        let hit = |ab: &str| -> Vec<String> {
            s.search_temp_words_by_abbrev("py", ab, 0)
                .unwrap()
                .into_iter()
                .map(|r| r.text)
                .collect()
        };

        // ① learn_temp_word
        s.learn_temp_word("py", "nihao", "你好", 500, 0b101)
            .unwrap();
        assert_eq!(hit("nh"), vec!["你好"], "learn 应建索引");

        // ② import_temp_word_rows
        s.import_temp_word_rows(
            "py",
            &[wdict::WordIo {
                code: "xi an ning".into(),
                text: "西安宁".into(),
                weight: 700,
                count: 3,
                boundary: None,
            }],
        )
        .unwrap();
        assert_eq!(hit("xan"), vec!["西安宁"], "import 应建索引");

        // ③ remove_temp_word
        s.remove_temp_word("py", "xianning", "西安宁").unwrap();
        assert!(hit("xan").is_empty(), "remove 应删索引");

        // ④ evict_temp_words（按权重淘汰最低者）
        s.learn_temp_word("py", "haoya", "好呀", 10, 0b1001)
            .unwrap();
        assert_eq!(hit("hy"), vec!["好呀"]);
        s.evict_temp_words("py", 1).unwrap();
        assert!(hit("hy").is_empty(), "evict 应同步删索引");
        assert_eq!(hit("nh"), vec!["你好"], "权重高者留下，索引也应留下");

        // ⑤ promote_temp_word：临时索引删、**用户索引建**
        assert!(s.promote_temp_word("py", "nihao", "你好").unwrap());
        assert!(hit("nh").is_empty(), "晋升后不该还留在临时索引里");
        assert_eq!(
            s.search_user_words_by_abbrev("py", "nh", 0)
                .unwrap()
                .into_iter()
                .map(|r| r.text)
                .collect::<Vec<_>>(),
            vec!["你好"],
            "晋升写的是用户词表，用户索引必须跟着建——这条路径住在 temp_words.rs 里，最易漏"
        );

        // ⑥ clear_temp_words
        s.learn_temp_word("py", "zaijian", "再见", 500, 0b1001)
            .unwrap();
        s.clear_temp_words("py").unwrap();
        assert!(hit("zj").is_empty(), "clear 应清索引");
        let _ = std::fs::remove_file(&p);
    }

    /// 复选只加 count，不该动索引——这正是索引 value 留空的收益。
    #[test]
    fn temp_count_bump_does_not_touch_the_index() {
        let p = tmp("wind_temp_abbrev_count.redb");
        let s = Store::open(&p).unwrap();
        s.learn_temp_word("py", "nihao", "你好", 500, 0b101)
            .unwrap();
        let before = s.abbrev_index_len();
        s.learn_temp_word("py", "nihao", "你好", 500, 0b101)
            .unwrap();
        s.increment_temp_if_exists("py", "nihao", "你好").unwrap();
        assert_eq!(s.abbrev_index_len(), before, "索引条数不该变");
        assert_eq!(
            s.search_temp_words_by_abbrev("py", "nh", 0).unwrap().len(),
            1
        );
        let _ = std::fs::remove_file(&p);
    }

    /// **重开库时自动补建**：老库升级上来索引是空的，若等调用方来判断就会漏
    /// （`Store::open` 有协调器/设置页/备份还原等多个入口），而漏掉的表现是静默失效。
    #[test]
    fn reopening_an_unindexed_db_backfills_automatically() {
        let p = tmp("wind_abbrev_backfill.redb");
        {
            let s = Store::open(&p).unwrap();
            s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
            s.learn_temp_word("py", "haoya", "好呀", 500, 0b1001)
                .unwrap();
            // 模拟老库：主表有数据、索引为空
            s.with_db(|db| {
                let txn = db.begin_write()?;
                for t in [USER_ABBREV, TEMP_ABBREV] {
                    abbrev_index::clear_schema(&mut txn.open_table(t)?, "py")?;
                }
                txn.commit()?;
                Ok(())
            })
            .unwrap();
            assert_eq!(s.abbrev_index_len(), 0, "前提：索引确实被清空了");
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.abbrev_index_len(), 2, "重开时应自动补建");
        assert_eq!(
            s.search_user_words_by_abbrev("py", "nh", 0).unwrap().len(),
            1
        );
        assert_eq!(
            s.search_temp_words_by_abbrev("py", "hy", 0).unwrap().len(),
            1
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 存量数据补建覆盖**两张**索引：只补用户词等于临时词简拼仍然失效。
    #[test]
    fn rebuild_covers_both_tables() {
        let p = tmp("wind_temp_abbrev_rebuild.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
        s.learn_temp_word("py", "haoya", "好呀", 500, 0b1001)
            .unwrap();

        // 模拟老库：主表有数据、两张索引都空
        s.with_db(|db| {
            let txn = db.begin_write()?;
            for t in [USER_ABBREV, TEMP_ABBREV] {
                let mut idx = txn.open_table(t)?;
                abbrev_index::clear_schema(&mut idx, "py")?;
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(s.abbrev_index_len(), 0, "前提：索引确实被清空了");

        assert_eq!(s.rebuild_abbrev_indexes().unwrap(), 2);
        assert_eq!(
            s.search_user_words_by_abbrev("py", "nh", 0).unwrap().len(),
            1
        );
        assert_eq!(
            s.search_temp_words_by_abbrev("py", "hy", 0).unwrap().len(),
            1,
            "临时词表也要补建"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn temp_words_wdict_roundtrip_and_clear() {
        let path = tmp("wind_tw_io.redb");
        let s = Store::open(&path).unwrap();
        s.learn_temp_word("wb", "ab", "临时", 50, 0).unwrap();
        s.learn_temp_word("py", "ni", "你", 10, 0).unwrap();
        let text = s.export_temp_words_wdict("wb", "t").unwrap();
        assert!(text.contains("--- !words"));

        let path2 = tmp("wind_tw_io2.redb");
        let s2 = Store::open(&path2).unwrap();
        let (imported, skipped) = s2.import_temp_words_wdict("wb", &text).unwrap();
        assert_eq!((imported, skipped), (1, 0));
        assert_eq!(s2.get_temp_word("wb", "ab", "临时").unwrap(), Some(1));

        assert_eq!(s.clear_temp_words("wb").unwrap(), 1);
        assert!(s.search_temp_words_prefix("wb", "", 0).unwrap().is_empty());
        assert_eq!(
            s.search_temp_words_prefix("py", "", 0).unwrap().len(),
            1,
            "其它 schema 不受影响"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn learn_count_reaches_threshold_then_promote() {
        let path = tmp("wind_tw_promote_thresh.redb");
        let s = Store::open(&path).unwrap();
        for i in 1..=3u32 {
            let n = s.learn_temp_word("wubi86", "abcd", "测试", 100, 0).unwrap();
            assert_eq!(n, i);
        }
        assert_eq!(
            s.get_temp_word("wubi86", "abcd", "测试").unwrap(),
            Some(3),
            "3 次学习后 count 应为 3"
        );
        assert!(s.promote_temp_word("wubi86", "abcd", "测试").unwrap());
        assert_eq!(
            s.get_temp_word("wubi86", "abcd", "测试").unwrap(),
            None,
            "晋升后临时层应删除"
        );
        // 不存在的词返回 None
        assert_eq!(s.get_temp_word("wubi86", "xxxx", "无").unwrap(), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_learn_count_only_weight_unchanged() {
        let path = tmp("wind_tw_learn.redb");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.learn_temp_word("wb", "a", "工", 800, 0).unwrap(), 1);
        assert_eq!(s.learn_temp_word("wb", "a", "工", 800, 0).unwrap(), 2);
        let r = s.get_temp_words("wb", "a").unwrap();
        assert_eq!(r[0].count, 2);
        assert_eq!(r[0].weight, 800, "权重不再随复选累加，保持写入初值");
        // 初值上限
        let _ = s.learn_temp_word("wb", "b", "戈", 99999, 0).unwrap();
        assert_eq!(
            s.get_temp_words("wb", "b").unwrap()[0].weight,
            TEMP_WORD_MAX_WEIGHT
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_increment_if_exists() {
        let path = tmp("wind_tw_inc.redb");
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.increment_temp_if_exists("wb", "a", "工").unwrap(),
            (false, 0)
        );
        s.learn_temp_word("wb", "a", "工", 100, 0).unwrap();
        assert_eq!(
            s.increment_temp_if_exists("wb", "a", "工").unwrap(),
            (true, 2)
        );
        assert_eq!(
            s.get_temp_words("wb", "a").unwrap()[0].weight,
            100,
            "计数不再驱动权重变化"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// **用过的词不能和没用过的词一起被随机淘汰。**
    ///
    /// 自动造词写入的 weight 恒为 `LEARN_ADD_WEIGHT`，真实库里整片并列；只按 weight 排时
    /// 实际顺序退化成 redb 的 key 字典序。此处三条词权重故意全同、且**字典序与使用次数相反**
    /// （用得最多的 code 是 "a"，排字典序第一个）—— 排序键一旦退回纯 weight，被删的头两条
    /// 就是 a/b，用了 3 次的「常用」当场消失。
    #[test]
    fn evict_keeps_the_used_word_when_weights_are_all_equal() {
        let path = tmp("wind_tw_evict_count.redb");
        let s = Store::open(&path).unwrap();
        for _ in 0..3 {
            s.learn_temp_word("wb", "a", "常用", 800, 0).unwrap(); // count=3
        }
        s.learn_temp_word("wb", "b", "没用过一", 800, 0).unwrap(); // count=1
        s.learn_temp_word("wb", "c", "没用过二", 800, 0).unwrap(); // count=1
        assert_eq!(s.evict_temp_words("wb", 1).unwrap(), 2);
        assert!(
            !s.get_temp_words("wb", "a").unwrap().is_empty(),
            "count=3 的词必须留下"
        );
        assert!(s.get_temp_words("wb", "b").unwrap().is_empty());
        assert!(s.get_temp_words("wb", "c").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// count 与 weight 都相同时按 `created_at` 升序淘汰（先造的先走）。
    ///
    /// 这一维覆盖的正是淘汰的主力人群——自动造词产出的 `count=1` + `weight=800` 那批，
    /// 它们在前两个键上全等，只剩 `created_at` 能定序。`learn_temp_word` 取的是当下秒数、
    /// 且对已存在记录沿用旧值，故此处直接写表构造隔秒的两条。
    #[test]
    fn evict_falls_back_to_created_at_when_count_and_weight_tie() {
        let path = tmp("wind_tw_evict_ca.redb");
        let s = Store::open(&path).unwrap();
        // 该被淘汰的是 created_at 更早的 zz_old，而它的 key 字典序排在后面：排序键一旦
        // 漏掉 created_at，稳定排序会保持 range 的字典序，删掉的就变成 aa_new。
        s.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(TEMP_WORDS)?;
                for (code, text, ca) in [("aa_new", "新造", 100i64), ("zz_old", "老词", 50)] {
                    let key = enc_key("wb", code, text);
                    t.insert(key.as_str(), enc_val(800, 1, ca, 0).as_slice())?;
                }
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(s.evict_temp_words("wb", 1).unwrap(), 1);
        assert!(
            s.get_temp_words("wb", "zz_old").unwrap().is_empty(),
            "created_at 更早的那条该被淘汰"
        );
        assert!(!s.get_temp_words("wb", "aa_new").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_evict_lowest_weight() {
        let path = tmp("wind_tw_evict.redb");
        let s = Store::open(&path).unwrap();
        s.learn_temp_word("wb", "a", "低", 10, 0).unwrap();
        s.learn_temp_word("wb", "b", "中", 50, 0).unwrap();
        s.learn_temp_word("wb", "c", "高", 90, 0).unwrap();
        // 保留 2 → 删除权重最低的 1 条（"低"）
        assert_eq!(s.evict_temp_words("wb", 2).unwrap(), 1);
        assert!(s.get_temp_words("wb", "a").unwrap().is_empty());
        assert!(!s.get_temp_words("wb", "c").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_promote_uses_fixed_weight() {
        let path = tmp("wind_tw_promote.redb");
        let s = Store::open(&path).unwrap();
        s.learn_temp_word("wb", "a", "工", 800, 0).unwrap();
        s.learn_temp_word("wb", "a", "工", 800, 0).unwrap(); // count=2
        assert!(s.promote_temp_word("wb", "a", "工").unwrap());
        // 临时库已删
        assert!(s.get_temp_words("wb", "a").unwrap().is_empty());
        // 晋升权重统一取 PROMOTED_WEIGHT（不沿用临时权重），count 累计保留
        let u = s.get_user_words("wb", "a").unwrap();
        assert_eq!(u[0].weight, PROMOTED_WEIGHT);
        assert_eq!(u[0].count, 2);
        // 已存在更高权重的手动加词：晋升不应下调，取 max
        s.add_user_word("wb", "b", "戈", 1200, 0).unwrap();
        s.learn_temp_word("wb", "b", "戈", 800, 0).unwrap();
        assert!(s.promote_temp_word("wb", "b", "戈").unwrap());
        assert_eq!(s.get_user_words("wb", "b").unwrap()[0].weight, 1200);
        // 不存在的临时词晋升返回 false
        assert!(!s.promote_temp_word("wb", "z", "无").unwrap());
        let _ = std::fs::remove_file(&path);
    }
}
