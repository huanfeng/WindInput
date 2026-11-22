//! 临时词存储（redb）—— 自动学习的临时词库
//!
//! 与 Go 版本 `wind_input/internal/store/temp_words.go` 对齐。复用 user_words 的 key/value 编码。
//! 权重上限 `TEMP_WORD_MAX_WEIGHT=10000`。淘汰（evict）在**单写事务**内完成（修 Go 的 view→update
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
    /// **它是本模块「哪些记录算数」的唯一一份判据**——[`Self::count_temp_words`] 与
    /// [`Self::search_temp_words_prefix`] 都走它。此前那两个各抄了一遍过滤条件，于是
    /// 「计数与列表口径一致」只能靠测试去比，而**在全是好记录的库上比两条路相等，测不出
    /// 坏记录上的分叉**（`tests/counts_match_listings.rs` 当时确实漏掉了这个：把计数那份
    /// 的过滤删成无条件 `n += 1`，测试照样绿）。结构上只留一份，分叉就不可能发生。
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
    /// 口径与 [`Self::search_temp_words_prefix`] 一致**是结构保证的**：两者都走
    /// [`Self::for_each_temp_word`]，不存在第二份过滤判据可供分叉。
    ///
    /// ⚠️ 它省的是 Rust 侧的物化峰值（19 万条 × 两个 `String`），**不省 redb 读缓存**：
    /// range 迭代照样把走过的叶子页读进缓存。见 [`Store::drop_page_cache`]。
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

    /// 从 wdict `WordIo` 行导入临时词，**保留 count 晋升进度**（Merge：weight 取 max、count 取 max；
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
                // ★ `WordIo::boundary` 优先于空格载体：导入闸口的求解结果走这个字段送进来，
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

    /// 一批 [`Self::promote_temp_word`]：写用户词与删临时词在**同一次** `apply_batch` 里（用户词
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
            0b1,
            "不给显式边界：拼音文本里单个汉字按单音节读（与用户词文本同一规则，P5）"
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

        // ④ evict_temp_words：淘汰排序键是 (count, created_at, weight)，主键 count。
        // 让"你好"的 count 领先"好呀"一次，淘汰结果就不再依赖两次 learn 是否落在同一秒——
        // 否则 created_at 打平时才退到 weight 比大小，慢 CI（如 macOS 共享 runner）跨秒即可打破这个假设而偶发翻车。
        s.learn_temp_word("py", "nihao", "你好", 0, 0b101).unwrap();
        s.learn_temp_word("py", "haoya", "好呀", 10, 0b1001)
            .unwrap();
        assert_eq!(hit("hy"), vec!["好呀"]);
        s.evict_temp_words("py", 1).unwrap();
        assert!(hit("hy").is_empty(), "evict 应同步删索引");
        assert_eq!(hit("nh"), vec!["你好"], "count 更高者留下，索引也应留下");

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
        // P1 起临时词在运行时引擎：用带时刻的操作直接造（created_at 取操作时刻）。
        use wind_userdata::engine::Op;
        use wind_userdata::record::{OpKind, Payload, Target};
        let mut ops = Vec::new();
        for (code, text, ca) in [("aa_new", "新造", 100i64), ("zz_old", "老词", 50)] {
            for (kind, payload) in [
                (OpKind::Upsert, Payload::Weight(800)),
                (OpKind::Incr, Payload::Count(1)),
            ] {
                let mut op = Op::new(Target::Temp, "wb", kind, code, text, payload, 0);
                op.ts_ms = ca * 1000;
                ops.push(op);
            }
        }
        s.runtime().data().apply_batch(&ops).unwrap();
        assert_eq!(s.evict_temp_words("wb", 1).unwrap(), 1);
        assert!(
            s.get_temp_words("wb", "zz_old").unwrap().is_empty(),
            "created_at 更早的那条该被淘汰"
        );
        assert!(!s.get_temp_words("wb", "aa_new").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// 没删到词就不能动代次：代次一变，`UserTextIndex` / 联想索引就整表重扫用户词。
    /// 协调器每 64 次造词调一次淘汰，绝大多数时候词数远在上限内——空提交照样 bump 的话，
    /// 等于每 64 次造词白白触发一轮全表重建（及其分配器高水位）。
    #[test]
    fn evict_without_deletion_keeps_words_generation() {
        let path = tmp("wind_tw_evict_nogen.redb");
        let s = Store::open(&path).unwrap();
        s.learn_temp_word("wb", "a", "甲", 800, 0).unwrap();
        let before = s.words_generation_of("wb");
        assert_eq!(s.evict_temp_words("wb", 10).unwrap(), 0);
        assert_eq!(s.words_generation_of("wb"), before, "未删词却 bump 了代次");
        assert_eq!(s.evict_temp_words("wb", 0).unwrap(), 1);
        assert_ne!(s.words_generation_of("wb"), before, "删了词必须 bump 代次");
        let _ = std::fs::remove_file(&path);
    }

    /// 晋升落空（临时词已不在）同样不能动代次，理由同上。
    #[test]
    fn promote_missing_temp_word_keeps_words_generation() {
        let path = tmp("wind_tw_promote_nogen.redb");
        let s = Store::open(&path).unwrap();
        let before = s.words_generation_of("wb");
        assert!(!s.promote_temp_word("wb", "zz", "无").unwrap());
        assert_eq!(s.words_generation_of("wb"), before);
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
        // 晋升权重统一取 PROMOTED_WEIGHT（不沿用临时权重）；用户词 P2 起没有 count
        let u = s.get_user_words("wb", "a").unwrap();
        assert_eq!(u[0].weight, PROMOTED_WEIGHT);
        // 已存在更高权重的手动加词：晋升不应下调，取 max
        s.add_user_word("wb", "b", "戈", 1200, 0).unwrap();
        s.learn_temp_word("wb", "b", "戈", 800, 0).unwrap();
        assert!(s.promote_temp_word("wb", "b", "戈").unwrap());
        assert_eq!(s.get_user_words("wb", "b").unwrap()[0].weight, 1200);
        // 不存在的临时词晋升返回 false
        assert!(!s.promote_temp_word("wb", "z", "无").unwrap());
        let _ = std::fs::remove_file(&path);
    }

    /// Review Focus 5：门面委托后代次推进时机与旧实现一致。
    #[test]
    fn temp_writes_bump_generations_like_before() {
        let s = Store::open(tmp("wind_tw_gens.redb")).unwrap();
        let (w0, c0) = (s.words_generation(), s.words_count_generation());
        s.learn_temp_word("py", "a", "甲", 800, 0).unwrap();
        assert!(s.words_generation() > w0, "新词推进结构代次");
        let (w1, c1) = (s.words_generation(), s.words_count_generation());
        assert_eq!(c1, c0);
        s.learn_temp_word("py", "a", "甲", 800, 0).unwrap();
        assert_eq!(s.words_generation(), w1, "count+1 不推进结构代次");
        assert!(s.words_count_generation() > c1);
        s.remove_temp_word("py", "a", "甲").unwrap();
        assert!(s.words_generation() > w1);
    }

    /// §10 退出判据：按键路径（学习 / 点查 / 临时词层查询）不经过 redb Mutex。
    #[test]
    fn temp_hot_path_does_not_touch_redb() {
        let s = std::sync::Arc::new(Store::open(tmp("wind_tw_hot.redb")).unwrap());
        s.pause().unwrap(); // redb 暂停：with_db 一律报错
        s.learn_temp_word("py", "nihao", "你好", 800, 0b101)
            .unwrap();
        assert_eq!(s.get_temp_word("py", "nihao", "你好").unwrap(), Some(1));
        let layer = crate::StoreTempLayer::new(s.clone(), "py");
        use wind_dict::DictLayer;
        assert_eq!(layer.search("nihao", 0).len(), 1);
        assert_eq!(layer.search_abbrev("nh", 0).len(), 1);
    }
}
