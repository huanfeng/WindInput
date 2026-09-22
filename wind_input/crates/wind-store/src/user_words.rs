//! 用户词存储（redb）
//!
//! 与 Go 版本 `wind_input/internal/store/user_words.go` 对齐，但：
//! - value 用定长 28 字节（weight i32 + count u32 + created_at i64 + boundary u64 + order u32），
//!   text/code 存于 key，比 Go 的 JSON 紧凑（store.md §7.3）。历经 16B→24B→28B 三版，
//!   一路惰性升级免 migration，见 `dec_val_ordered`。
//! - created_at 统一为 i64 unix 秒（修 Go user=秒/temp=毫秒 不一致，store.md §7.2）。
//!
//! key 编码：`"{schema}\0{code}\0{text}"`（store.md §2）。

use crate::abbrev_index;
use crate::store::{META, Store, USER_ABBREV, USER_WORDS};
use crate::wdict;
use redb::{ReadableTable, WriteTransaction};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// 遍历期的**借用视图**：`code` / `text` 直接指向 redb 页里的字节，不分配。
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

/// 当前 unix 秒
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 用户词入库序号的发号器游标（存 META 表）。
const NEXT_ORDER_KEY: &str = "user_words_next_order";

/// 在**调用方的写事务内**领取 `n` 个连续的入库序号，返回起始号（含）。
///
/// # 为什么必须同事务
///
/// 号段与词条写入同属调用方那一个事务：回滚时号段一并作废，不会在序号轴上留下空洞；
/// 提交后游标已推进，下一批必然更大 —— 于是「入库先后」与「序号大小」恒同序。
/// 若改成自开事务先领号再写词，两者之间崩溃就会漏号，更糟的是并发两个导入可能拿到同一段。
///
/// # 为什么不扫表取 max+1
///
/// 那要求每次写入都全表扫一遍；用户词库上万条时代价不可接受，且并发下仍会撞号。
///
/// 序号从 **1** 起：0 被 `enc_val_ordered` 保留表示「无序号」（v1/v2 遗留记录）。
///
/// 游标用 `saturating_add` 封顶，全程无回绕、无负数、无 panic；到顶后新词一律拿同一个号，
/// 退化成「新词之间无序」。⚠️ 但**推进速度不是「词条数」**：`import_user_words` 一次领满
/// `rows.len()`，不管其中多少行只是更新 —— 所以游标吃掉的是**累计导入行数**。
/// 一个一万条的词库反复导入约 43 万次即可耗尽 u32，虽仍属够不着，但别按「42 亿个词」去理解。
pub(crate) fn take_word_orders(txn: &WriteTransaction, n: u32) -> anyhow::Result<u32> {
    let mut t = txn.open_table(META)?;
    let cur = t
        .get(NEXT_ORDER_KEY)?
        .and_then(|g| <[u8; 4]>::try_from(g.value()).ok())
        .map(u32::from_le_bytes)
        .unwrap_or(1);
    t.insert(
        NEXT_ORDER_KEY,
        cur.saturating_add(n).to_le_bytes().as_slice(),
    )?;
    Ok(cur)
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

/// 手动加词文本长度上限（字符数）。
///
/// `add_user_word` 是所有**手动加词**路径的共同写入闸口（Ctrl+= 快捷加词、命令栏
/// `dict.add` 显式给码、设置页词库管理），本上限加在那一处即等价于加在全部手动入口，
/// 不必在每个调用点重复。⚠️ 批量导入（`import_user_words`）走独立写入逻辑、不经过
/// 此处，库里仍可能存在历史导入的超长词条——这不是本上限的覆盖范围。
///
/// 数值不追求精确：`ADD_WORD_MAX_LEN=10`（wind-coordinator `handle_addword.rs`）已经拦住了
/// 推导编码的正常场景，但**显式给码**（`dict.add(text, code)`）与设置页手动加词两条路径不
/// 经过那道校验，此前无任何长度限制（用户实测提交过 7500 字词条，未必是本上限拦住的现象——
/// 这里只是兜底防线，防止病态输入，不代表 7500 字场景本身已被诊断清楚）。10000 字对真实
/// 使用场景足够宽松。
pub(crate) const USER_WORD_TEXT_MAX_CHARS: usize = 10000;

/// 手动加词 code（编码）长度上限（字符数）。
///
/// 与 [`USER_WORD_TEXT_MAX_CHARS`] 同一闸口、同一动机，但数值按 code 的实际容量定：
/// 二进制格式 `code_len: u16`（`wind_dict::binformat`）本身给不出实质约束，真正收紧的是
/// 拼音音节边界 `boundary: u64`——**code 超过 64 字节，边界位掩码就装不下、消费方降级回
/// DAG 猜测**。真实方案数值都远小于它：五笔类码表码固定 4 位，英文方案
/// `max_code_length=32`，10 字词组全拼全码约 60 字节。128 留出 2 倍以上余量，够真实场景
/// 用、又能挡住病态输入（比如把 text 错填进了 code 参数）。
pub(crate) const USER_WORD_CODE_MAX_CHARS: usize = 128;

impl Store {
    /// 新增/合并用户词：已存在则权重取 max、保留原 created_at；新词记 created_at=now。
    /// 用户词**无权重上限**（store.md §3），但文本/编码长度分别有 `USER_WORD_TEXT_MAX_CHARS`
    /// / `USER_WORD_CODE_MAX_CHARS` 兜底上限。
    ///
    /// `boundary`：该 code 的音节边界（见 `wind_dict::binformat::DictEntry::boundary`）。
    /// 造词路径（`generate_word_pinyin`）算得；用户手输码/wdict 导入无从得知，传 0
    /// （消费方降级回 DAG）。已存在且旧值非 0 时沿用旧值——同 (schema,code,text) 的切分是
    /// 确定的，不因再次加词而变。
    pub fn add_user_word(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        weight: i32,
        boundary: u64,
    ) -> anyhow::Result<()> {
        let n = text.chars().count();
        if n > USER_WORD_TEXT_MAX_CHARS {
            anyhow::bail!("词条过长（{n} 字，上限 {USER_WORD_TEXT_MAX_CHARS}）");
        }
        let code_len = code.chars().count();
        if code_len > USER_WORD_CODE_MAX_CHARS {
            anyhow::bail!("编码过长（{code_len} 字，上限 {USER_WORD_CODE_MAX_CHARS}）");
        }
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(USER_WORDS)?;
                let existing = t
                    .get(key.as_str())?
                    .and_then(|g| dec_val_ordered(g.value()));
                let (w, c, ca, b) = match existing {
                    Some((ow, oc, oca, ob, _)) => {
                        (ow.max(weight), oc, oca, if ob != 0 { ob } else { boundary })
                    }
                    None => (weight, 0, now_secs(), boundary),
                };
                // 入库序号：已存在的原样保留（含 0 —— 旧库记录不补号，理由见 `enc_val_ordered`
                // 的惰性升级一节：补了它们会拿到比所有新词更大的号，反而打乱既有词序）。
                let order = match existing {
                    Some((_, _, _, _, oo)) => oo,
                    None => take_word_orders(&txn, 1)?,
                };
                // 边界可能从 0 被补齐（见上），索引键随之改变 → shift 负责删旧建新。
                let old_b = existing.map(|(_, _, _, ob, _)| ob);
                let mut idx = txn.open_table(USER_ABBREV)?;
                abbrev_index::shift(&mut idx, schema, code, text, old_b, b)?;
                t.insert(key.as_str(), enc_val_ordered(w, c, ca, b, order).as_slice())?;
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 流式遍历某方案下以 `prefix` 开头的用户词。回调返回 `false` 即停。
    ///
    /// 与 [`Self::search_user_words_prefix`] 的唯一差别是**不物化**：`code` / `text` 直接
    /// 借 redb 的页，一条也不往堆上抄。调用方自己决定留哪几条。
    ///
    /// # 为什么要有它
    ///
    /// 设置页的分页列表从前一律「先全收进 `Vec` 再 `skip(offset).take(limit)`」——用户
    /// 19 万条的词库，翻第一页也要先造 38 万个 `String`。本机实测那一趟几十 MB 峰值，
    /// 而真正要显示的只有 50 条。
    ///
    /// ⚠️ **它省的是 Rust 侧的峰值，不是 redb 的读缓存**：range 迭代照样把走过的 B 树
    /// 叶子页读进缓存（key 与 value 同页），那笔高水位只有丢弃 `Database` 才还得掉
    /// （见 `tests/redb_cache_high_water.rs` 与 [`Store::drop_page_cache`]）。两件事要分开记。
    pub fn for_each_user_word(
        &self,
        schema: &str,
        prefix: &str,
        f: &mut dyn FnMut(UserWordView<'_>) -> bool,
    ) -> anyhow::Result<()> {
        let scan = format!("{schema}\u{0}{prefix}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(USER_WORDS)?;
            for item in t.range(scan.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&scan) {
                    break;
                }
                let (Some((_, code, text)), Some((w, c, ca, b, o))) =
                    (split_key(key), dec_val_ordered(v.value()))
                else {
                    // 解不出的记录**跳过但不计数**，与 `search_user_words_prefix` 一致：
                    // 那边也是 `if let` 不匹配就不 push。总数口径必须和它逐条对齐，否则
                    // 会出现「说有 N 条、翻到最后一页只有 N-1 条」。
                    continue;
                };
                if !f(UserWordView {
                    code,
                    text,
                    weight: w,
                    count: c,
                    created_at: ca,
                    boundary: b,
                    order: o,
                }) {
                    break;
                }
            }
            Ok(())
        })
    }

    /// 某方案下的用户词条数。只数不抄。
    ///
    /// 口径与 [`Self::search_user_words_prefix`]`(schema, "", 0).len()` 逐条一致
    /// （解不出的记录两边都不算）——设置页拿它显示「共 N 条」，与列表必须对得上。
    pub fn count_user_words(&self, schema: &str) -> anyhow::Result<usize> {
        let mut n = 0usize;
        self.for_each_user_word(schema, "", &mut |_| {
            n += 1;
            true
        })?;
        Ok(n)
    }

    /// 精确取某 code 下的所有用户词
    pub fn get_user_words(&self, schema: &str, code: &str) -> anyhow::Result<Vec<UserWordRecord>> {
        let prefix = format!("{schema}\u{0}{code}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(USER_WORDS)?;
            let mut out = Vec::new();
            for item in t.range(prefix.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&prefix) {
                    break;
                }
                let text = &key[prefix.len()..];
                if let Some((w, c, ca, b, o)) = dec_val_ordered(v.value()) {
                    out.push(UserWordRecord {
                        code: code.to_string(),
                        text: text.to_string(),
                        weight: w,
                        count: c,
                        created_at: ca,
                        boundary: b,
                        order: o,
                    });
                }
            }
            Ok(out)
        })
    }

    /// 前缀检索（跨 code）。limit<=0 表示不限。
    pub fn search_user_words_prefix(
        &self,
        schema: &str,
        prefix: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<UserWordRecord>> {
        let scan = format!("{schema}\u{0}{prefix}");
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(USER_WORDS)?;
            let mut out = Vec::new();
            for item in t.range(scan.as_str()..)? {
                let (k, v) = item?;
                let key = k.value();
                if !key.starts_with(&scan) {
                    break;
                }
                if let (Some((_, code, text)), Some((w, c, ca, b, o))) =
                    (split_key(key), dec_val_ordered(v.value()))
                {
                    out.push(UserWordRecord {
                        code: code.to_string(),
                        text: text.to_string(),
                        weight: w,
                        count: c,
                        created_at: ca,
                        boundary: b,
                        order: o,
                    });
                }
                if limit > 0 && out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
    }

    /// 删除用户词（不存在静默成功）
    pub fn remove_user_word(&self, schema: &str, code: &str, text: &str) -> anyhow::Result<()> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(USER_WORDS)?;
                // 先读边界才能算出索引键——删主表之后就查不到了，顺序不可调换。
                let b = t
                    .get(key.as_str())?
                    .and_then(|g| dec_val(g.value()))
                    .map(|(_, _, _, b)| b);
                t.remove(key.as_str())?;
                if let Some(b) = b {
                    abbrev_index::remove(&mut txn.open_table(USER_ABBREV)?, schema, code, text, b)?;
                }
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 更新用户词权重（不存在返回 false，不创建）
    pub fn update_user_word_weight(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        new_weight: i32,
    ) -> anyhow::Result<bool> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let updated;
            {
                let mut t = txn.open_table(USER_WORDS)?;
                let existing = t
                    .get(key.as_str())?
                    .and_then(|g| dec_val_ordered(g.value()));
                match existing {
                    // 仅改权重：boundary 与 order 均沿用（切分、入库先后都与权重无关）。
                    Some((_, c, ca, b, o)) => {
                        t.insert(
                            key.as_str(),
                            enc_val_ordered(new_weight, c, ca, b, o).as_slice(),
                        )?;
                        updated = true;
                    }
                    None => updated = false,
                }
            }
            txn.commit()?;
            Ok(updated)
        })
    }

    /// 选词回调：count++，每 count_threshold 次给权重 +boost_delta；不存在则创建（weight=0）。
    /// 注：用户词的"调频"为权重微调；候选"用过上浮"由独立的用户词频系统负责（frequency.md）。
    pub fn on_word_selected(
        &self,
        schema: &str,
        code: &str,
        text: &str,
        boost_delta: i32,
        count_threshold: u32,
    ) -> anyhow::Result<()> {
        let key = enc_key(schema, code, text);
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(USER_WORDS)?;
                // 不存在则创建 weight=0 记录（隐性造词路径）：此处只有扁平 code，无边界可算 → 0。
                let existing = t
                    .get(key.as_str())?
                    .and_then(|g| dec_val_ordered(g.value()));
                let is_new = existing.is_none();
                // 本路径会凭空造词（见下），新词同样要领入库序号；已有的原样保留。
                let order = match existing {
                    Some((_, _, _, _, oo)) => oo,
                    None => take_word_orders(&txn, 1)?,
                };
                let (w, c, ca, b, _) = existing.unwrap_or((0, 0, now_secs(), 0, 0));
                let nc = c.saturating_add(1);
                let nw = if count_threshold > 0 && nc % count_threshold == 0 {
                    w.saturating_add(boost_delta)
                } else {
                    w
                };
                t.insert(
                    key.as_str(),
                    enc_val_ordered(nw, nc, ca, b, order).as_slice(),
                )?;
                // ⚠️ **本路径会凭空造出用户词**（上面那句注释说的「隐性造词」），故必须建索引。
                // 改权重不用动索引（value 空），但新增必须——漏了这一处，靠选词自动产生的
                // 词就永远进不了简拼索引，且只在「用过一段时间后」才显形。
                if is_new {
                    abbrev_index::insert(&mut txn.open_table(USER_ABBREV)?, schema, code, text, b)?;
                }
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// 清空某 schema 的全部用户词(单写事务),返回删除条数。
    pub fn clear_user_words(&self, schema: &str) -> anyhow::Result<usize> {
        let prefix = format!("{schema}\u{0}");
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let n;
            {
                let mut t = txn.open_table(USER_WORDS)?;
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
            abbrev_index::clear_schema(&mut txn.open_table(USER_ABBREV)?, schema)?;
            txn.commit()?;
            Ok(n)
        })
    }

    /// 批量导入用户词(单写事务,Merge 语义与 add_user_word 一致):
    /// 新键 → added(count=0, created_at=now);导入权重 > 现有 → updated(保留 count/created_at);
    /// 否则 → unchanged(不写)。dry-run 见 preview_import_user_words,两者分类必须一致。
    pub fn import_user_words(
        &self,
        schema: &str,
        rows: &[wdict::WordIo],
    ) -> anyhow::Result<WordsImportCounts> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            let mut c = WordsImportCounts::default();
            {
                let mut t = txn.open_table(USER_WORDS)?;
                let mut idx = txn.open_table(USER_ABBREV)?;
                // ★ t80 的正题：**一次领满号段**，按 `rows` 的行序依次发给新词条，于是
                // 「词条在导入文件里的先后」= 「order 的大小」，同码等权时便按原文件顺序出。
                // 逐条领号也对，但那要每条都开一次 META 表读写；万条词库时代价白费。
                // 更新已有词条不消耗号，于是号段里会留下空洞 —— 无害，order 只比大小、
                // 不要求连续。
                let mut next_order = take_word_orders(&txn, rows.len() as u32)?;
                for r in rows {
                    // code 列可能是带空格的音节码（`ni hao`）→ 拆成扁平 key + 边界。
                    // 无空格（五笔码/旧版导出）→ boundary=0，与改动前等价。
                    let (code, spaced_b) = wdict::split_spaced_code(&r.code);
                    // ★ 显式边界优先：空格载体表达不了「单音节」（`xian` 的 0b1 经
                    // join→split 会退化成 0），故导入闸口求解出的边界走 `WordIo::boundary`。
                    let in_b = r.boundary.unwrap_or(spaced_b);
                    let key = enc_key(schema, &code, &r.text);
                    let existing = t
                        .get(key.as_str())?
                        .and_then(|g| dec_val_ordered(g.value()));
                    match existing {
                        None => {
                            t.insert(
                                key.as_str(),
                                enc_val_ordered(r.weight, r.count, now_secs(), in_b, next_order)
                                    .as_slice(),
                            )?;
                            next_order = next_order.saturating_add(1);
                            abbrev_index::insert(&mut idx, schema, &code, &r.text, in_b)?;
                            c.added += 1;
                        }
                        Some((w, cnt, ca, b, o)) => {
                            // weight/count 各取 max；boundary 旧值非 0 则沿用（同
                            // `add_user_word`：同 (schema,code,text) 的切分是确定的，
                            // 不因再次导入而变），旧值为 0 时用导入行补齐。
                            // 三者任一变化即写盘为 updated，否则 unchanged。
                            let nw = w.max(r.weight);
                            let nc = cnt.max(r.count);
                            let nb = if b != 0 { b } else { in_b };
                            if nw != w || nc != cnt || nb != b {
                                // order 原样保留：再次导入同一个词不该把它挪到队尾。
                                t.insert(
                                    key.as_str(),
                                    enc_val_ordered(nw, nc, ca, nb, o).as_slice(),
                                )?;
                                // 边界被补齐时索引键随之改变 → shift 删旧建新。
                                if nb != b {
                                    abbrev_index::shift(
                                        &mut idx,
                                        schema,
                                        &code,
                                        &r.text,
                                        Some(b),
                                        nb,
                                    )?;
                                }
                                c.updated += 1;
                            } else {
                                c.unchanged += 1;
                            }
                        }
                    }
                }
            }
            txn.commit()?;
            Ok(c)
        })
    }

    /// 导入 dry-run(只读):分类规则与 import_user_words 完全一致;
    /// samples 取前 5 个会落盘行(added/updated)的 "code text"。
    pub fn preview_import_user_words(
        &self,
        schema: &str,
        rows: &[wdict::WordIo],
    ) -> anyhow::Result<(WordsImportCounts, Vec<String>)> {
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(USER_WORDS)?;
            let mut c = WordsImportCounts::default();
            let mut samples = Vec::new();
            for r in rows {
                // 与 import_user_words 同款拆分：key 必须用扁平码，否则带空格的行
                // 一律查不到既有记录、全部误报为 added。显式边界同样优先（判据必须与
                // 落库端逐项一致，否则预览的 willUpdate 会与实际不符）。
                let (code, spaced_b) = wdict::split_spaced_code(&r.code);
                let in_b = r.boundary.unwrap_or(spaced_b);
                let key = enc_key(schema, &code, &r.text);
                let existing = t.get(key.as_str())?.and_then(|g| dec_val(g.value()));
                let will_write = match existing {
                    None => {
                        c.added += 1;
                        true
                    }
                    // 判据须与 import_user_words 逐项对齐，含 boundary 补齐那一项
                    // （旧值为 0 且导入行给得出边界 → 会落盘 → 算 updated）。
                    Some((w, cnt, _, b))
                        if r.weight > w || r.count > cnt || (b == 0 && in_b != 0) =>
                    {
                        c.updated += 1;
                        true
                    }
                    Some(_) => {
                        c.unchanged += 1;
                        false
                    }
                };
                if will_write && samples.len() < 5 {
                    samples.push(format!("{} {}", r.code, r.text));
                }
            }
            Ok((c, samples))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 惰性升级：旧 16B 记录经一次写入后自然补齐为 24B，boundary 从此可用。
    #[test]
    fn v1_record_upgrades_on_write() {
        let path = tmp("wind_uw_v1_upgrade.redb");
        let s = Store::open(&path).unwrap();

        // 直接以 v1（16B）格式塞一条记录，绕过 add_user_word，模拟旧库。
        let key = enc_key("py", "nihao", "你好");
        {
            let mut v1 = [0u8; 16];
            v1[0..4].copy_from_slice(&500i32.to_le_bytes());
            v1[4..8].copy_from_slice(&3u32.to_le_bytes());
            v1[8..16].copy_from_slice(&1_700_000_000i64.to_le_bytes());
            s.with_db(|db| {
                let txn = db.begin_write()?;
                {
                    let mut t = txn.open_table(USER_WORDS)?;
                    t.insert(key.as_str(), v1.as_slice())?;
                }
                txn.commit()?;
                Ok(())
            })
            .unwrap();
        }

        // 旧记录可读，boundary=0。
        let r = s.get_user_words("py", "nihao").unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].weight, 500, "旧记录的既有字段不得丢失");
        assert_eq!(r[0].boundary, 0, "v1 无 boundary");

        // 再次加词（权重取 max）：旧值 boundary=0 → 用新算出的补齐。
        s.add_user_word("py", "nihao", "你好", 100, 0b101).unwrap();
        let r2 = s.get_user_words("py", "nihao").unwrap();
        assert_eq!(r2[0].weight, 500, "权重取 max，不被低值覆盖");
        assert_eq!(r2[0].boundary, 0b101, "旧记录 boundary=0 时应被新值补齐");

        // 已有非 0 boundary 时沿用，不被后续调用抹掉（切分与 code/text 绑定，不因再加词而变）。
        s.add_user_word("py", "nihao", "你好", 100, 0).unwrap();
        let r3 = s.get_user_words("py", "nihao").unwrap();
        assert_eq!(r3[0].boundary, 0b101, "已有边界不该被 0 覆盖");
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

    /// **索引必须覆盖每一条写路径。**
    ///
    /// 这是本方案唯一的系统性风险：主表写了、索引没写，简拼就静默召不回那个词——
    /// 不报错、不告警。五条写路径逐一验证。
    #[test]
    fn every_write_path_maintains_the_index() {
        let p = tmp("wind_abbrev_paths.redb");
        let s = Store::open(&p).unwrap();
        let hit = |ab: &str| -> Vec<String> {
            s.search_user_words_by_abbrev("py", ab, 0)
                .unwrap()
                .into_iter()
                .map(|r| r.text)
                .collect()
        };

        // ① add_user_word
        s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
        assert_eq!(hit("nh"), vec!["你好"], "add_user_word 应建索引");

        // ② import_user_words
        s.import_user_words(
            "py",
            &[wdict::WordIo {
                code: "xi an ning".into(),
                text: "西安宁".into(),
                weight: 700,
                count: 0,
                boundary: None,
            }],
        )
        .unwrap();
        assert_eq!(hit("xan"), vec!["西安宁"], "import 应建索引");

        // ③ on_word_selected 的隐性造词（最容易漏的一条）
        s.on_word_selected("py", "zg", "中国", 0, 0).unwrap();
        assert_eq!(
            hit("zg"),
            vec!["中国"],
            "隐性造词的记录 boundary=0，应落在 z 的兜底组（否则它永远召不回）"
        );

        // ④ remove_user_word
        s.remove_user_word("py", "nihao", "你好").unwrap();
        assert!(hit("nh").is_empty(), "remove 应删索引");

        // ⑤ clear_user_words
        s.clear_user_words("py").unwrap();
        assert!(
            hit("xan").is_empty() && hit("zg").is_empty(),
            "clear 应清索引"
        );
        assert_eq!(s.abbrev_index_len(), 0, "索引应随主表一起清空");
        let _ = std::fs::remove_file(&p);
    }

    /// **改权重不该动索引**——这正是 value 留空的收益。
    /// 若哪天把 weight 塞进 value，这两条高频路径就都得同步更新，漏一处即静默错乱。
    #[test]
    fn weight_changes_do_not_touch_the_index() {
        let p = tmp("wind_abbrev_weight.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
        let before = s.abbrev_index_len();

        s.update_user_word_weight("py", "nihao", "你好", 900)
            .unwrap();
        s.on_word_selected("py", "nihao", "你好", 100, 1).unwrap();
        assert_eq!(s.abbrev_index_len(), before, "索引条数不该变");

        let got = s.search_user_words_by_abbrev("py", "nh", 0).unwrap();
        assert_eq!(got.len(), 1);
        assert!(
            got[0].weight >= 900,
            "回主表点查拿到的必须是**最新**权重，实际 {}",
            got[0].weight
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 边界从 0 被补齐时，索引键要跟着搬家——旧键残留会变成永远匹配不上的幽灵。
    #[test]
    fn filling_boundary_moves_the_index_key() {
        let p = tmp("wind_abbrev_move.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("py", "nihao", "你好", 500, 0).unwrap(); // 无边界 → \u{1}n 兜底组
        // 探针：`nx` 的声母组必然为空，故命中只可能来自 n 的兜底组。
        assert_eq!(
            s.search_user_words_by_abbrev("py", "nx", 0).unwrap().len(),
            1,
            "补齐前它躺在 n 的兜底组里"
        );

        s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap(); // 补齐边界
        assert_eq!(
            s.abbrev_index_len(),
            1,
            "补齐后仍应只有一条索引（旧键必须被删）"
        );
        assert!(
            s.search_user_words_by_abbrev("py", "nx", 0)
                .unwrap()
                .is_empty(),
            "补齐边界后不该再留在兜底组（残留即幽灵：查什么都跟着出来）"
        );
        assert_eq!(
            s.search_user_words_by_abbrev("py", "nh", 0).unwrap().len(),
            1
        );
        let _ = std::fs::remove_file(&p);
    }

    /// 存量数据补建索引：老库升级上来时索引是空的，不补建则简拼静默失效。
    #[test]
    fn rebuild_covers_preexisting_words() {
        let p = tmp("wind_abbrev_rebuild.redb");
        let s = Store::open(&p).unwrap();
        s.add_user_word("py", "nihao", "你好", 500, 0b101).unwrap();
        s.add_user_word("wb", "aaaa", "工工", 100, 0).unwrap();

        // 模拟老库：主表有数据、索引为空
        s.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut idx = txn.open_table(USER_ABBREV)?;
                let keys: Vec<String> = idx
                    .iter()?
                    .filter_map(|i| i.ok().map(|(k, _)| k.value().to_string()))
                    .collect();
                for k in &keys {
                    idx.remove(k.as_str())?;
                }
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
        assert_eq!(s.abbrev_index_len(), 0, "前提：索引确实被清空了");
        assert!(
            s.search_user_words_by_abbrev("py", "nh", 0)
                .unwrap()
                .is_empty(),
            "前提校验：没有索引时确实召不回——这正是必须补建的理由"
        );

        assert_eq!(s.rebuild_abbrev_indexes().unwrap(), 2);
        assert_eq!(
            s.search_user_words_by_abbrev("py", "nh", 0).unwrap().len(),
            1,
            "补建后应能召回"
        );
        assert_eq!(
            s.search_user_words_by_abbrev("wb", "aaaa", 0)
                .unwrap()
                .len(),
            1,
            "跨 schema 的词也要补建，且各归各的 schema"
        );
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
    fn test_on_word_selected_threshold_boost() {
        let path = tmp("wind_uw_sel.redb");
        let s = Store::open(&path).unwrap();
        // 阈值 3：第 3 次选词才 +boost
        for _ in 0..2 {
            s.on_word_selected("wb", "a", "工", 500, 3).unwrap();
        }
        assert_eq!(
            s.get_user_words("wb", "a").unwrap()[0].weight,
            0,
            "未到阈值不加权"
        );
        s.on_word_selected("wb", "a", "工", 500, 3).unwrap();
        let r = s.get_user_words("wb", "a").unwrap();
        assert_eq!(r[0].count, 3);
        assert_eq!(r[0].weight, 500, "第 3 次达阈值 +500");
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
    /// ⚠️ 对照组是本测试的要害：不给显式边界时确实退化为 0——那正是旁路存在的理由。
    /// 少了对照组，这条测试无法证明「显式边界」比「code 空格」多做了任何事。
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
                // 对照：同样单音节码，不给显式边界
                row("gong", "工", None),
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
        assert_eq!(
            b("gong", "工"),
            0,
            "不给显式边界 ⇒ 空格载体推不出单音节，退化为 0"
        );
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
    fn export_dict_wdict_roundtrips_words_count_and_shadow() {
        let path = tmp("wind_uw_dict_io.redb");
        let s = Store::open(&path).unwrap();
        // 用户词 + 调频次数
        s.add_user_word("wb", "a", "工", 100, 0).unwrap();
        s.on_word_selected("wb", "a", "工", 0, 0).unwrap(); // count -> 1
        s.on_word_selected("wb", "a", "工", 0, 0).unwrap(); // count -> 2
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
        assert_eq!(got[0].count, 2, "count(调频)随导出/导入流转");

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
}
