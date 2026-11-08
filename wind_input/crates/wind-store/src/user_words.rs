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
/// v1 为 16 字节（无 boundary）。**惰性升级、无需 migration**：`dec_val` 按实际长度取值，
/// 旧的 16B 记录读出 boundary=0（无边界信息，消费方降级回 DAG），下次写入时自然补齐为 24B。
///
/// ⚠️ **凡是写 `USER_WORDS` 表的路径一律不得用它**：写出的 24B 记录不含 `order`，会把该
/// 词条既有的入库序号抹成「无」，于是它跳到该 code 下所有词之前 —— 且只在「用过一阵子」
/// 后显形。本函数只给 `TEMP_WORDS`（临时词库本就没有入库序号）用。
///
/// ⚠️ **别按文件名去数写入点**：`temp_words.rs::promote_temp_word` 写的就是 `USER_WORDS`
/// （晋升住在临时词模块里）。本次改动第一版正是漏了它 —— 而那处注释早写着同样的警告，
/// 上一次栽的是简拼索引。
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
/// `order` 是**入库先后序号**，由 [`take_word_orders`] 统一发号。同一个 code 下的多条
/// 词条等权时按它升序排，于是「导入文件里的先后」得以保留。
///
/// # 为什么需要它（t80）
///
/// dict 侧的二进制格式本就带 `order u32`，`RankKey` 拿它做 weight 之后的二级键，所以文本
/// 码表不写权重时，同码词条按**词库出现顺序**出。wdict 这边此前没有任何顺序字段：
/// `record_to_candidate` 造候选时 `natural_order` 取 `Default`（恒 0），全部打平，`better()`
/// 退化到按 `code → text` 字典序 —— 从别的平台迁词库过来，原有词序就这么丢了。
///
/// # 版本沿革与惰性升级
///
/// v1=16B（无 boundary）→ v2=24B → v3=28B（追加 order）。[`dec_val_ordered`] 按**实际长度**
/// 取值，读不到的字段取 0，下次写入自然补齐 —— 与 v1→v2 那次同一条路，不需要 migration。
/// `order = 0` 的语义是「没有序号」（v1/v2 旧库记录）。它**不做特殊处理**，原样进
/// `Candidate::natural_order` 参与升序比较 —— 于是旧记录彼此保持现状（全 0、打平，
/// 由 `better()` 的后续键 `code → text` 决定），新词（order ≥ 1）接在它们后面。
/// 这是刻意选的最小改变：让旧记录改沉到队尾同样自洽，但会平白挪动所有老用户的既有词序。
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
/// 长度守卫刻意宽松（`< 16` 而非 `!= 28`）：v1 的 16B 记录仍能解出前三项，v2 的 24B 记录
/// 再多解出 boundary。直接切 `b[16..24]` / `b[24..28]` 会在旧记录上越界，故必须按长度分支
/// ——这是惰性升级免 migration 的前提，v1→v2 那次就是这么过来的。
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
    /// code 列输出**带空格的音节码**（`ni hao`），边界随之流出——此前导出的是扁平码，
    /// 边界在文本里无处安放，于是「导出→清空→导入」一轮就把 boundary 全清零
    /// （备份还原正是这条路径）。见 [`wdict::join_code_by_boundary`]。
    pub(crate) fn collect_user_word_rows(
        &self,
        schema: &str,
    ) -> anyhow::Result<Vec<wdict::WordIo>> {
        let mut recs = self.search_user_words_prefix(schema, "", 0)?;
        // 按入库序号导出，于是「导出 → 再导入」往返一圈词序不变（t80）——否则
        // `search_user_words_prefix` 给的是 key 字典序，一次往返就把原有词序洗掉了。
        // `sort_by_key` 是**稳定**排序：order 相同的（尤其全为 0 的旧库记录）保持
        // 搜索给出的相对次序，不会因为导出而彼此重排。
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
mod tests {
    use super::*;
    use wind_userdata::runtime::{
        USER_CODE_MAX_CHARS as USER_WORD_CODE_MAX_CHARS,
        USER_TEXT_MAX_CHARS as USER_WORD_TEXT_MAX_CHARS,
    };

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    fn row(code: &str, text: &str) -> wdict::WordIo {
        wdict::WordIo {
            code: code.into(),
            text: text.into(),
            weight: 0,
            count: 0,
            boundary: None,
        }
    }

    fn io(code: &str, text: &str, weight: i32) -> wdict::WordIo {
        wdict::WordIo {
            weight,
            ..row(code, text)
        }
    }

    /// 用户词落进文本：门面写 → 折回 → 文件里有；重开后从文本读回。
    #[test]
    fn user_words_live_in_text() {
        let p = tmp("wind_uw_in_text.redb");
        {
            let s = Store::open(&p).unwrap();
            s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
                .unwrap();
            s.runtime()
                .data()
                .fold(wind_userdata::record::Target::User, "pinyin")
                .unwrap();
            let f = s.user_text_path("pinyin");
            assert!(
                std::fs::read_to_string(&f)
                    .unwrap()
                    .contains("ni hao\t你好\t9")
            );
        }
        let s = Store::open(&p).unwrap();
        let got = s.get_user_words("pinyin", "nihao").unwrap();
        assert_eq!(
            (got[0].text.as_str(), got[0].weight, got[0].boundary),
            ("你好", 9, 0b101)
        );
    }

    /// 未折回的新词列得出、数得对、导出排最后。
    #[test]
    fn unfolded_words_are_listed_counted_and_exported_last() {
        let s = Store::open(tmp("wind_uw_unfolded.redb")).unwrap();
        s.add_user_word("wb", "aa", "式", 5, 0).unwrap();
        s.runtime()
            .data()
            .fold(wind_userdata::record::Target::User, "wb")
            .unwrap();
        s.add_user_word("wb", "aa", "工", 5, 0).unwrap();
        let mut n = 0;
        s.for_each_user_word("wb", "", &mut |_| {
            n += 1;
            true
        })
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(s.count_user_words("wb").unwrap(), 2);
        let rows = s.collect_user_word_rows("wb").unwrap();
        assert_eq!(rows.last().unwrap().text, "工", "未折回的新词导出在末尾");
    }

    /// 预览分类与实际导入一致（含同批重复键、补边界）。
    #[test]
    fn preview_matches_import_counts() {
        let s = Store::open(tmp("wind_uw_preview2.redb")).unwrap();
        s.add_user_word("pinyin", "nihao", "你好", 100, 0).unwrap();
        let rows = vec![
            io("ni hao", "你好", 1),
            io("ta", "他", 3),
            io("ta", "他", 9),
        ];
        let (pc, _) = s.preview_import_user_words("pinyin", &rows).unwrap();
        let ic = s.import_user_words("pinyin", &rows).unwrap();
        assert_eq!(
            (pc.added, pc.updated, pc.unchanged),
            (ic.added, ic.updated, ic.unchanged)
        );
        assert_eq!((ic.added, ic.updated, ic.unchanged), (1, 2, 0));
    }

    /// 超长文本（> `USER_WORD_TEXT_MAX_CHARS`）一律拒绝——覆盖显式给码/设置页手动加词这
    /// 两条不经过 `check_derivable_word`（wind-coordinator）的路径，兜住此前无上限的空档
    /// （用户实测提交过 7500 字词条）。恰好等于上限的文本应正常写入，被拒绝的词不落库
    /// （校验须在 `enc_key`/写事务之前，不留部分写入痕迹）。
    #[test]
    fn add_user_word_rejects_text_over_max_chars() {
        let p = tmp("wind_uw_text_max_len.redb");
        let s = Store::open(&p).unwrap();

        let ok_text: String = "汉".repeat(USER_WORD_TEXT_MAX_CHARS);
        s.add_user_word("pinyin", "abc", &ok_text, 100, 0)
            .expect("恰好等于上限应写入成功");

        let too_long: String = "汉".repeat(USER_WORD_TEXT_MAX_CHARS + 1);
        let err = s
            .add_user_word("pinyin", "abd", &too_long, 100, 0)
            .expect_err("超过上限应被拒绝");
        assert!(
            err.to_string().contains("过长"),
            "错误信息应提示过长: {err}"
        );
        assert!(
            s.get_user_words("pinyin", "abd").unwrap().is_empty(),
            "拒绝的词不得落库"
        );
    }

    /// 超长 code（> `USER_WORD_CODE_MAX_CHARS`）一律拒绝，语义与文本长度校验对称。
    #[test]
    fn add_user_word_rejects_code_over_max_chars() {
        let p = tmp("wind_uw_code_max_len.redb");
        let s = Store::open(&p).unwrap();

        let ok_code: String = "a".repeat(USER_WORD_CODE_MAX_CHARS);
        s.add_user_word("pinyin", &ok_code, "你好", 100, 0)
            .expect("恰好等于上限应写入成功");

        let too_long_code: String = "a".repeat(USER_WORD_CODE_MAX_CHARS + 1);
        let err = s
            .add_user_word("pinyin", &too_long_code, "你好", 100, 0)
            .expect_err("超过上限应被拒绝");
        assert!(
            err.to_string().contains("过长"),
            "错误信息应提示过长: {err}"
        );
        assert!(
            s.get_user_words("pinyin", &too_long_code)
                .unwrap()
                .is_empty(),
            "拒绝的词不得落库"
        );
    }

    /// **同码词条按导入文件的先后出**（t80 的验收标准）。
    ///
    /// 三条词刻意按「甲 乙 丙」导入，而它们的 text 字典序恰好相反（丙 U+4E19 < 乙 U+4E59
    /// < 甲 U+7532）—— 于是「按 order」与「按 text 字典序」两种结果可区分。缺陷期
    /// `natural_order` 恒 0、redb 又按 key 字典序返回，拿到的就是「丙 乙 甲」。
    ///
    /// ⚠️ 光断言顺序不够：order 全为 0 时 `sort_by_key` 是稳定排序，会原样吐回 search 的
    /// 次序，看着也可能"对"。故必须同时断言 order **严格递增且 ≥ 1**。
    ///
    /// 反向验证（2026-09-14 实跑）：删掉 `import_user_words` 里的
    /// `next_order = next_order.saturating_add(1)` —— 所有新词拿同一个号 —— 本测试与
    /// `word_order_survives_export_clear_import` 一起变红。
    ///
    /// ⚠️ 只把 `next_order` 的**初值**改成 0 是**无效变异**：`saturating_add` 照样让它
    /// 递增成 0,1,2，顺序断言仍然通过，红的只是上面那句「order ≥ 1」。实测过，别照这条改。
    #[test]
    fn import_assigns_order_following_row_sequence() {
        let p = tmp("wind_uw_order_import.redb");
        let s = Store::open(&p).unwrap();
        s.import_user_words(
            "pinyin",
            &[row("abc", "甲"), row("abc", "乙"), row("abc", "丙")],
        )
        .unwrap();

        let mut recs = s.get_user_words("pinyin", "abc").unwrap();
        assert_eq!(recs.len(), 3);
        recs.sort_by_key(|r| r.order);
        let texts: Vec<&str> = recs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["甲", "乙", "丙"], "同码词条应按导入行序排");

        let orders: Vec<u32> = recs.iter().map(|r| r.order).collect();
        assert!(
            orders[0] >= 1 && orders[0] < orders[1] && orders[1] < orders[2],
            "order 应自 1 起严格递增（0 会被稳定排序掩盖成假通过），实际: {orders:?}"
        );
    }

    /// 「导出 → 清空 → 再导入」一圈之后词序不变。
    ///
    /// 这是 t80 楼主的真实用法（迁词库 / 备份还原）。导出若按 key 字典序吐出，一次往返
    /// 就把原有词序洗成字典序 —— 与缺陷期表现一模一样，且**更隐蔽**，因为库里明明存着
    /// 正确的 order。
    ///
    /// 反向验证（2026-09-14 实跑）：删掉 `collect_user_word_rows` 里那句按 order 的排序，
    /// 本测试即变红，另外三条不受影响。
    #[test]
    fn word_order_survives_export_clear_import() {
        let p = tmp("wind_uw_order_roundtrip.redb");
        let s = Store::open(&p).unwrap();
        s.import_user_words(
            "pinyin",
            &[row("abc", "甲"), row("abc", "乙"), row("abc", "丙")],
        )
        .unwrap();

        let text = s.export_user_words_wdict("pinyin", "2026-09-14").unwrap();
        s.clear_user_words("pinyin").unwrap();
        s.import_user_words_wdict("pinyin", &text).unwrap();

        let mut recs = s.get_user_words("pinyin", "abc").unwrap();
        recs.sort_by_key(|r| r.order);
        let texts: Vec<&str> = recs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["甲", "乙", "丙"], "一轮备份还原后词序应原样保留");
    }

    /// 再次导入同一批词**不得**把它们挪到队尾。
    ///
    /// 用户重复导入（或备份还原叠加导入）是常态。若更新路径也发新号，每导一次全库词序
    /// 就被翻搅一次，且新号总比老号大 —— 表现为「越导越乱」。
    ///
    /// 反向验证（2026-09-14 实跑）：把更新分支的 `enc_val_ordered(nw, nc, ca, nb, o)` 的
    /// `o` 换成 `next_order`（即更新也发新号），**只有本测试**变红 —— 另外三条都覆盖不到
    /// 这个维度，故这一条不可省。
    #[test]
    fn reimport_keeps_existing_order() {
        let p = tmp("wind_uw_order_reimport.redb");
        let s = Store::open(&p).unwrap();
        let rows = [row("abc", "甲"), row("abc", "乙")];
        s.import_user_words("pinyin", &rows).unwrap();
        let before: Vec<u32> = {
            let mut r = s.get_user_words("pinyin", "abc").unwrap();
            r.sort_by_key(|x| x.text.clone());
            r.iter().map(|x| x.order).collect()
        };

        // 第二次导入：权重抬高以确保真的走了写盘分支（否则 unchanged 不写，测不到东西）。
        let bumped: Vec<wdict::WordIo> = rows
            .iter()
            .map(|r| wdict::WordIo {
                weight: 900,
                ..r.clone()
            })
            .collect();
        s.import_user_words("pinyin", &bumped).unwrap();

        let after: Vec<u32> = {
            let mut r = s.get_user_words("pinyin", "abc").unwrap();
            r.sort_by_key(|x| x.text.clone());
            assert!(
                r.iter().all(|x| x.weight == 900),
                "前提：第二次导入应已写盘"
            );
            r.iter().map(|x| x.order).collect()
        };
        assert_eq!(before, after, "重复导入不得改变既有词条的入库序号");
    }

    /// 晋升临时词**不得**抹掉该词条在用户词表里的入库序号。
    ///
    /// `promote_temp_word` 住在 `temp_words.rs` 里，但它写的是 `USER_WORDS` —— 按文件名去数
    /// 用户词的写入点必漏这一处（该函数的注释早写着这条警告，上一次栽的是简拼索引）。
    /// 漏了它的后果不是「不生效」而是**越用越乱**：导入词条 order=10，被用户用熟晋升一次
    /// 就被 24B 的 `enc_val` 写成 order=0，升序下 0 < 10，该词跳到这个 code 下所有词之前，
    /// 且要「用过一阵子」才显形。
    ///
    /// 反向验证（2026-09-14 实跑）：把 `promote_temp_word` 的写入换回
    /// `enc_val(nw, nc, nca, nb)`，本测试即变红。
    #[test]
    fn promote_temp_word_keeps_user_word_order() {
        let p = tmp("wind_uw_order_promote.redb");
        let s = Store::open(&p).unwrap();
        s.import_user_words("pinyin", &[row("abc", "甲"), row("abc", "乙")])
            .unwrap();
        let pick = |st: &Store| -> UserWordRecord {
            st.get_user_words("pinyin", "abc")
                .unwrap()
                .into_iter()
                .find(|r| r.text == "乙")
                .expect("「乙」应在用户词表里")
        };
        let before = pick(&s).order;
        assert!(before >= 1, "前提：导入应已发号，实际 {before}");

        // 用户词表里已有「乙」→ 晋升走 existing 分支（真实场景：导入的词后来被用熟）。
        s.learn_temp_word("pinyin", "abc", "乙", 800, 0).unwrap();
        assert!(
            s.promote_temp_word("pinyin", "abc", "乙").unwrap(),
            "前提：应晋升成功，否则测的是空路径"
        );

        assert_eq!(
            pick(&s).order,
            before,
            "晋升不得改动既有入库序号（抹成 0 会把该词顶到所有导入词之前）"
        );
    }

    /// 简拼召回的记录必须带**真实** order，否则表现为「全码对、简拼不对」。
    ///
    /// 缩写索引自己不定序，但它交出的记录经 `StoreUserLayer::search_abbrev` →
    /// `record_to_candidate` → `sort_trunc`，而 `sort_trunc` 用的正是 `better()`，
    /// `natural_order` 就在那条排序链上。硬编码 0 会让同一批词走全码时顺序正确、
    /// 走简拼时退回 text 字典序。
    ///
    /// 反向验证（2026-09-14 实跑）：把 `abbrev_index::search` 里的 `order: o` 改回
    /// `order: 0`，本测试即变红。
    #[test]
    fn abbrev_search_carries_real_order() {
        let p = tmp("wind_uw_order_abbrev.redb");
        let s = Store::open(&p).unwrap();
        // 按「拟好 你好」导入；text 字典序恰好相反（你 U+4F60 < 拟 U+62DF），两者可区分。
        s.import_user_words("pinyin", &[row("ni hao", "拟好"), row("ni hao", "你好")])
            .unwrap();

        let mut recs = s.search_user_words_by_abbrev("pinyin", "nh", 0).unwrap();
        assert_eq!(recs.len(), 2, "简拼 nh 应召回两条，实际 {recs:?}");
        recs.sort_by_key(|r| r.order);
        let texts: Vec<&str> = recs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["拟好", "你好"], "简拼召回应保住导入词序");

        let orders: Vec<u32> = recs.iter().map(|r| r.order).collect();
        assert!(
            orders[0] >= 1 && orders[0] < orders[1],
            "order 须为真值而非硬编码 0（全 0 时稳定排序会掩盖成假通过），实际 {orders:?}"
        );
    }

    /// **向后兼容契约**：v2 的 24B 记录（无 order）读出 order=0，且不 panic。
    ///
    /// 与 `dec_val_reads_v1_16byte_records` 同理 —— 长度守卫必须分三档，直接切
    /// `b[24..28]` 会在 v1/v2 记录上越界。
    ///
    /// 反向验证（2026-09-14 实跑）：去掉 `dec_val_ordered` 里 order 那档长度分支，
    /// 本测试与 `dec_val_reads_v1_16byte_records` 一起变红（后者也红是对的：`dec_val`
    /// 现在是 `dec_val_ordered` 的薄壳，两者共用同一条解码路径）。
    #[test]
    fn dec_val_ordered_reads_v1_and_v2_records() {
        let mut v1 = [0u8; 16];
        v1[0..4].copy_from_slice(&123i32.to_le_bytes());
        assert_eq!(
            dec_val_ordered(&v1).map(|t| (t.3, t.4)),
            Some((0, 0)),
            "v1（16B）：boundary 与 order 均取 0"
        );

        let v2 = enc_val(123, 7, 1_700_000_000, 0b101);
        assert_eq!(v2.len(), 24);
        assert_eq!(
            dec_val_ordered(&v2),
            Some((123, 7, 1_700_000_000, 0b101, 0)),
            "v2（24B）：boundary 读回、order 取 0"
        );

        let v3 = enc_val_ordered(123, 7, 1_700_000_000, 0b101, 42);
        assert_eq!(v3.len(), 28);
        assert_eq!(
            dec_val_ordered(&v3),
            Some((123, 7, 1_700_000_000, 0b101, 42))
        );

        // 薄壳与全量版对同一条记录必须给出一致的前四项。
        assert_eq!(dec_val(&v3), Some((123, 7, 1_700_000_000, 0b101)));
        assert_eq!(dec_val_ordered(&[0u8; 15]), None);
    }

    /// **备份还原不得丢音节边界**（本次改动的验收标准）。
    ///
    /// `backup.rs` 的还原路径是 `clear_user_words` + `import_user_words_wdict`。清空后
    /// 全是新键，而 wdict 此前是扁平四列文本、边界无处安放 ⇒ **一轮备份还原把 boundary
    /// 全部清零**（实测：写入 `[5,21]` → 还原后 `[0,0]`）。现 code 列改写带空格的音节码。
    ///
    /// 反向验证：把 `collect_user_word_rows` 的 `join_code_by_boundary` 换回 `r.code`，
    /// 本测试即变红。
    #[test]
    fn boundary_survives_export_clear_import() {
        let p = tmp("wind_uw_boundary_backup.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("pinyin", "nihao", "你好", 500, 0b101)
            .unwrap();
        s.add_user_word("pinyin", "xianning", "西安宁", 800, 0b10101)
            .unwrap();

        let text = s.export_user_words_wdict("pinyin", "2026-07-29").unwrap();
        assert!(
            text.contains("ni hao") && text.contains("xi an ning"),
            "导出文本的 code 列须为带空格的音节码，实际:\n{text}"
        );

        // ── 模拟备份还原 ──
        s.clear_user_words("pinyin").unwrap();
        s.import_user_words_wdict("pinyin", &text).unwrap();

        let recs = s.search_user_words_prefix("pinyin", "", 0).unwrap();
        let mut got: Vec<(String, u64)> =
            recs.iter().map(|r| (r.code.clone(), r.boundary)).collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("nihao".to_string(), 0b101),
                ("xianning".to_string(), 0b10101)
            ],
            "还原后 key 须仍是扁平码、且边界原样存活"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 旧版 wdict（code 列为扁平码，无空格）导入后 boundary=0，不报错、不误判为单音节。
    /// 用户手上已有的备份文件都是这个形态，必须平滑降级。
    #[test]
    fn legacy_flat_wdict_imports_as_unknown_boundary() {
        let p = tmp("wind_uw_legacy_wdict.redb");
        let s = Store::open(&p).unwrap();
        let legacy = "# WindInput 用户数据文件\nwind_dict:\n  version: 1\n  sections:\n    words:\n      columns: [code, text, weight, count]\n\n--- !words\nnihao\t你好\t500\t0\nabcd\t工作\t100\t0\n";
        let (imported, skipped) = s.import_user_words_wdict("pinyin", legacy).unwrap();
        assert_eq!((imported, skipped), (2, 0));
        let recs = s.search_user_words_prefix("pinyin", "", 0).unwrap();
        assert!(
            recs.iter().all(|r| r.boundary == 0),
            "无空格的码一律按「无边界信息」处理"
        );
        assert!(recs.iter().any(|r| r.code == "nihao"));
        let _ = std::fs::remove_file(&p);
    }

    /// 导入行带边界、库中旧记录 boundary=0（v1 遗留或历史扁平导入）→ 补齐而非忽略，
    /// 且该行计入 updated；`preview_import_user_words` 的分类必须给出同样答案。
    #[test]
    fn import_fills_missing_boundary_and_preview_agrees() {
        let p = tmp("wind_uw_fill_boundary.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("pinyin", "nihao", "你好", 500, 0).unwrap(); // 旧记录无边界

        let rows = vec![wdict::WordIo {
            code: "ni hao".into(),
            text: "你好".into(),
            weight: 500, // weight/count 均不变，只有 boundary 可补
            count: 0,
            boundary: None,
        }];
        let (pc, _) = s.preview_import_user_words("pinyin", &rows).unwrap();
        let ic = s.import_user_words("pinyin", &rows).unwrap();
        assert_eq!(
            (pc.added, pc.updated, pc.unchanged),
            (0, 1, 0),
            "dry-run 须把「仅补边界」也算作会落盘的 updated"
        );
        assert_eq!(
            (ic.added, ic.updated, ic.unchanged),
            (0, 1, 0),
            "实际导入的分类须与 dry-run 完全一致"
        );
        assert_eq!(
            s.get_user_words("pinyin", "nihao").unwrap()[0].boundary,
            0b101
        );
        let _ = std::fs::remove_file(&p);
    }

    /// **向后兼容契约**（数据安全）：value 从 v1 的 16B 扩到 24B（追加 boundary u64）。
    /// 旧库里全是 16B 记录，新代码必须能读——且**不可直接切 `b[16..24]`**，那会在旧记录上
    /// 越界 panic，必须按长度分支。这是惰性升级免 migration 的前提。
    #[test]
    fn dec_val_reads_v1_16byte_records() {
        // 手工拼一条 v1（16B）记录，模拟旧库数据。
        let mut v1 = [0u8; 16];
        v1[0..4].copy_from_slice(&123i32.to_le_bytes());
        v1[4..8].copy_from_slice(&7u32.to_le_bytes());
        v1[8..16].copy_from_slice(&1_700_000_000i64.to_le_bytes());
        assert_eq!(
            dec_val(&v1),
            Some((123, 7, 1_700_000_000, 0)),
            "v1 记录须能读出，boundary 取 0（无信息 → 消费方降级回 DAG）"
        );

        // v2（24B）：boundary 原样读回。
        let v2 = enc_val(123, 7, 1_700_000_000, 0b101);
        assert_eq!(v2.len(), 24);
        assert_eq!(dec_val(&v2), Some((123, 7, 1_700_000_000, 0b101)));

        // 短于 16B 视为损坏 → None（而非 panic）。
        assert_eq!(dec_val(&[0u8; 15]), None);
        assert_eq!(dec_val(&[]), None);
    }

    /// 无边界词（手输码/旧版扁平导入）仍须被简拼查询捞出来交给引擎现判，
    /// 且**只在首字符对得上时**才捞——这正是分组的意义。
    #[test]
    fn no_boundary_words_are_recalled_by_first_char_only() {
        let p = tmp("wind_abbrev_noboundary.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("py", "xianning", "西安宁", 100, 0).unwrap();
        s.add_user_word("py", "nihao", "你好", 100, 0).unwrap();

        let texts = |ab: &str| -> Vec<String> {
            s.search_user_words_by_abbrev("py", ab, 0)
                .unwrap()
                .into_iter()
                .map(|r| r.text)
                .collect()
        };
        // 首字符 x → 只捞出 x 开头的那条，n 开头的那条被分组挡掉
        assert_eq!(texts("xan"), vec!["西安宁"]);
        assert_eq!(texts("nh"), vec!["你好"]);
        // 首字符对不上 → 一条不返回（改动前是整库返回）
        assert!(texts("zg").is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn test_add_get_user_word() {
        let path = tmp("wind_uw_addget.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        s.add_user_word("wb", "a", "戈", 50, 0).unwrap();
        let mut got = s.get_user_words("wb", "a").unwrap();
        got.sort_by_key(|r| r.text.clone());
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|r| r.text == "工" && r.weight == 100));
        // add 同词更高权重 → 取 max
        s.add_user_word("wb", "a", "工", 200, 0).unwrap();
        let g = s.get_user_words("wb", "a").unwrap();
        assert_eq!(g.iter().find(|r| r.text == "工").unwrap().weight, 200);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_prefix_remove_update() {
        let path = tmp("wind_uw_prefix.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "ab", "阿", 10, 0).unwrap();
        s.add_user_word("wb", "abc", "啊", 20, 0).unwrap();
        s.add_user_word("wb", "x", "西", 30, 0).unwrap();
        // 前缀 "ab" 命中 ab/abc，不含 x
        let pre = s.search_user_words_prefix("wb", "ab", 0).unwrap();
        assert_eq!(pre.len(), 2);
        assert!(pre.iter().all(|r| r.code.starts_with("ab")));
        // 更新权重
        assert!(s.update_user_word_weight("wb", "ab", "阿", 99).unwrap());
        assert!(!s.update_user_word_weight("wb", "ab", "缺", 1).unwrap());
        assert_eq!(s.get_user_words("wb", "ab").unwrap()[0].weight, 99);
        // 删除
        s.remove_user_word("wb", "ab", "阿").unwrap();
        assert!(s.get_user_words("wb", "ab").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn export_import_user_words_roundtrip() {
        let path = tmp("wind_uw_io.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        s.add_user_word("wb", "ml", "多行\n带\t制表", 5, 0).unwrap();
        let text = s
            .export_user_words_wdict("wb", "2026-07-11T00:00:00+08:00")
            .unwrap();
        assert!(text.contains("--- !words"));

        // 导入到新库应还原
        let path2 = tmp("wind_uw_io2.redb");
        let s2 = Store::open(&path2).unwrap();
        let (imported, skipped) = s2.import_user_words_wdict("wb", &text).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(imported, 2);
        let got = s2.get_user_words("wb", "a").unwrap();
        assert_eq!(got[0].text, "工");
        assert_eq!(got[0].weight, 100);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    /// ★★ 显式 `WordIo::boundary` 优先于 code 里的空格，且**单音节边界必须能存活**。
    ///
    /// 空格载体表达不了单音节：`join_code_by_boundary("xian", 0b1)` 产出无空格的
    /// `xian`，`split_spaced_code` 读回来是 0。导入闸口求解出的单字词边界若走 code
    /// 传递会**静默退化为 0，而调用方以为补上了**。本条锁住那条旁路。
    ///
    /// P2 起用户词落文本：单个汉字的单音节由文本解析推断（一个汉字只对应一个音节），所以不给
    /// 显式边界的单字词同样是 0b1；非汉字单字符（emoji）的单音节文本表达不了，退化为 0——
    /// 这是对照组，证明推断只认汉字。
    #[test]
    fn explicit_boundary_survives_where_spaces_cannot() {
        let path = tmp("wind_uw_explicit_b.redb");
        let s = Store::open(&path).unwrap();
        let row = |code: &str, text: &str, b: Option<u64>| crate::wdict::WordIo {
            code: code.into(),
            text: text.into(),
            weight: 100,
            count: 0,
            boundary: b,
        };
        s.import_user_words(
            "pinyin",
            &[
                // 单音节：显式 0b1
                row("xian", "先", Some(0b1)),
                // 不给显式边界的单个汉字：由文本推断
                row("gong", "工", None),
                // 对照：非汉字单字符，显式单音节在文本里表达不了
                row("xiao", "😊", Some(0b1)),
                // 多音节：显式边界与 code 空格都能表达，显式优先
                row("nihao", "你好", Some(0b101)),
            ],
        )
        .unwrap();
        let b = |code: &str, text: &str| {
            s.get_user_words("pinyin", code)
                .unwrap()
                .into_iter()
                .find(|r| r.text == text)
                .unwrap()
                .boundary
        };
        assert_eq!(b("xian", "先"), 0b1, "单音节边界必须存活");
        assert_eq!(b("gong", "工"), 0b1, "单个汉字由文本推断为单音节");
        assert_eq!(b("xiao", "😊"), 0, "非汉字单字符的单音节退化为 0");
        assert_eq!(b("nihao", "你好"), 0b101);
        let _ = std::fs::remove_file(&path);
    }

    /// 显式边界缺省时仍按 code 里的空格推断——旧路径逐字节不变。
    #[test]
    fn spaced_code_still_carries_boundary_when_no_explicit() {
        let path = tmp("wind_uw_spaced_b.redb");
        let s = Store::open(&path).unwrap();
        s.import_user_words(
            "pinyin",
            &[crate::wdict::WordIo {
                code: "ni hao".into(),
                text: "你好".into(),
                weight: 100,
                count: 0,
                boundary: None,
            }],
        )
        .unwrap();
        let got = s.get_user_words("pinyin", "nihao").unwrap();
        assert_eq!(got[0].boundary, 0b101, "key 拆成扁平、边界从空格来");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn import_user_words_merges_max_weight() {
        let path = tmp("wind_uw_merge.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        // 导入同词更低权重 → 保持 max(100)
        let text = crate::wdict::export_words_wdict(
            &[crate::wdict::WordIo {
                code: "a".into(),
                text: "工".into(),
                weight: 30,
                count: 0,
                boundary: None,
            }],
            "2026-07-11T00:00:00+08:00",
        );
        let (imported, _) = s.import_user_words_wdict("wb", &text).unwrap();
        assert_eq!(imported, 1);
        assert_eq!(
            s.get_user_words("wb", "a").unwrap()[0].weight,
            100,
            "Merge 取 max"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn import_user_words_classifies_added_updated_unchanged() {
        let path = tmp("wind_uw_batch.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        let rows = vec![
            // 已有且权重更低 → unchanged(P2 约束 1:不落盘)
            crate::wdict::WordIo {
                code: "a".into(),
                text: "工".into(),
                weight: 30,
                count: 0,
                boundary: None,
            },
            // 新键 → added
            crate::wdict::WordIo {
                code: "b".into(),
                text: "了".into(),
                weight: 5,
                count: 0,
                boundary: None,
            },
        ];
        let c = s.import_user_words("wb", &rows).unwrap();
        assert_eq!((c.added, c.updated, c.unchanged), (1, 0, 1));
        assert_eq!(
            s.get_user_words("wb", "a").unwrap()[0].weight,
            100,
            "unchanged 不改权重"
        );

        // 权重严格更大 → updated,取导入值
        let rows2 = vec![crate::wdict::WordIo {
            code: "a".into(),
            text: "工".into(),
            weight: 200,
            count: 0,
            boundary: None,
        }];
        let c2 = s.import_user_words("wb", &rows2).unwrap();
        assert_eq!((c2.added, c2.updated, c2.unchanged), (0, 1, 0));
        assert_eq!(s.get_user_words("wb", "a").unwrap()[0].weight, 200);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn preview_import_is_readonly_and_matches_import() {
        let path = tmp("wind_uw_preview.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        let rows = vec![
            crate::wdict::WordIo {
                code: "a".into(),
                text: "工".into(),
                weight: 30,
                count: 0,
                boundary: None,
            },
            crate::wdict::WordIo {
                code: "b".into(),
                text: "了".into(),
                weight: 5,
                count: 0,
                boundary: None,
            },
            crate::wdict::WordIo {
                code: "a".into(),
                text: "工".into(),
                weight: 300,
                count: 0,
                boundary: None,
            },
        ];
        let (c, samples) = s.preview_import_user_words("wb", &rows).unwrap();
        assert_eq!((c.added, c.updated, c.unchanged), (1, 1, 1));
        assert_eq!(samples.len(), 2, "samples 只含会落盘的行(added+updated)");
        assert!(samples.iter().any(|x| x.contains("了")));
        // 只读:预览后库里仍只有原 1 条、权重未动
        assert_eq!(s.get_user_words("wb", "a").unwrap()[0].weight, 100);
        assert!(s.get_user_words("wb", "b").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_user_words_only_target_schema() {
        let path = tmp("wind_uw_clear.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 1, 0).unwrap();
        s.add_user_word("wb", "b", "了", 1, 0).unwrap();
        s.add_user_word("py", "ni", "你", 1, 0).unwrap();
        let n = s.clear_user_words("wb").unwrap();
        assert_eq!(n, 2);
        assert!(s.search_user_words_prefix("wb", "", 0).unwrap().is_empty());
        assert_eq!(
            s.search_user_words_prefix("py", "", 0).unwrap().len(),
            1,
            "其它 schema 不受影响"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn export_dict_wdict_roundtrips_words_and_shadow() {
        let path = tmp("wind_uw_dict_io.redb");
        let s = Store::open(&path).unwrap();
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        // shadow：pin + del
        s.pin_shadow("wb", "aaaa", "恭", None, 0).unwrap();
        s.delete_shadow("wb", "bbbb", "见").unwrap();

        let text = s
            .export_dict_wdict("wb", "2026-07-14T00:00:00+08:00")
            .unwrap();
        assert!(text.contains("--- !words"), "含 words 段");
        assert!(text.contains("--- !shadow"), "含 shadow 段");

        // 导入到新库：words + shadow 均还原
        let path2 = tmp("wind_uw_dict_io2.redb");
        let s2 = Store::open(&path2).unwrap();
        let (imported, skipped) = s2.import_user_words_wdict("wb", &text).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(imported, 1);
        let got = s2.get_user_words("wb", "a").unwrap();
        assert_eq!(got[0].weight, 100);

        let (actions, sk) = crate::wdict::parse_shadow_wdict(&text).unwrap();
        assert_eq!(sk, 0);
        let n = s2.import_shadow_actions("wb", &actions).unwrap();
        assert!(n >= 2, "至少重放 pin + del 两条");
        assert!(
            s2.get_shadow_rules("wb", "aaaa").unwrap().is_some(),
            "pin 规则还原"
        );
        assert_eq!(
            s2.get_shadow_rules("wb", "bbbb").unwrap().unwrap().deleted,
            vec!["见".to_string()],
            "del 规则还原"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    /// 手改文本 → sync → 新词可见、删掉的行消失、代次推进；未被手改的未折回操作仍在。
    #[test]
    fn reload_picks_up_hand_edit_and_bumps_generation() {
        let s = Store::open(tmp("wind_uw_handedit.redb")).unwrap();
        s.add_user_word("pinyin", "nihao", "你好", 9, 0b101)
            .unwrap();
        s.add_user_word("pinyin", "ta", "他", 1, 0b1).unwrap();
        s.runtime()
            .data()
            .fold(wind_userdata::record::Target::User, "pinyin")
            .unwrap();
        s.add_user_word("pinyin", "wo", "我", 2, 0b1).unwrap(); // 未折回
        let f = s.user_text_path("pinyin");
        let edited = std::fs::read_to_string(&f)
            .unwrap()
            .replace("ta\t他\t1\n", "")
            + "shou gai\t手改\t3\n";
        std::fs::write(&f, edited).unwrap();
        let g0 = s.words_generation_of("pinyin");
        assert_eq!(s.sync_user_texts(), vec!["pinyin".to_string()]);
        assert_ne!(s.words_generation_of("pinyin"), g0);
        assert_eq!(
            s.get_user_words("pinyin", "shougai").unwrap()[0].text,
            "手改"
        );
        assert!(s.get_user_words("pinyin", "ta").unwrap().is_empty());
        assert_eq!(s.get_user_words("pinyin", "wo").unwrap()[0].text, "我");
        assert!(s.sync_user_texts().is_empty(), "没变就不重建");
    }

    /// dict.reload 的路径只认 `<text_dir>/<schema>/user.dict.yaml`。
    #[test]
    fn reload_rejects_paths_outside_user_dir() {
        let s = Store::open(tmp("wind_uw_reloadpath.redb")).unwrap();
        s.add_user_word("wb", "a", "工", 1, 0).unwrap();
        s.runtime()
            .data()
            .fold(wind_userdata::record::Target::User, "wb")
            .unwrap();
        let ok = s.user_text_path("wb").to_string_lossy().into_owned();
        assert!(s.reload_user_text_paths(&[ok]).is_ok());
        let base = s.user_text_dir().to_string_lossy().into_owned();
        for bad in [
            format!("{base}/../x/user.dict.yaml"),
            format!("{base}/wb/other.dict.yaml"),
            "/etc/passwd".to_string(),
            format!("{base}/wb/../../user.dict.yaml"),
            format!("{base}/user.dict.yaml"),
            "user.dict.yaml".to_string(),
        ] {
            assert!(
                s.reload_user_text_paths(std::slice::from_ref(&bad))
                    .is_err(),
                "{bad}"
            );
        }
    }

    /// Windows 路径不分大小写：盘符、目录、文件名换了大小写 / 斜杠方向，仍认得是哪个文件；
    /// 方案名取磁盘上的实际目录名。区分大小写的平台上照旧拒绝。
    #[test]
    fn text_path_matching_folds_case_when_asked() {
        let d = std::env::temp_dir().join(format!("wind_uw_ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let base = d.join("User");
        std::fs::create_dir_all(base.join("WubiEx")).unwrap();
        let upper = base.to_string_lossy().to_uppercase();
        let p = format!("{upper}/WUBIEX/User.Dict.YAML");
        match classify_text_path(&base, &p, true).unwrap() {
            TextPath::Words(s) => assert_eq!(s, "WubiEx"),
            _ => panic!("应认作用户词文件"),
        }
        assert!(matches!(
            classify_text_path(&base, &format!("{upper}/Phrases.Dict.Yaml"), true).unwrap(),
            TextPath::Phrases
        ));
        assert!(classify_text_path(&base, &p, false).is_err());
        assert!(classify_text_path(&base, &format!("{upper}/../X/user.dict.yaml"), true).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// P5：学到的数据与补全历史也能经 dict.reload 指名重载。
    #[test]
    fn reload_accepts_learned_and_completion_paths() {
        use wind_userdata::record::Target;
        let base = std::path::Path::new("/u/data");
        let p = |rel: &str| base.join(rel).to_string_lossy().into_owned();
        assert!(matches!(
            classify_text_path(base, &p("pinyin/freq.table.yaml"), false).unwrap(),
            TextPath::Learned(Target::Freq, s) if s == "pinyin"
        ));
        assert!(matches!(
            classify_text_path(base, &p("pinyin/temp.dict.yaml"), false).unwrap(),
            TextPath::Learned(Target::Temp, _)
        ));
        assert!(matches!(
            classify_text_path(base, &p("pinyin/draft.dict.yaml"), false).unwrap(),
            TextPath::Learned(Target::Draft, _)
        ));
        assert!(matches!(
            classify_text_path(base, &p("pinyin/assoc.table.yaml"), false).unwrap(),
            TextPath::Learned(Target::Assoc, _)
        ));
        assert!(matches!(
            classify_text_path(base, &p("completion.table.yaml"), false).unwrap(),
            TextPath::Completion
        ));
        assert!(classify_text_path(base, &p("pinyin/other.yaml"), false).is_err());
        assert!(
            matches!(
                classify_text_path(base, &p("stats/2026-10.toml"), false).unwrap(),
                TextPath::Stats
            ),
            "统计按需读文件，重载即生效"
        );
    }

    #[test]
    fn reload_of_hand_edited_freq_text_takes_effect() {
        let s = Store::open(tmp("wind_uw_reload_freq.redb")).unwrap();
        s.record_freq("pinyin", "a", "啊").unwrap();
        s.flush_user_data().unwrap();
        let f = s.user_text_dir().join("pinyin/freq.table.yaml");
        let t = std::fs::read_to_string(&f)
            .unwrap()
            .replace("a\t啊\t1\t", "a\t啊\t9\t");
        std::fs::write(&f, t).unwrap();
        let changed = s
            .reload_user_text_paths(&[f.to_string_lossy().into_owned()])
            .unwrap();
        assert_eq!(changed, vec!["pinyin".to_string()]);
        assert_eq!(
            s.get_freq("pinyin", "a", "啊").unwrap().map(|r| r.count),
            Some(9)
        );
    }

    /// 真实 Windows：dict.reload 收到小写 + 正斜杠写法的路径也能重载。
    #[cfg(windows)]
    #[test]
    fn reload_accepts_case_and_slash_variants_on_windows() {
        let s = Store::open(tmp("wind_uw_reload_ci.redb")).unwrap();
        s.add_user_word("wb", "a", "工", 1, 0).unwrap();
        s.flush_user_data().unwrap();
        let f = s.user_text_path("wb");
        std::fs::write(&f, std::fs::read_to_string(&f).unwrap() + "b\t式\t1\n").unwrap();
        let p = f.to_string_lossy().to_lowercase().replace('\\', "/");
        assert_eq!(
            s.reload_user_text_paths(&[p]).unwrap(),
            vec!["wb".to_string()]
        );
        assert_eq!(s.get_user_words("wb", "b").unwrap()[0].text, "式");
    }

    /// dict.flush：全部待处理操作折回，文本里有。
    #[test]
    fn flush_folds_everything() {
        let s = Store::open(tmp("wind_uw_flush.redb")).unwrap();
        s.add_user_word("wb", "a", "工", 1, 0).unwrap();
        s.flush_user_data().unwrap();
        assert!(
            std::fs::read_to_string(s.user_text_path("wb"))
                .unwrap()
                .contains("a\t工\t1")
        );
    }

    /// 手改短语与 Shadow 文件：sync_user_texts 一并载入，返回值含 "phrases" 与方案名；dict.reload
    /// 也认这两种文件。
    #[test]
    fn sync_picks_up_phrase_and_shadow_hand_edits() {
        let s = Store::open(tmp("wind_uw_sync_ph_sh.redb")).unwrap();
        s.add_phrase("yx", "me@x.com", 0, 1800).unwrap();
        s.pin_shadow("wb", "aa", "恭", None, 0).unwrap();
        let pf = s.phrases_path();
        std::fs::write(
            &pf,
            std::fs::read_to_string(&pf).unwrap() + "dz\t地址\t1800\n",
        )
        .unwrap();
        let sf = s.shadow_path("wb");
        std::fs::write(
            &sf,
            std::fs::read_to_string(&sf).unwrap() + "bb\t八\tdelete\n",
        )
        .unwrap();
        let mut changed = s.sync_user_texts();
        changed.sort();
        assert_eq!(changed, vec!["phrases".to_string(), "wb".to_string()]);
        assert!(
            s.enabled_phrases_for_input()
                .unwrap()
                .iter()
                .any(|p| p.code == "dz")
        );
        assert!(s.get_shadow_rules("wb", "bb").unwrap().is_some());
        std::fs::write(&pf, std::fs::read_to_string(&pf).unwrap() + "xx\t叉\t1\n").unwrap();
        let p = pf.to_string_lossy().into_owned();
        assert_eq!(
            s.reload_user_text_paths(&[p]).unwrap(),
            vec!["phrases".to_string()]
        );
        let p = sf.to_string_lossy().into_owned();
        assert!(s.reload_user_text_paths(&[p]).is_ok());
    }
}
