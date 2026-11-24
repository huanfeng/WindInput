//! 临时词存储（redb）—— 自动学习的临时词库
//!
//! 与 Go 版本 `wind_input/internal/store/temp_words.go` 对齐。复用 user_words 的 key/value 编码。
//! 权重上限 `TEMP_WORD_MAX_WEIGHT=10000`。淘汰（evict）在单写事务内完成（修 Go 的 view→update
//! TOCTOU，store.md §7.4）。晋升条件（count≥promote_count）由调用方（dict StoreTempLayer）判定。
//!
//! 词频重构后 count 与权重解耦：count 只用于晋升判定（不再随复选驱动权重增长），
//! 临时词权重固定为写入时的初值；晋升进用户词库时统一取 `PROMOTED_WEIGHT`（与已有
//! 用户词取 max，不覆盖手动加词的更高权重）。

use crate::store::Store;
use crate::user_words::UserWordRecord;
use crate::wdict;

/// 临时词权重硬上限（仅约束写入时的初值，权重不再随复选累加）
pub const TEMP_WORD_MAX_WEIGHT: i32 = 10000;

/// 自动学习词晋升入用户词库时的统一权重（与已存在的用户词权重取 max，不覆盖手动加词）
pub const PROMOTED_WEIGHT: i32 = 1000;

/// 引擎临时词 → 门面记录（临时词库本就没有入库序号，order 恒 0）。
fn rec_of(w: wind_userdata::runtime::TempWord) -> UserWordRecord {
    UserWordRecord {
        code: w.code,
        text: w.text,
        weight: w.weight,
        count: w.count,
        created_at: w.created_at,
        boundary: w.boundary,
        order: 0,
    }
}

