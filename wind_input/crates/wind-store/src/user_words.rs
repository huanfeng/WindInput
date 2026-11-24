//! 用户词存储（redb）
//!
//! 与 Go 版本 `wind_input/internal/store/user_words.go` 对齐，但：
//! - value 用定长 28 字节（weight i32 + count u32 + created_at i64 + boundary u64 + order u32），
//!   text/code 存于 key，比 Go 的 JSON 紧凑（store.md §7.3）。历经 16B→24B→28B 三版，
//!   一路惰性升级免 migration，见 `dec_val_ordered`。
//! - created_at 统一为 i64 unix 秒（修 Go user=秒/temp=毫秒 不一致，store.md §7.2）。
//!
//! key 编码：`"{schema}\0{code}\0{text}"`（store.md §2）。

use crate::store::Store;
use crate::wdict;
use serde::{Deserialize, Serialize};
use wind_userdata::runtime::UserWord;

/// 遍历期的**借用视图**：`code` / `text` 直接借引擎条目里的字符串，不分配。
///
/// 只在 [`Store::for_each_user_word`] 的回调里活着；要留下来就 [`Self::to_record`]。
#[derive(Debug, Clone, Copy)]
pub struct UserWordView<'a> {
    pub code: &'a str,
    pub text: &'a str,
    pub weight: i32,
    pub count: u32,
    pub created_at: i64,
    pub boundary: u64,
    pub order: u32,
}

impl UserWordView<'_> {
    /// 抄成拥有所有权的记录。**只对真正要留下的那几条调**——这两次 `to_string`
    /// 正是流式遍历要省掉的东西。
    pub fn to_record(self) -> UserWordRecord {
        UserWordRecord {
            code: self.code.to_string(),
            text: self.text.to_string(),
            weight: self.weight,
            count: self.count,
            created_at: self.created_at,
            boundary: self.boundary,
            order: self.order,
        }
    }
}

/// 用户词记录（code/text 来自 key，weight/count/created_at/boundary 来自定长 value）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserWordRecord {
    pub code: String,
    pub text: String,
    pub weight: i32,
    pub count: u32,
    /// 创建时间（unix 秒）
    pub created_at: i64,
    /// `code` 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`）；0=无信息。
    /// 造词路径（generate_word_pinyin）算得；手输码/wdict 文本导入无从得知，为 0。
    /// `serde(default)`：v1 记录与旧客户端 JSON 无此字段，按 0 处理。
    #[serde(default)]
    pub boundary: u64,
    /// 入库先后序号（见同模块的 `enc_val_ordered`，`pub(crate)` 故不做 intra-doc 链接）。
    /// 同码等权时按它升序排，用来保住
    /// 「导入文件里的词条先后」——对应 dict 侧二进制格式里的 `order` 字段（t80）。
    ///
    /// **0 = 无序号**：v1/v2 遗留记录，以及临时词库那些本就没有入库序号的来源。
    ///
    /// ⚠️ 0 是**最小值**，而 `better()` 按 `natural_order` **升序**排
    /// （`wind-candidate/src/candidate.rs:873`）—— 所以无序号者排在**最前**，有序号的新词
    /// 接在它们后面。这是刻意保留的现状：老库记录全是 0、彼此打平，次序一如既往。
    /// （真正做 order 排序的是 `collect_user_word_rows` 与 `better()`；
    /// `search_user_words_prefix` 不排序，它按 redb 的 key 字典序返回。）
    /// `serde(default)`：v2 记录与旧客户端 JSON 无此字段，按 0 处理。
    #[serde(default)]
    pub order: u32,
}

/// 批量导入的分类计数(P2:added=新键 / updated=权重严格更大 / unchanged=权重≤现有不落盘)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WordsImportCounts {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// key: "{schema}\0{code}\0{text}"
pub(crate) fn enc_key(schema: &str, code: &str, text: &str) -> String {
    format!("{schema}\u{0}{code}\u{0}{text}")
}

/// 拆分 key → (schema, code, text)
pub(crate) fn split_key(key: &str) -> Option<(&str, &str, &str)> {
    let mut it = key.splitn(3, '\u{0}');
    Some((it.next()?, it.next()?, it.next()?))
}