impl Store {
    /// 学习临时词：新词 weight=min(add_weight,MAX)/count=1；已存在只 count++，权重不变
    /// （count 只用于晋升判定，不再驱动权重增长）。返回新的 count（调用方据此与
    /// promote_count 比较决定是否晋升）。
    ///
    /// `boundary`：该 code 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`），
    /// 0=无信息。已存在的记录沿用旧 boundary：同 (schema,code,text) 的切分是确定的，
    /// 不因再次学习而变；且旧值可能来自更可靠的来源。
    pub fn learn_temp_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        add_weight: i32,
        boundary: u64,
    ) -> anyhow::Result<u32> {
        let (count, is_new) = self
            .runtime()
            .learn_temp_detailed(schema, code, text, add_weight, boundary)?;
        // 新词是结构变化；已有词只是 count +1（边界补齐不进联想索引）。
        if is_new {
            self.bump_words_gen(schema);
        } else {
            self.bump_words_count_gen();
        }
        self.after_write();
        Ok(count)
    }

    /// 点查临时词当前累积 count（无记录返回 None）。供协调器选词路径判断晋升。
    pub fn get_temp_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<Option<u32>> {
        Ok(self
            .runtime()
            .temp_word(schema, code, text)
            .map(|w| w.count))
    }

    /// 仅当已存在时计数 +1（不创建新条目、权重不变）。返回 (是否存在, 新count)。
    pub fn increment_temp_if_exists(
        &self,
        schema: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<(bool, u32)> {
        let r = self
            .runtime()
            .increment_temp_if_exists(schema, code, text)?;
        if r.0 {
            self.bump_words_count_gen();
            self.after_write();
        }
        Ok(r)
    }

    /// 精确取某 code 下的所有临时词
    pub fn get_temp_words(&self, schema: &str, code: &str) -> anyhow::Result<Vec<UserWordRecord>> {
        Ok(self
            .runtime()
            .temp_words(schema, code)
            .into_iter()
            .map(rec_of)
            .collect())
    }

    /// 流式遍历某方案下以 `prefix` 开头的临时词。回调返回 `false` 即停。
    ///
    /// 与 [`Store::for_each_user_word`] 同构：`code` / `text` 直接借 redb 的页，不物化。
    ///
    /// 它是本模块「哪些记录算数」的唯一判据，[`Self::count_temp_words`] 与
    /// [`Self::search_temp_words_prefix`] 都走它。别再各抄一份过滤条件：好记录上的测试
    /// 测不出坏记录上的分叉。
    pub fn for_each_temp_word(
        &self,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(crate::user_words::UserWordView<'_>) -> bool,
    ) -> anyhow::Result<()> {
        self.runtime().for_each_temp(schema, prefix, &mut |w| {
            f(crate::user_words::UserWordView {
                code: &w.code,
                text: &w.text,
                weight: w.weight,
                count: w.count,
                created_at: w.created_at,
                boundary: w.boundary,
                order: 0,
            })
        });
        Ok(())
    }

    /// 某方案下的临时词条数。只数不抄。
    ///
    /// 口径与 [`Self::search_temp_words_prefix`] 一致由结构保证（都走 [`Self::for_each_temp_word`]）。
    /// 它省的是 Rust 侧的物化峰值，不省 redb 读缓存，见 [`Store::drop_page_cache`]。
    pub fn count_temp_words(&self, schema: &str) -> anyhow::Result<usize> {
        Ok(self.runtime().count_temp(schema))
    }

    /// 前缀检索临时词（跨 code）。limit<=0 不限。
    pub fn search_temp_words_prefix(
        &self,
        schema: &str,
        prefix: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<UserWordRecord>> {
        let mut out = Vec::new();
        self.runtime().for_each_temp(schema, prefix, &mut |w| {
            out.push(rec_of(w.clone()));
            limit == 0 || out.len() < limit
        });
        Ok(out)
    }

    /// 淘汰：保留 max_keep 条，删除其余（按 `(count, created_at, weight)` 升序淘汰）。
    /// 单写事务完成（先在事务内收集快照再删除，无 TOCTOU）。返回淘汰条数。
    ///
    /// 排序键必须以 `count` 打头：自动造词的 weight 恒为 `LEARN_ADD_WEIGHT`，按 weight 排会
    /// 整库并列、退化成 key 字典序。`count` 才是使用计数（造词记 1，每次选中再加 1）。
    ///
    /// 次键取 `created_at` 而非 FREQ 表的 `last_used`：淘汰主力是 `count == 1` 那批，它们在
    /// FREQ 里没有记录，跨表读会重新并列。
    pub fn evict_temp_words(&self, schema: &str, max_keep: usize) -> anyhow::Result<usize> {
        let n = self.runtime().evict_temp(schema, max_keep)?;
        // 没删到就不动代次：代次一变下游就整表重建索引（见测试
        // evict_without_deletion_keeps_words_generation）。
        if n > 0 {
            self.bump_words_gen(schema);
            self.after_write();
        }
        Ok(n)
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
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
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

    /// 从 wdict `WordIo` 行导入临时词，保留 count 晋升进度（Merge：weight 取 max、count 取 max；
    /// 权重仍受 `TEMP_WORD_MAX_WEIGHT` 约束）。返回导入条数。
    /// 与 `import_temp_words_wdict`（走 learn_temp_word，count 归 1）不同，本方法用于多段词库导入以保真 count。
    pub fn import_temp_word_rows(
        &self,
        schema: &str,
        rows: &[wdict::WordIo],
    ) -> anyhow::Result<usize> {
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
        let rows: Vec<wind_userdata::runtime::TempWord> = rows
            .iter()
            .map(|r| {
                // code 列可能是带空格的音节码 → 拆成扁平码 + 边界。
                // `WordIo::boundary` 优先于空格载体：导入闸口的求解结果走这个字段送进来，
                // 而空格表达不了单音节（`xian` 的 0b1 join 后无空格，split 回来是 0）。
                let (code, spaced_b) = wdict::split_spaced_code(&r.code);
                wind_userdata::runtime::TempWord {
                    code,
                    text: r.text.clone(),
                    weight: r.weight,
                    count: r.count,
                    created_at: 0,
                    boundary: r.boundary.unwrap_or(spaced_b),
                }
            })
            .collect();
        let n = self.runtime().import_temp_rows(schema, &rows)?;
        self.bump_words_gen(schema);
        Ok(n)
    }

    /// 清空某方案全部临时词（单写事务），返回删除条数。
    pub fn clear_temp_words(&self, schema: &str) -> anyhow::Result<usize> {
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
        let n = self.runtime().clear_temp(schema)?;
        self.bump_words_gen(schema);
        Ok(n)
    }

    /// 删除临时词（不存在静默成功）。
    pub fn remove_temp_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
        self.runtime().remove_temp(schema, code, text)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(())
    }

    /// 一批 [`Self::remove_temp_word`]（不存在的跳过）：引擎一次 `apply_batch`。
    pub fn remove_temp_words(&self, schema: &str, keys: &[(String, String)]) -> anyhow::Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
        self.runtime().remove_temps(schema, keys)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(())
    }

    /// 一批 [`Self::promote_temp_word`]：写用户词与删临时词在同一次 `apply_batch` 里（用户词
    /// 在前），不存在的跳过。返回转正了的 `(code, text)`。
    pub fn promote_temp_words(
        &self,
        schema: &str,
        keys: &[(String, String)],
    ) -> anyhow::Result<Vec<(String, String)>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
        self.ensure_user_text(schema);
        let promoted = self
            .runtime()
            .promote_temps(schema, keys, PROMOTED_WEIGHT)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(promoted)
    }

    /// 晋升：把临时词移入用户词库，并从临时库删除。返回是否发生晋升。
    /// 权重统一取 `PROMOTED_WEIGHT`（与已存在的用户词权重取 max，不覆盖手动加词的更高权重，
    /// 不再沿用临时词自身权重）；count=temp+user，created_at 优先保留 user 旧值。
    pub fn promote_temp_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<bool> {
        self.check_learned_not_held(wind_userdata::record::Target::Temp, schema)?;
        // 先写用户词、再删临时词：崩溃在两步之间两边各有一份，用户层去重兜得住；反过来会丢词。
        // `add_user_word` 本身就是「已存在则权重取 max、0 边界补齐」，与旧事务里的合并一致；
        // count 相加不再保留（用户词 P2 起没有 count）。
        let Some(tw) = self.runtime().temp_word(schema, code, text) else {
            return Ok(false);
        };
        self.add_user_word(schema, code, text, PROMOTED_WEIGHT, tw.boundary)?;
        self.runtime().remove_temp(schema, code, text)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(true)
    }
}

#[cfg(test)]
#[path = "temp_words_tests.rs"]
mod tests;