/// value: 定长 24 字节 —— `weight i32 | count u32 | created_at i64 | boundary u64`
///
/// v1 为 16 字节（无 boundary），惰性升级：`dec_val` 按实际长度取值，旧记录读出 boundary=0
/// （消费方降级回 DAG），下次写入补齐。
///
/// 写 `USER_WORDS` 表的路径一律不得用它：24B 记录不含 `order`，会把入库序号抹成「无」，
/// 该词跳到同 code 所有词之前。本函数只给 `TEMP_WORDS` 用。注意 `temp_words.rs::promote_temp_word`
/// 写的也是 `USER_WORDS`，别按文件名去数写入点。
pub(crate) fn enc_val(weight: i32, count: u32, created_at: i64, boundary: u64) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..4].copy_from_slice(&weight.to_le_bytes());
    b[4..8].copy_from_slice(&count.to_le_bytes());
    b[8..16].copy_from_slice(&created_at.to_le_bytes());
    b[16..24].copy_from_slice(&boundary.to_le_bytes());
    b
}

/// value(v3): 定长 28 字节 —— `weight i32 | count u32 | created_at i64 | boundary u64 | order u32`
///
/// `order` 是入库先后序号，由 [`take_word_orders`] 统一发号。同 code 词条等权时按它升序排，
/// 保留导入文件里的先后（与 dict 侧二进制格式的 `order` 对齐）；没有它，`better()` 会退化成
/// `code → text` 字典序，迁来的词序就丢了。
///
/// v1=16B → v2=24B → v3=28B，[`dec_val_ordered`] 按实际长度取值，缺的字段取 0，免 migration。
/// `order = 0`（旧记录）不做特殊处理，原样参与升序比较：旧记录彼此保持现状，新词接在后面。
/// 让旧记录沉到队尾同样自洽，但会挪动老用户的既有词序，故不取。
pub(crate) fn enc_val_ordered(
    weight: i32,
    count: u32,
    created_at: i64,
    boundary: u64,
    order: u32,
) -> [u8; 28] {
    let mut b = [0u8; 28];
    b[0..4].copy_from_slice(&weight.to_le_bytes());
    b[4..8].copy_from_slice(&count.to_le_bytes());
    b[8..16].copy_from_slice(&created_at.to_le_bytes());
    b[16..24].copy_from_slice(&boundary.to_le_bytes());
    b[24..28].copy_from_slice(&order.to_le_bytes());
    b
}

/// 解码 value → (weight, count, created_at, boundary)
///
/// [`dec_val_ordered`] 的薄壳，丢掉 order。给不关心顺序的调用方（`temp_words` /
/// `abbrev_index`）用，免得它们为一个用不上的字段改 14 处解构。
pub(crate) fn dec_val(b: &[u8]) -> Option<(i32, u32, i64, u64)> {
    dec_val_ordered(b).map(|(w, c, ca, bd, _)| (w, c, ca, bd))
}

/// 解码 value → (weight, count, created_at, boundary, order)
///
/// 长度守卫刻意宽松（`< 16` 而非 `!= 28`）：旧记录按长度分支解出能解的字段，直接切
/// `b[16..24]` / `b[24..28]` 会越界。这是惰性升级免 migration 的前提。
pub(crate) fn dec_val_ordered(b: &[u8]) -> Option<(i32, u32, i64, u64, u32)> {
    if b.len() < 16 {
        return None;
    }
    let boundary = if b.len() >= 24 {
        u64::from_le_bytes(b[16..24].try_into().ok()?)
    } else {
        0 // v1 遗留记录：无边界信息
    };
    let order = if b.len() >= 28 {
        u32::from_le_bytes(b[24..28].try_into().ok()?)
    } else {
        0 // v1/v2 遗留记录：无入库序号
    };
    Some((
        i32::from_le_bytes(b[0..4].try_into().ok()?),
        u32::from_le_bytes(b[4..8].try_into().ok()?),
        i64::from_le_bytes(b[8..16].try_into().ok()?),
        boundary,
        order,
    ))
}

/// 未折回进文本的新词 order 为 0：门面上排到最后（与用户词层同向；旧库里 0 表示「无序号的旧记录」，
/// P2 起不再有这种记录——迁移时按旧序写成了文本行号）。
fn order_out(o: u32) -> u32 {
    if o == 0 {
        wind_userdata::layer::UNFOLDED_ORDER
    } else {
        o
    }
}

fn record_of(w: UserWord) -> UserWordRecord {
    UserWordRecord {
        order: order_out(w.order),
        code: w.code,
        text: w.text,
        weight: w.weight,
        count: 0,
        created_at: 0,
        boundary: w.boundary,
    }
}

/// `WordIo` → `UserWord`：码拆成扁平码 + 边界，显式边界优先——空格载体表达不了「单音节」
/// （`xian` 的 0b1 经 join→split 会退化成 0），导入闸口求解出的边界走 `WordIo::boundary`。
fn user_rows(rows: &[wdict::WordIo]) -> Vec<UserWord> {
    rows.iter()
        .map(|r| {
            let (code, spaced_b) = wdict::split_spaced_code(&r.code);
            UserWord {
                code,
                text: r.text.clone(),
                weight: r.weight,
                boundary: r.boundary.unwrap_or(spaced_b),
                order: 0,
            }
        })
        .collect()
}

/// P2 起用户词的真源是 `user/<schema>/user.dict.yaml`（设计 §6）：方法签名不变，方法体委托
/// `wind_userdata::runtime` 的用户词 API。写入先进日志 + delta，空闲 30 秒或显式折回时按行补丁
/// 写回文本；清空 / 导入是用户的显式动作，立即写回。用户词不再有 count / created_at（恒 0）。
impl Store {
    /// 新增/合并用户词：已存在则权重取 max、旧边界为 0 时补上。文本 / 编码长度分别有
    /// `wind_userdata::runtime::USER_TEXT_MAX_CHARS` / `USER_CODE_MAX_CHARS` 兜底上限（超出报「过长」）。
    ///
    /// `boundary`：该 code 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`）。
    /// 造词路径（`generate_word_pinyin`）算得；用户手输码 / wdict 导入无从得知，传 0
    /// （消费方降级回 DAG）。
    pub fn add_user_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        weight: i32,
        boundary: u64,
    ) -> anyhow::Result<()> {
        self.ensure_user_text(schema);
        self.runtime()
            .add_user_word(schema, code, text, weight, boundary)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(())
    }

    /// 流式遍历某方案下以 `prefix` 开头的用户词（按 (code, text) 升序）。回调返回 `false` 即停。
    /// 设置页分页列表只物化要显示的那一页。
    pub fn for_each_user_word(
        &self,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(UserWordView<'_>) -> bool,
    ) -> anyhow::Result<()> {
        self.ensure_user_text(schema);
        self.runtime().for_each_user_word(schema, prefix, &mut |w| {
            f(UserWordView {
                code: &w.code,
                text: &w.text,
                weight: w.weight,
                count: 0,
                created_at: 0,
                boundary: w.boundary,
                order: order_out(w.order),
            })
        });
        Ok(())
    }

    /// 某方案下的用户词条数。口径与 [`Self::for_each_user_word`] 逐条一致。
    pub fn count_user_words(&self, schema: &str) -> anyhow::Result<usize> {
        self.ensure_user_text(schema);
        Ok(self.runtime().count_user_words(schema))
    }

    /// 精确取某 code 下的所有用户词
    pub fn get_user_words(&self, schema: &str, code: &str) -> anyhow::Result<Vec<UserWordRecord>> {
        self.ensure_user_text(schema);
        Ok(self
            .runtime()
            .user_words(schema, code)
            .into_iter()
            .map(record_of)
            .collect())
    }

    /// 前缀检索（跨 code，按 (code, text) 升序）。limit<=0 表示不限。
    pub fn search_user_words_prefix(
        &self,
        schema: &str,
        prefix: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<UserWordRecord>> {
        let mut out = Vec::new();
        self.for_each_user_word(schema, prefix, &mut |w| {
            out.push(w.to_record());
            limit == 0 || out.len() < limit
        })?;
        Ok(out)
    }

    /// 删除用户词（不存在静默成功）
    pub fn remove_user_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        self.ensure_user_text(schema);
        self.runtime().remove_user_word(schema, code, text)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(())
    }

    /// 一批 [`Self::add_user_word`]（`(code, text, weight, boundary)`）：任一条过长整批不写；
    /// 引擎一次 `apply_batch`，代次与维护各推进一次。同批重复的键按逐条加词的结果合并。
    pub fn add_user_words(
        &self,
        schema: &str,
        rows: &[(String, String, i32, u64)],
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.ensure_user_text(schema);
        let rows: Vec<UserWord> = rows
            .iter()
            .map(|(code, text, weight, boundary)| UserWord {
                code: code.clone(),
                text: text.clone(),
                weight: *weight,
                boundary: *boundary,
                order: 0,
            })
            .collect();
        self.runtime().add_user_words(schema, &rows)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(())
    }

    /// 一批 [`Self::remove_user_word`]（`(code, text)`，不存在的跳过）：引擎一次 `apply_batch`。
    pub fn remove_user_words(&self, schema: &str, keys: &[(String, String)]) -> anyhow::Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.ensure_user_text(schema);
        self.runtime().remove_user_words(schema, keys)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(())
    }

    /// 把一段文字从某数据方案的用户数据里抹掉：所有编码下的用户词与临时词、词频，以及以它为
    /// 上文或选中词的联想记录。返回删掉的用户词 + 临时词条数。拼音右键删多字用户词 / 临时词用
    /// （「当作没输入过」，不留屏蔽记录）。
    pub fn purge_word(&self, schema: &str, text: &str) -> anyhow::Result<usize> {
        let mut user = Vec::new();
        self.for_each_user_word(schema, "", &mut |w| {
            if w.text == text {
                user.push(w.code.to_string());
            }
            true
        })?;
        let mut temp = Vec::new();
        self.for_each_temp_word(schema, "", &mut |w| {
            if w.text == text {
                temp.push(w.code.to_string());
            }
            true
        })?;
        let mut freq = Vec::new();
        self.for_each_freq(schema, "", &mut |c, t, _| {
            if t == text {
                freq.push(c.to_string());
            }
            true
        })?;
        let mut assoc = Vec::new();
        self.runtime().for_each_assoc(schema, &mut |ctx, w, _| {
            if ctx == text || w == text {
                assoc.push((ctx.to_string(), w.to_string()));
            }
            true
        });
        for c in &user {
            self.remove_user_word(schema, c, text)?;
        }
        for c in &temp {
            self.remove_temp_word(schema, c, text)?;
        }
        for c in &freq {
            self.delete_freq(schema, c, text)?;
        }
        for (ctx, w) in &assoc {
            self.runtime().delete_assoc(schema, ctx, w)?;
        }
        Ok(user.len() + temp.len())
    }

    /// 更新用户词权重（不存在返回 false，不创建）
    pub fn update_user_word_weight(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        new_weight: i32,
    ) -> anyhow::Result<bool> {
        self.check_words_not_held(schema)?;
        self.ensure_user_text(schema);
        let updated = self
            .runtime()
            .update_user_word_weight(schema, code, text, new_weight)?;
        self.bump_words_gen(schema);
        self.after_write();
        Ok(updated)
    }

    /// 清空某 schema 的全部用户词（文本整文件重写为空表），返回删除条数。
    pub fn clear_user_words(&self, schema: &str) -> anyhow::Result<usize> {
        self.ensure_user_text(schema);
        let n = self.runtime().clear_user_words(schema)?;
        self.bump_words_gen(schema);
        Ok(n)
    }

    /// 批量导入用户词（Merge 语义与 add_user_word 一致）：新键 → added；导入权重更大或补上 0 边界
    /// → updated；否则 → unchanged。导入后立即写回文本；已有词不挪行，新词按导入行序追加。
    /// dry-run 见 preview_import_user_words，两者分类一致。
    pub fn import_user_words(
        &self,
        schema: &str,
        rows: &[wdict::WordIo],
    ) -> anyhow::Result<WordsImportCounts> {
        self.ensure_user_text(schema);
        let c = self.runtime().import_user_rows(schema, &user_rows(rows))?;
        self.bump_words_gen(schema);
        Ok(WordsImportCounts {
            added: c.added,
            updated: c.updated,
            unchanged: c.unchanged,
        })
    }

    /// 导入 dry-run（只读）：分类规则与 import_user_words 完全一致（同批重复键按「本批已合并」
    /// 判定）；samples 取前 5 个会落盘行（added/updated）的 "code text"。
    pub fn preview_import_user_words(
        &self,
        schema: &str,
        rows: &[wdict::WordIo],
    ) -> anyhow::Result<(WordsImportCounts, Vec<String>)> {
        use wind_userdata::runtime::RowEffect;
        self.ensure_user_text(schema);
        let mut c = WordsImportCounts::default();
        let mut samples = Vec::new();
        let effects = self.runtime().classify_user_rows(schema, &user_rows(rows));
        for (r, effect) in rows.iter().zip(effects) {
            match effect {
                RowEffect::Added => c.added += 1,
                RowEffect::Updated => c.updated += 1,
                RowEffect::Unchanged => c.unchanged += 1,
            }
            if effect != RowEffect::Unchanged && samples.len() < 5 {
                samples.push(format!("{} {}", r.code, r.text));
            }
        }
        Ok((c, samples))
    }

    /// 由文本重建该方案的用户词段（手改生效）。变了 ⇒ 推进代次（联想 / 按词查码索引据此重建）。
    /// 失败（文本隔离 / 读不了）只 WARN 返回 false：查询照旧按已有段与未折回操作提供。
    pub fn sync_user_text(&self, schema: &str) -> bool {
        use wind_userdata::record::Target;
        self.ensure_user_text(schema);
        match self.runtime().data().sync_text(Target::User, schema) {
            Ok(true) => {
                self.bump_words_gen(schema);
                true
            }
            Ok(false) => false,
            Err(e) => {
                tracing::warn!("用户词文本 {schema} 同步失败：{e:#}");
                false
            }
        }
    }

    /// 学到的数据的文本被手改 / 换掉 ⇒ 由文本重建段（P5）。变了返回 true 并 bump 代次。
    pub fn sync_learned_text(&self, target: wind_userdata::record::Target, schema: &str) -> bool {
        let ud = self.runtime().data();
        if let Err(e) = ud.bind_by_rule(target, schema) {
            tracing::warn!("数据文件 {schema} 绑定失败：{e:#}");
            return false;
        }
        match ud.sync_text(target, schema) {
            Ok(true) => {
                self.bump_words_gen(schema);
                true
            }
            Ok(false) => false,
            Err(e) => {
                tracing::warn!("数据文件 {schema} 同步失败：{e:#}");
                false
            }
        }
    }

    /// 发现（用户目录下新出现的方案目录）+ 同步全部用户文本：各方案的用户词与 Shadow、全局短语。
    /// 返回变了的：方案名，短语变了另含 `"phrases"`（调用方据此重建输入期短语层）。
    /// 触发：配置热重载、`dict.reload` 不带路径（设计 §5）。
    pub fn sync_user_texts(&self) -> Vec<String> {
        let mut changed: Vec<String> = self
            .user_text_schemas()
            .into_iter()
            .filter(|s| self.sync_user_text(s))
            .collect();
        for s in self.sync_shadows() {
            if !changed.contains(&s) {
                changed.push(s);
            }
        }
        for (t, s) in self.learned_text_keys() {
            if self.sync_learned_text(t, &s) && !changed.contains(&s) {
                changed.push(s);
            }
        }
        if self.sync_phrases() {
            changed.push("phrases".into());
        }
        if self.quick_doc().sync() {
            changed.push("quick_format".into());
        }
        if self.common_doc().sync() {
            changed.push("common_chars".into());
        }
        if self.sync_charsets() {
            changed.push("charsets".into());
        }
        changed
    }

    /// `dict.reload` 带路径：只认 `classify_text_path` 列出的那几种——短语、快捷格式、常用字、补全、误读表、
    /// `charsets/*.yaml`、`stats/*.toml`，以及 `<schema>/` 下的用户词、Shadow、学到的数据、用户分库与
    /// `dicts.toml`（按路径分量比对，不碰文件系统；`..` 是 `ParentDir` 分量，必然比对失败）。任一路径不合规 ⇒ 整体 Err、一个都不同步。返回值同 [`Self::sync_user_texts`]。
    pub fn reload_user_text_paths(&self, paths: &[String]) -> anyhow::Result<Vec<String>> {
        let targets = paths
            .iter()
            .map(|p| classify_text_path(self.user_text_dir(), p, cfg!(windows)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut changed = Vec::new();
        for t in targets {
            let name = match t {
                TextPath::Phrases => self.sync_phrases().then(|| "phrases".to_string()),
                TextPath::Quick => self.quick_doc().sync().then(|| "quick_format".to_string()),
                TextPath::Common => self.common_doc().sync().then(|| "common_chars".to_string()),
                TextPath::Charsets(_) => self.sync_charsets().then(|| "charsets".to_string()),
                TextPath::Words(s) => self.sync_user_text(&s).then_some(s),
                TextPath::Shadow(s) => self.shadow_table_synced(&s).then_some(s),
                TextPath::Learned(t, s) => self.sync_learned_text(t, &s).then_some(s),
                TextPath::Completion => self.reload_completions().then(|| "completion".to_string()),
                TextPath::Stats => None,
                // 误读表归引擎：随配置重载重读
                TextPath::Misread => None,
                // 分库不归 store 管内容：交给调用方失效用到该数据方案的方案
                TextPath::UserDicts(s) => Some(format!("{USER_DICTS_CHANGED}{s}")),
            };
            if let Some(n) = name
                && !changed.contains(&n)
            {
                changed.push(n);
            }
        }
        Ok(changed)
    }

    /// `dict.flush`：折回全部待处理操作（文本目标写回文件），再压实日志。
    pub fn flush_user_data(&self) -> anyhow::Result<()> {
        self.runtime().data().compact()?;
        self.flush_small_files()
    }

    /// 导出某方案的全部用户词为 wdict 文本(仅 code/text/weight,不含个人 count/created_at)。
    pub fn export_user_words_wdict(
        &self,
        schema: &str,
        exported_at: &str,
    ) -> anyhow::Result<String> {
        let rows = self.collect_user_word_rows(schema)?;
        Ok(wdict::export_words_wdict(&rows, exported_at))
    }

    /// 收集某方案全部用户词为 wdict WordIo 行(code/text/weight/count)。
    ///
    /// code 列输出带空格的音节码（`ni hao`），让边界随备份往返保留，
    /// 见 [`wdict::join_code_by_boundary`]。
    pub(crate) fn collect_user_word_rows(
        &self,
        schema: &str,
    ) -> anyhow::Result<Vec<wdict::WordIo>> {
        let mut recs = self.search_user_words_prefix(schema, "", 0)?;
        // 按入库序号导出，往返一圈词序不变（搜索给的是 key 字典序）。`sort_by_key` 是稳定
        // 排序，order 相同的旧记录保持相对次序。
        recs.sort_by_key(|r| r.order);
        Ok(recs
            .into_iter()
            .map(|r| wdict::WordIo {
                code: wdict::join_code_by_boundary(&r.code, r.boundary),
                text: r.text,
                weight: r.weight,
                count: r.count,
                boundary: None,
            })
            .collect())
    }

    /// 从 wdict 文本导入用户词到某方案(Merge:max-weight upsert)。
    /// 返回 (imported, skipped)。imported=解析成功的行数(含 unchanged);细分类见 import_user_words。
    pub fn import_user_words_wdict(
        &self,
        schema: &str,
        text: &str,
    ) -> anyhow::Result<(usize, usize)> {
        let (rows, skipped) = wdict::parse_words_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
        self.import_user_words(schema, &rows)?;
        Ok((rows.len(), skipped))
    }

    /// 导出某方案的「用户词 + shadow 规则」为单个 wdict 文本（对齐 Go：一个文件两段）。
    pub fn export_dict_wdict(&self, schema: &str, exported_at: &str) -> anyhow::Result<String> {
        let words = self.collect_user_word_rows(schema)?;
        let shadow = self.export_shadow_actions(schema)?;
        Ok(wdict::export_dict_wdict(&words, &shadow, exported_at))
    }
}

/// `dict.reload` 收到的路径指向哪类用户文本。
#[derive(Clone)]
pub(crate) enum TextPath {
    Phrases,
    Quick,
    Common,
    /// `charsets/<key>.yaml`：带磁盘上实际的 key。
    Charsets(String),
    Words(String),
    Shadow(String),
    /// 学到的数据（P5）：`<schema>/{temp,draft}.dict.yaml`、`{freq,assoc}.table.yaml`。
    Learned(wind_userdata::record::Target, String),
    Completion,
    /// `stats/*.toml`：统计每次按需读文件，重载无需动作。
    Stats,
    /// 用户分库 `<schema>/dicts/<名>.dict.yaml` 与开关 `<schema>/dicts.toml`。
    UserDicts(String),
    /// 用户误读表 [`crate::MISREAD_FILE`]：引擎随配置重载读它，store 不管内容。
    Misread,
}

/// 重载 / 结束编辑的返回里，用户分库变了记作 `dicts:<数据方案>`。
pub const USER_DICTS_CHANGED: &str = "dicts:";

/// 只认 `<base>/` 下的那几种文件。`fold_case`：按不分大小写的文件系统比较（Windows）；
/// 此时方案名取磁盘上实际的目录名，调用方传来的大小写不一定与之相同。
pub(crate) fn classify_text_path(
    base: &std::path::Path,
    p: &str,
    fold_case: bool,
) -> anyhow::Result<TextPath> {
    use std::path::Component;
    let fold = |s: &str| {
        if fold_case {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    };
    let names = |path: &std::path::Path| -> Option<Vec<String>> {
        path.components()
            .map(|c| match c {
                Component::ParentDir | Component::CurDir => None,
                c => Some(fold(&c.as_os_str().to_string_lossy())),
            })
            .collect()
    };
    let outside = || anyhow::anyhow!("路径不在用户文本目录内：{p}");
    let base_names = names(base).ok_or_else(outside)?;
    let all = names(std::path::Path::new(p)).ok_or_else(outside)?;
    let rest = match all.strip_prefix(base_names.as_slice()) {
        Some(rest) => rest,
        None => return Err(outside()),
    };
    let is = |got: &String, want: &str| *got == fold(want);
    match rest {
        [f] if is(f, "phrases.dict.yaml") => Ok(TextPath::Phrases),
        [f] if is(f, "quick_format.toml") => Ok(TextPath::Quick),
        [f] if is(f, "common_chars.yaml") => Ok(TextPath::Common),
        [f] if is(f, "completion.table.yaml") => Ok(TextPath::Completion),
        [f] if is(f, crate::MISREAD_FILE) => Ok(TextPath::Misread),
        [d, f] if is(d, "charsets") && f.ends_with(&fold(".yaml")) => {
            let name = schema_dir_name(&base.join("charsets"), f, fold_case);
            Ok(TextPath::Charsets(
                name[..name.len() - ".yaml".len()].to_string(),
            ))
        }
        [d, f] if is(d, "stats") && f.ends_with(&fold(".toml")) => Ok(TextPath::Stats),
        [d, l, f]
            if is(l, crate::user_dicts::LIBS_DIR)
                && f.strip_suffix(&fold(".dict.yaml"))
                    .is_some_and(|n| crate::user_dicts::validate_name(n).is_ok()) =>
        {
            Ok(TextPath::UserDicts(schema_dir_name(base, d, fold_case)))
        }
        [d, f] => {
            let schema = schema_dir_name(base, d, fold_case);
            if is(f, "user.dict.yaml") {
                Ok(TextPath::Words(schema))
            } else if is(f, "shadow.dict.yaml") {
                Ok(TextPath::Shadow(schema))
            } else if is(f, crate::user_dicts::META_FILE) {
                Ok(TextPath::UserDicts(schema))
            } else if let Some(t) = crate::learned_text::LEARNED_TARGETS.into_iter().find(|t| {
                crate::learned_text::learned_binding(base, *t, "s")
                    .and_then(|b| b.path.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .is_some_and(|n| is(f, &n))
            }) {
                Ok(TextPath::Learned(t, schema))
            } else {
                anyhow::bail!("不是可重载的用户数据文件：{p}")
            }
        }
        _ => Err(outside()),
    }
}

/// 方案目录名。不分大小写时 `given` 已折成小写，取 `base` 下实际的那个目录名；
/// 目录不在（文件还没建过）就用折过的名字。
fn schema_dir_name(base: &std::path::Path, given: &str, fold_case: bool) -> String {
    if !fold_case {
        return given.to_string();
    }
    std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .find(|n| n.to_lowercase() == given)
        .unwrap_or_else(|| given.to_string())
}

#[cfg(test)]
#[path = "user_words_tests.rs"]
mod tests;
