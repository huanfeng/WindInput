//! 补全候选的**用户学习数据**：邮箱后缀词频与网址输入历史。
//!
//! ## 一张表承载两类，而不是两张表
//!
//! 两者的形态完全一样——「一条文本 + 用过多少次 + 最后一次什么时候」，消费端也一样
//! （按 `(count, last_used)` 排序后作为补全候选）。拆两张表就要把
//! record/list/remove/clear/prune/export/import 七组原语各写两遍，而它们唯一的差别
//! 只是表名；两份实现迟早分叉，分叉的表现是「邮箱后缀按频次排了、网址历史没排」
//! 这类只在一边出现的怪事。
//!
//! 故 key 编码为 `"{kind}\0{text}"`，[`CompletionKind`] 是全部合法 kind 的**闭集**——
//! 用枚举而不是裸字符串，是因为 kind 拼错不会报错，只会写进一个谁也读不到的分区，
//! 且清空时同样读不到（"emial" 那条数据会永远留在库里）。
//!
//! ## 键不带方案：这是全局属性
//!
//! 与 [`crate::common_chars`] 同一条判据——「我常用哪个邮箱后缀」「我访问过哪个网址」
//! 与用五笔还是拼音毫无关系。带 schema 前缀会让用户切个方案就发现补全全没了。
//!
//! ## 与 [`crate::freq`] 的分界
//!
//! 词频表回答「这个方案、这个码下，这个词该排第几」，键必须带方案与输入码；本表回答
//! 「这条文本用过几次」，没有码的概念。两者 value 编码刻意同构（12 字节定长），但**不
//! 复用同一张表**：freq 的 key 结构是三段式，塞进来只能用空 schema/空 code 假装，那会
//! 让 `list_data_schemas()` 之类按前缀扫描的逻辑读到一个不存在的方案。

use crate::store::{COMPLETION, Store};

/// 学习数据的类别（表 key 的第一段）。
///
/// 闭集：新增一类补全数据就在这里加一个变体，`as_str` 是它在库里的唯一写法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    /// 邮箱后缀（存 `@` **之后**的部分，如 `qq.com`）。
    ///
    /// 不含 `@`：用户可能把同一个域名写进预置列表时带不带 `@` 不一致，存归一化后的
    /// 形态才能与预置列表比对去重。拼回完整邮箱是消费端的事。
    EmailSuffix,
    /// 网址输入历史（存上屏的完整文本，如 `www.example.com/path`）。
    UrlHistory,
    /// 快捷输入历史（存在快捷输入里上屏过的字面文本，如 `${reset_time}`、`RESET_TIME`）。
    ///
    /// **区分大小写地存**：`RESET_TIME` 与 `reset_time` 是两条（t46）。按前缀**不区分大小写**
    /// 地取是消费端的事，见协调器 `quick_history_candidates`。
    QuickHistory,
}

impl CompletionKind {
    /// 全部类别。导出、整表清空等「对每一类都做一遍」的地方一律遍历它，不各自手写列表
    /// ——新增一类时手写的列表会被漏，症状是那一类不进备份、还原时也清不掉。
    pub const ALL: [CompletionKind; 3] = [
        CompletionKind::EmailSuffix,
        CompletionKind::UrlHistory,
        CompletionKind::QuickHistory,
    ];

    /// 库中的字面写法。**改动即破坏存量数据**（旧 kind 的记录会读不到）。
    pub fn as_str(self) -> &'static str {
        match self {
            CompletionKind::EmailSuffix => "email_suffix",
            CompletionKind::UrlHistory => "url_history",
            CompletionKind::QuickHistory => "quick_history",
        }
    }

    /// 从库中字面写法还原。未知字符串返回 `None`（导入时用来跳过陌生分区）。
    ///
    /// 名字刻意不叫 `from_str`：那个名字会被 clippy 判为与 `std::str::FromStr::from_str`
    /// 易混（这里返回 `Option` 而非 `Result`，签名并不兼容）。同 `DictSection::from_key`。
    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "email_suffix" => Some(CompletionKind::EmailSuffix),
            "url_history" => Some(CompletionKind::UrlHistory),
            "quick_history" => Some(CompletionKind::QuickHistory),
            _ => None,
        }
    }
}

/// 一条学习记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionRecord {
    /// 用过多少次。
    pub count: u32,
    /// 最后一次使用的 Unix 秒。
    pub last_used: i64,
}

/// 单条文本的长度上限（字符数）。
///
/// 网址可以很长，但**不该无上限**：上屏文本直接进 key，一条畸形长文本会把整张表撑大，
/// 而补全候选窗根本显示不下。与 [`crate::user_words::USER_WORD_CODE_MAX_CHARS`] 同一
/// 用意，取值放宽到 512 是因为带查询串的网址确实能到几百字符。
pub const COMPLETION_TEXT_MAX_CHARS: usize = 512;

/// value: count u32 + last_used i64 = 12 字节（与 [`crate::freq`] 同构）。
fn enc_rec(count: u32, last_used: i64) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&count.to_le_bytes());
    b[4..12].copy_from_slice(&last_used.to_le_bytes());
    b
}

fn dec_rec(b: &[u8]) -> Option<CompletionRecord> {
    if b.len() < 12 {
        return None;
    }
    Some(CompletionRecord {
        count: u32::from_le_bytes(b[0..4].try_into().ok()?),
        last_used: i64::from_le_bytes(b[4..12].try_into().ok()?),
    })
}

fn enc_key(kind: CompletionKind, text: &str) -> String {
    format!("{}\u{0}{}", kind.as_str(), text)
}

/// 扫描某 kind 全部记录用的前缀（含分隔符，避免 `email` 误配 `email_suffix`）。
fn scan_prefix(kind: CompletionKind) -> String {
    format!("{}\u{0}", kind.as_str())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 排序判据：次数降序 → 最近使用降序 → 文本升序。
///
/// 末尾那道文本比较不是装饰：只按前两项排时，两条次数与时刻都相同的记录顺序取决于
/// 存储的返回次序，候选窗会在两次相同输入间无故换序。补全候选的位置必须是**可盲打**的。
fn better(a: &(String, CompletionRecord), b: &(String, CompletionRecord)) -> std::cmp::Ordering {
    b.1.count
        .cmp(&a.1.count)
        .then(b.1.last_used.cmp(&a.1.last_used))
        .then(a.0.cmp(&b.0))
}

/// 内存里的补全历史（P5 起真源是用户数据目录的 `completion.table.yaml`）。
#[derive(Default)]
pub(crate) struct CompletionState {
    loaded: bool,
    map: std::collections::BTreeMap<(&'static str, String), CompletionRecord>,
    dirty: bool,
    /// 每次改动 +1：落盘在锁外做，写完时代次没变才算「干净」。
    generation: u64,
    /// 文件读不了（IO 错误，如被占用）：本次运行不写回，免得盖掉读不到的原文件。解析失败不走
    /// 这里——机写文件坏了从 `.bak` 自愈或另存后重建（§2.5）。
    unreadable: bool,
    /// 最近一次读到 / 写出的文件指纹：重载时据此分辨文件是不是被手改过。
    fp: Option<String>,
}

fn file_fp(p: &std::path::Path) -> Option<String> {
    wind_userdata::text::fold::read_text(p)
        .ok()
        .map(|c| wind_userdata::text::fold::fingerprint(c.as_ref().map(|(b, _)| b.as_slice())))
}

pub(crate) const COMPLETION_COLUMNS: [&str; 4] = ["kind", "text", "count", "last_used"];

type CompletionMap = std::collections::BTreeMap<(&'static str, String), CompletionRecord>;

/// 解析补全表：坏行（未知类别、空文本、数字坏）跳过并计数。
fn parse_completion_table(
    bytes: &[u8],
    path: &std::path::Path,
) -> anyhow::Result<(CompletionMap, usize)> {
    let doc = wind_userdata::text::table::parse_table(bytes, path)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut map = CompletionMap::new();
    let mut bad = 0;
    for r in &doc.rows {
        let kind = CompletionKind::from_key(doc.get(r, "kind"));
        let text = doc.get(r, "text");
        let count = doc.get(r, "count").trim().parse::<u32>();
        let last = doc.get(r, "last_used").trim().parse::<i64>();
        match (kind, count, last) {
            (Some(k), Ok(count), Ok(last_used)) if !text.is_empty() => {
                map.entry((k.as_str(), text.to_string()))
                    .or_insert(CompletionRecord { count, last_used });
            }
            _ => bad += 1,
        }
    }
    Ok((map, bad))
}

/// 补全表的整文件渲染。
pub(crate) fn render_completion_table(map: &CompletionMap) -> String {
    let rows: Vec<Vec<String>> = map
        .iter()
        .map(|((k, t), r)| {
            vec![
                k.to_string(),
                t.clone(),
                r.count.to_string(),
                r.last_used.to_string(),
            ]
        })
        .collect();
    wind_userdata::text::table::render_table(&COMPLETION_COLUMNS, "completion", &rows)
}

impl Store {
    /// 补全历史：`<data>/completion.table.yaml`（P5 起随用户目录走）。
    pub fn completion_path(&self) -> std::path::PathBuf {
        self.user_text_dir().join("completion.table.yaml")
    }

    /// 取内存表（首次访问时从文件载入）。
    fn completions(&self) -> std::sync::MutexGuard<'_, CompletionState> {
        let mut g = self.completions.lock().unwrap_or_else(|e| e.into_inner());
        if !g.loaded {
            g.loaded = true;
            let path = self.completion_path();
            let parse = |b: &[u8]| parse_completion_table(b, &path);
            match crate::machine_file::load_machine_file(&path, &parse) {
                Ok(None) => {}
                Ok(Some((map, bad))) => {
                    wind_userdata::issues::report_parse(&path, bad);
                    g.map = map;
                }
                Err(e) => {
                    tracing::warn!("读不了补全历史文件：{e:#}（本次运行不写回）");
                    g.unreadable = true;
                }
            }
            g.fp = file_fp(&path);
        }
        g
    }

    /// 文件被手改 / 换掉（`dict.reload` 指名）：丢掉内存表，下次访问重读。
    ///
    /// 文件被手改过（指纹与最近一次读写的不同）⇒ 以文件为准，内存里未落盘的改动丢弃，不先落盘
    /// 盖掉手改。
    pub fn reload_completions(&self) -> bool {
        let edited = {
            let g = self.completions.lock().unwrap_or_else(|e| e.into_inner());
            g.loaded && g.fp != file_fp(&self.completion_path())
        };
        if edited {
            tracing::info!("补全历史文件被改过，以文件为准重新载入");
        } else if let Err(e) = self.flush_completions() {
            tracing::warn!("补全历史落盘失败：{e:#}");
        }
        let mut g = self.completions.lock().unwrap_or_else(|e| e.into_inner());
        *g = CompletionState::default();
        true
    }

    /// 内存表有改动就整文件原子写一次（去抖落盘线程、`Store` 释放、`dict.flush` 调）。
    ///
    /// 数据锁只在取快照时短暂持有：写盘（两次 fsync，机械盘上可达上百毫秒）在锁外做，按键路径的
    /// 读写不陪等。并发的两次落盘由 `completion_io` 串行。
    pub(crate) fn flush_completions(&self) -> anyhow::Result<()> {
        let _io = self.completion_io.lock().unwrap_or_else(|e| e.into_inner());
        let (bytes, generation) = {
            let g = self.completions.lock().unwrap_or_else(|e| e.into_inner());
            if !g.dirty || g.unreadable {
                return Ok(());
            }
            (Self::completion_bytes(&g)?, g.generation)
        };
        wind_userdata::text::fold::commit_text(&self.completion_path(), &bytes, None)?;
        let mut g = self.completions.lock().unwrap_or_else(|e| e.into_inner());
        g.fp = Some(wind_userdata::text::fold::fingerprint(Some(&bytes)));
        if g.generation == generation {
            g.dirty = false;
        }
        Ok(())
    }

    fn completion_bytes(g: &CompletionState) -> anyhow::Result<Vec<u8>> {
        Ok(render_completion_table(&g.map).into_bytes())
    }

    /// 记录一次使用：count++、last_used=now（只改内存，去抖落盘）。
    ///
    /// 空文本与超长文本**静默忽略**（返回 `Ok(false)`）：调用点在上屏路径上，那里为一条
    /// 学习数据报错没有任何可做的补救，而把畸形数据写进去会一直留在补全列表里。
    pub fn record_completion(&self, kind: CompletionKind, text: &str) -> anyhow::Result<bool> {
        if text.is_empty() || text.chars().count() > COMPLETION_TEXT_MAX_CHARS {
            return Ok(false);
        }
        let now = now_secs();
        let mut g = self.completions();
        let r = g
            .map
            .entry((kind.as_str(), text.to_string()))
            .or_insert(CompletionRecord {
                count: 0,
                last_used: 0,
            });
        r.count = r.count.saturating_add(1);
        r.last_used = now;
        g.dirty = true;
        g.generation += 1;
        Ok(true)
    }

    pub fn get_completion(
        &self,
        kind: CompletionKind,
        text: &str,
    ) -> anyhow::Result<Option<CompletionRecord>> {
        Ok(self
            .completions()
            .map
            .get(&(kind.as_str(), text.to_string()))
            .copied())
    }

    /// 列举某 kind 下以 `prefix` 开头的记录，已按 [`better`] 排好序。
    ///
    /// `limit = 0` 表示不限。返回 `(本页, 该 kind 下命中前缀的总数)`。
    ///
    /// ⚠️ 前缀匹配是**字面**的，不做大小写归一——归一属于消费端的策略（网址补全要
    /// 不区分大小写，而用户自定义的邮箱后缀大小写可能有意义）。
    pub fn list_completions(
        &self,
        kind: CompletionKind,
        prefix: &str,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<(String, CompletionRecord)>, usize)> {
        let k = kind.as_str();
        let mut all: Vec<(String, CompletionRecord)> = self
            .completions()
            .map
            .range((k, prefix.to_string())..)
            .take_while(|((kk, t), _)| *kk == k && t.starts_with(prefix))
            .map(|((_, t), r)| (t.clone(), *r))
            .collect();
        all.sort_by(better);
        let total = all.len();
        let page: Vec<_> = all
            .into_iter()
            .skip(offset)
            .take(if limit == 0 { usize::MAX } else { limit })
            .collect();
        Ok((page, total))
    }

    /// 删一条。返回它本来在不在。
    pub fn remove_completion(&self, kind: CompletionKind, text: &str) -> anyhow::Result<bool> {
        let mut g = self.completions();
        let existed = g.map.remove(&(kind.as_str(), text.to_string())).is_some();
        g.dirty |= existed;
        g.generation += 1;
        Ok(existed)
    }

    /// 一批 [`Self::remove_completion`]：一次取锁。返回本来在的条数。
    pub fn remove_completions(
        &self,
        kind: CompletionKind,
        texts: &[String],
    ) -> anyhow::Result<usize> {
        if texts.is_empty() {
            return Ok(0);
        }
        let mut g = self.completions();
        let mut n = 0;
        for t in texts {
            n += usize::from(g.map.remove(&(kind.as_str(), t.clone())).is_some());
        }
        g.dirty |= n > 0;
        g.generation += 1;
        Ok(n)
    }

    /// 清空某 kind 的全部记录，返回删除条数。
    ///
    /// **只清这一类**：用户在设置里点「清空网址历史」不该顺带把邮箱后缀学习也抹掉。
    pub fn clear_completions(&self, kind: CompletionKind) -> anyhow::Result<usize> {
        let mut g = self.completions();
        let before = g.map.len();
        g.map.retain(|(k, _), _| *k != kind.as_str());
        let removed = before - g.map.len();
        g.dirty |= removed > 0;
        g.generation += 1;
        Ok(removed)
    }

    /// 裁剪到只保留排序最靠前的 `max` 条，返回删除条数。`max = 0` 表示不限（不裁剪）。
    ///
    /// 网址历史是**无界**的，没有这道闸它会一直长。裁剪按与补全展示**同一个排序**
    /// （[`better`]）取舍，被裁掉的一定是补全里最靠后、用户最不可能选到的那些。
    ///
    /// # ★ `keep` 存在的理由：不加它，表一满就再也学不进新东西
    ///
    /// 调用方的顺序是「先记一条，再裁到上限」，而新记的那条 `count` 恒为 1，在 [`better`]
    /// 的首要判据（次数降序）里排在所有 `count ≥ 2` 的老条目**之后**。表里一旦攒够 `max`
    /// 条老条目，每次上屏都是「写进去、当场被自己这次裁剪删掉」——历史从此冻结。`keep`
    /// 把本次刚写入的那条提到排序最前，使它必定在保留集内。
    pub fn prune_completions(
        &self,
        kind: CompletionKind,
        max: usize,
        keep: Option<&str>,
    ) -> anyhow::Result<usize> {
        if max == 0 {
            return Ok(0);
        }
        let (mut all, total) = self.list_completions(kind, "", 0, 0)?;
        if total <= max {
            return Ok(0);
        }
        // 把 `keep` 提到队首再切，保留集恰好仍是 `max` 条。
        if let Some(k) = keep
            && let Some(pos) = all.iter().position(|(text, _)| text == k)
        {
            let item = all.remove(pos);
            all.insert(0, item);
        }
        let mut g = self.completions();
        let mut removed = 0usize;
        for (text, _) in all.into_iter().skip(max) {
            if g.map.remove(&(kind.as_str(), text)).is_some() {
                removed += 1;
            }
        }
        g.dirty |= removed > 0;
        g.generation += 1;
        Ok(removed)
    }

    /// 导出**全部** kind 为 jsonl（每行 `{"kind","text","count","last_used"}`）。
    ///
    /// 不按 kind 分文件：备份包里多一个文件就多一处要在 `create_backup` /
    /// `restore_backup` / `RESTORE_SECTIONS` 三处同步登记的地方。
    pub fn export_completions_jsonl(&self) -> anyhow::Result<String> {
        let mut out = String::new();
        for kind in CompletionKind::ALL {
            let (rows, _) = self.list_completions(kind, "", 0, 0)?;
            for (text, rec) in rows {
                out.push_str(&serde_json::to_string(&serde_json::json!({
                    "kind": kind.as_str(),
                    "text": text,
                    "count": rec.count,
                    "last_used": rec.last_used,
                }))?);
                out.push('\n');
            }
        }
        Ok(out)
    }

    /// 从 jsonl 导入。已存在的条目取 `max(count)` / `max(last_used)`。导入完立即落盘。
    ///
    /// 返回 `(imported, skipped)`。非法行、未知 kind、超长文本都计入 skipped 而不报错
    /// ——备份还原时为一条坏记录中断整个还原，代价远大于丢掉那一条。
    pub fn import_completions_jsonl(&self, text: &str) -> anyhow::Result<(usize, usize)> {
        let normalized = wind_utils::text::normalize_input(text);
        let text = normalized.as_ref();
        let mut skipped = 0usize;
        let mut imported = 0usize;
        {
            let mut g = self.completions();
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    skipped += 1;
                    continue;
                };
                let kind = v
                    .get("kind")
                    .and_then(|x| x.as_str())
                    .and_then(CompletionKind::from_key);
                let word = v.get("text").and_then(|x| x.as_str());
                let (Some(kind), Some(word)) = (kind, word) else {
                    skipped += 1;
                    continue;
                };
                if word.is_empty() || word.chars().count() > COMPLETION_TEXT_MAX_CHARS {
                    skipped += 1;
                    continue;
                }
                let count = v.get("count").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
                let last_used = v.get("last_used").and_then(|x| x.as_i64()).unwrap_or(0);
                let r =
                    g.map
                        .entry((kind.as_str(), word.to_string()))
                        .or_insert(CompletionRecord {
                            count: 0,
                            last_used: 0,
                        });
                r.count = r.count.max(count);
                r.last_used = r.last_used.max(last_used);
                g.dirty = true;
                g.generation += 1;
                imported += 1;
            }
        }
        self.flush_completions()?;
        Ok((imported, skipped))
    }

    /// 旧 `COMPLETION` 表全部行（P4a 迁移用）。
    pub(crate) fn legacy_completion_rows(
        &self,
    ) -> anyhow::Result<Vec<(CompletionKind, String, CompletionRecord)>> {
        self.with_db(|db| {
            use redb::ReadableTable;
            let txn = db.begin_read()?;
            let t = txn.open_table(COMPLETION)?;
            let mut out = Vec::new();
            for item in t.iter()? {
                let (k, v) = item?;
                let Some((kind, text)) = k.value().split_once('\u{0}') else {
                    continue;
                };
                if let (Some(kind), Some(rec)) =
                    (CompletionKind::from_key(kind), dec_rec(v.value()))
                {
                    out.push((kind, text.to_string(), rec));
                }
            }
            Ok(out)
        })
    }

    /// 测试 / 探针：往旧 `COMPLETION` 表写一批（模拟升级前的库；redb 页缓存机制测试也拿它做负载，
    /// P4b 随 redb 一起删）。单写事务。
    #[doc(hidden)]
    pub fn legacy_put_completions(
        &self,
        rows: &[(CompletionKind, String, u32, i64)],
    ) -> anyhow::Result<()> {
        self.with_db(|db| {
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(COMPLETION)?;
                for (kind, text, count, last_used) in rows {
                    t.insert(
                        enc_key(*kind, text).as_str(),
                        enc_rec(*count, *last_used).as_slice(),
                    )?;
                }
            }
            txn.commit()?;
            Ok(())
        })
    }

    /// redb 页缓存机制测试的读负载：扫旧 `COMPLETION` 表里 kind 下以 `prefix` 开头的文本（`limit`
    /// 0 = 不限）。P4b 随 redb 一起删。
    #[doc(hidden)]
    pub fn legacy_scan_completions(
        &self,
        kind: CompletionKind,
        prefix: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let scan = format!("{}{}", scan_prefix(kind), prefix);
        let strip = scan_prefix(kind).len();
        self.with_db(|db| {
            let txn = db.begin_read()?;
            let t = txn.open_table(COMPLETION)?;
            let mut out = Vec::new();
            for item in t.range(scan.as_str()..)? {
                let (k, _) = item?;
                let key = k.value();
                if !key.starts_with(&scan) {
                    break;
                }
                out.push(key[strip..].to_string());
                if limit > 0 && out.len() >= limit {
                    break;
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试独立文件：redb 是单写者，共用文件会让并发测试互相阻塞。
    fn store(tag: &str) -> Store {
        let p = std::env::temp_dir().join(format!(
            "wind_completion_test_{}_{}_{:?}.redb",
            std::process::id(),
            tag,
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        Store::open(&p).unwrap()
    }

    fn reopen(tag: &str, s: Store) -> Store {
        drop(s);
        let p = std::env::temp_dir().join(format!(
            "wind_completion_test_{}_{}_{:?}.redb",
            std::process::id(),
            tag,
            std::thread::current().id()
        ));
        Store::open(&p).unwrap()
    }

    fn has(s: &Store, kind: CompletionKind, text: &str) -> bool {
        s.get_completion(kind, text).unwrap().is_some()
    }

    /// dict.reload 指名补全文件：手改的内容生效，不被内存里未落盘的旧版本盖掉。
    #[test]
    fn reload_keeps_hand_edit_even_with_unflushed_changes() {
        let s = store("reload_edit");
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        s.flush_completions().unwrap();
        s.record_completion(CompletionKind::EmailSuffix, "163.com")
            .unwrap(); // 未落盘
        let p = s.completion_path();
        let edited = std::fs::read_to_string(&p)
            .unwrap()
            .replace("qq.com", "hand.example");
        std::fs::write(&p, &edited).unwrap();
        assert!(s.reload_completions());
        assert!(has(&s, CompletionKind::EmailSuffix, "hand.example"));
        assert!(!has(&s, CompletionKind::EmailSuffix, "qq.com"));
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            edited,
            "手改的文件没被盖掉"
        );
    }

    /// M-2 / §2.5 机写文件：坏了从 `.bak` 自愈，之后照常学习、照常落盘。
    #[test]
    fn corrupt_completion_self_heals_from_bak() {
        let s = store("heal");
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        s.flush_completions().unwrap();
        s.record_completion(CompletionKind::EmailSuffix, "163.com")
            .unwrap();
        s.flush_completions().unwrap(); // .bak = 只有 qq.com 的那一版
        let p = s.completion_path();
        let s = reopen("heal", s);
        std::fs::write(&p, b"\xff garbage").unwrap();
        let s = reopen("heal", s);
        assert!(has(&s, CompletionKind::EmailSuffix, "qq.com"));
        s.record_completion(CompletionKind::EmailSuffix, "gmail.com")
            .unwrap();
        s.flush_completions().unwrap();
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("gmail.com") && txt.contains("qq.com"), "{txt}");
    }

    #[test]
    fn completion_without_good_bak_is_kept_aside_and_rebuilt() {
        let s = store("rebuild");
        let p = s.completion_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"\xff garbage").unwrap();
        s.record_completion(CompletionKind::UrlHistory, "example.com")
            .unwrap();
        s.flush_completions().unwrap();
        let kept = std::fs::read_dir(s.user_text_dir().join(".backup"))
            .unwrap()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("completion.table.yaml.corrupt-")
            });
        assert!(kept);
        assert!(std::fs::read_to_string(&p).unwrap().contains("example.com"));
    }

    #[test]
    fn completion_file_is_table_text_in_data_dir() {
        let s = store("table");
        s.record_completion(CompletionKind::QuickHistory, "a\tb")
            .unwrap();
        s.flush_completions().unwrap();
        let p = s.completion_path();
        assert!(p.ends_with("completion.table.yaml"));
        assert!(p.starts_with(s.user_text_dir()));
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("kind: completion"), "{txt}");
        assert!(txt.contains("quick_history\ta\\tb\t1\t"), "{txt}");
        let s = reopen("table", s);
        assert!(has(&s, CompletionKind::QuickHistory, "a\tb"));
    }

    #[test]
    fn completion_bad_rows_are_skipped_and_counted() {
        let s = store("badrow");
        let p = s.completion_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "---\ncolumns: [kind, text, count, last_used]\nwind: {kind: completion, escape: true}\n...\n# no comment\nemail_suffix\tqq.com\t2\t1\nnope\tx\t1\t1\nemail_suffix\ty.com\tz\t1\n",
        )
        .unwrap();
        assert!(has(&s, CompletionKind::EmailSuffix, "qq.com"));
        assert_eq!(
            wind_userdata::issues::list_under(&p),
            vec![wind_userdata::issues::FileIssue {
                path: p.clone(),
                kind: wind_userdata::issues::IssueKind::BadLines(2)
            }]
        );
        wind_userdata::issues::clear(&p);
    }

    #[test]
    fn record_accumulates_count() {
        let s = store("acc");
        assert_eq!(
            s.get_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap(),
            None
        );

        assert!(
            s.record_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap()
        );
        assert!(
            s.record_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap()
        );
        let rec = s
            .get_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap()
            .expect("记过两次应查得到");
        assert_eq!(rec.count, 2, "同一条重复记录应累加而非新增");
        assert!(rec.last_used > 0, "last_used 应被填上");
    }

    #[test]
    fn kinds_do_not_leak_into_each_other() {
        let s = store("kinds");
        s.record_completion(CompletionKind::EmailSuffix, "same.text")
            .unwrap();
        s.record_completion(CompletionKind::UrlHistory, "same.text")
            .unwrap();

        // 同一条文本在两个 kind 下各有独立计数。
        s.record_completion(CompletionKind::UrlHistory, "same.text")
            .unwrap();
        assert_eq!(
            s.get_completion(CompletionKind::EmailSuffix, "same.text")
                .unwrap()
                .unwrap()
                .count,
            1
        );
        assert_eq!(
            s.get_completion(CompletionKind::UrlHistory, "same.text")
                .unwrap()
                .unwrap()
                .count,
            2
        );

        // 清一类不影响另一类——设置里「清空网址历史」不该抹掉邮箱学习。
        assert_eq!(s.clear_completions(CompletionKind::UrlHistory).unwrap(), 1);
        assert_eq!(
            s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
                .unwrap()
                .1,
            0
        );
        assert_eq!(
            s.list_completions(CompletionKind::EmailSuffix, "", 0, 0)
                .unwrap()
                .1,
            1
        );
    }

    #[test]
    fn list_filters_by_prefix_and_sorts_by_count() {
        let s = store("list");
        for _ in 0..3 {
            s.record_completion(CompletionKind::UrlHistory, "www.b.com")
                .unwrap();
        }
        s.record_completion(CompletionKind::UrlHistory, "www.a.com")
            .unwrap();
        s.record_completion(CompletionKind::UrlHistory, "bbs.c.com")
            .unwrap();

        // 前缀过滤：bbs. 那条不在 www. 的结果里。
        let (rows, total) = s
            .list_completions(CompletionKind::UrlHistory, "www.", 0, 0)
            .unwrap();
        assert_eq!(total, 2, "www. 前缀应命中两条，实际 {:?}", rows);
        // 次数多的在前，与字典序相反 —— 证明排的是频次而不是 redb 的返回序。
        assert_eq!(rows[0].0, "www.b.com");
        assert_eq!(rows[1].0, "www.a.com");

        // 空前缀 = 全部。
        assert_eq!(
            s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
                .unwrap()
                .1,
            3
        );
    }

    #[test]
    fn list_paginates() {
        let s = store("page");
        for t in ["a", "b", "c"] {
            s.record_completion(CompletionKind::UrlHistory, t).unwrap();
        }
        let (page, total) = s
            .list_completions(CompletionKind::UrlHistory, "", 1, 1)
            .unwrap();
        assert_eq!(total, 3, "total 是命中总数而非本页条数");
        assert_eq!(page.len(), 1);
    }

    #[test]
    fn remove_one_leaves_the_rest() {
        let s = store("rm");
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        s.record_completion(CompletionKind::EmailSuffix, "163.com")
            .unwrap();

        assert!(
            s.remove_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap()
        );
        assert!(
            !s.remove_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap(),
            "删第二次应返回 false 而不是报错"
        );
        assert_eq!(
            s.list_completions(CompletionKind::EmailSuffix, "", 0, 0)
                .unwrap()
                .1,
            1
        );
    }

    #[test]
    fn prune_keeps_the_top_entries_by_the_display_order() {
        let s = store("prune");
        // hot 记 3 次、mid 2 次、cold 1 次 —— 裁剪应按补全展示的同一排序取舍。
        for (text, n) in [("hot", 3), ("mid", 2), ("cold", 1)] {
            for _ in 0..n {
                s.record_completion(CompletionKind::UrlHistory, text)
                    .unwrap();
            }
        }
        assert_eq!(
            s.prune_completions(CompletionKind::UrlHistory, 2, None)
                .unwrap(),
            1
        );
        let (rows, total) = s
            .list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["hot", "mid"]
        );

        // 未超上限时不动任何东西；max=0 表示不限。
        assert_eq!(
            s.prune_completions(CompletionKind::UrlHistory, 5, None)
                .unwrap(),
            0
        );
        assert_eq!(
            s.prune_completions(CompletionKind::UrlHistory, 0, None)
                .unwrap(),
            0
        );
        assert_eq!(
            s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
                .unwrap()
                .1,
            2
        );
    }

    /// `keep` 保护刚写入的那条 —— 没有它，表一满就再也学不进新东西。
    ///
    /// 新条目 `count` 恒为 1，在次数降序里排在所有老条目之后，于是调用方「先记再裁」
    /// 的顺序会让它**当场被自己这次裁剪删掉**。这条钉的就是那个结构性缺陷。
    #[test]
    fn prune_keeps_the_just_written_entry() {
        let s = store("prunekeep");
        for (text, n) in [("old_hot", 5), ("old_mid", 3)] {
            for _ in 0..n {
                s.record_completion(CompletionKind::UrlHistory, text)
                    .unwrap();
            }
        }
        // 模拟「上屏一条新网址」：count=1，排序上垫底。
        s.record_completion(CompletionKind::UrlHistory, "fresh")
            .unwrap();

        assert_eq!(
            s.prune_completions(CompletionKind::UrlHistory, 2, Some("fresh"))
                .unwrap(),
            1
        );
        let (rows, total) = s
            .list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap();
        assert_eq!(total, 2, "保留集仍恰好是 max 条，实际 {rows:?}");
        let kept: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert!(
            kept.contains(&"fresh"),
            "刚写入的那条必须留下，实际 {kept:?}"
        );
        assert!(
            kept.contains(&"old_hot"),
            "最热的那条也该留下，实际 {kept:?}"
        );
        assert!(
            !kept.contains(&"old_mid"),
            "被挤掉的应是「除新条目外最冷的」"
        );

        // 不传 keep 时行为不变（批量维护等调用方仍走纯 LFU）。
        s.record_completion(CompletionKind::UrlHistory, "fresh2")
            .unwrap();
        s.prune_completions(CompletionKind::UrlHistory, 2, None)
            .unwrap();
        let (rows2, _) = s
            .list_completions(CompletionKind::UrlHistory, "", 0, 0)
            .unwrap();
        assert!(
            !rows2.iter().any(|r| r.0 == "fresh2"),
            "keep=None 时新条目照旧垫底被裁，实际 {rows2:?}"
        );
    }

    #[test]
    fn oversized_and_empty_text_are_ignored() {
        let s = store("guard");
        assert!(!s.record_completion(CompletionKind::UrlHistory, "").unwrap());
        let long = "x".repeat(COMPLETION_TEXT_MAX_CHARS + 1);
        assert!(
            !s.record_completion(CompletionKind::UrlHistory, &long)
                .unwrap()
        );
        assert_eq!(
            s.list_completions(CompletionKind::UrlHistory, "", 0, 0)
                .unwrap()
                .1,
            0
        );

        // 恰好等于上限的仍收。
        let ok = "x".repeat(COMPLETION_TEXT_MAX_CHARS);
        assert!(
            s.record_completion(CompletionKind::UrlHistory, &ok)
                .unwrap()
        );
    }

    #[test]
    fn jsonl_roundtrip_carries_both_kinds() {
        let a = store("exp");
        a.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        a.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        a.record_completion(CompletionKind::UrlHistory, "www.x.com")
            .unwrap();
        let dump = a.export_completions_jsonl().unwrap();
        assert_eq!(dump.lines().count(), 2, "两个 kind 各一条:\n{dump}");

        let b = store("imp");
        let (imported, skipped) = b.import_completions_jsonl(&dump).unwrap();
        assert_eq!((imported, skipped), (2, 0));
        assert_eq!(
            b.get_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap()
                .unwrap()
                .count,
            2,
            "count 应随导出导入保住"
        );
        assert_eq!(
            b.list_completions(CompletionKind::UrlHistory, "", 0, 0)
                .unwrap()
                .1,
            1
        );
    }

    /// 每个 kind 都要能经导出 / 导入往返——遍历 `ALL` 而不是手写，新增 kind 自动纳入。
    /// 曾经导出循环是手写的两项列表，新加一类不改它，那一类就静默不进备份。
    #[test]
    fn jsonl_roundtrip_covers_every_kind() {
        let a = store("exp_all");
        for kind in CompletionKind::ALL {
            a.record_completion(kind, "Same_Text").unwrap();
        }
        let dump = a.export_completions_jsonl().unwrap();
        let b = store("imp_all");
        b.import_completions_jsonl(&dump).unwrap();
        for kind in CompletionKind::ALL {
            assert!(
                b.get_completion(kind, "Same_Text").unwrap().is_some(),
                "{kind:?} 没随导出导入保住:\n{dump}"
            );
            assert_eq!(CompletionKind::from_key(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn import_merges_by_max_and_skips_junk() {
        let s = store("merge");
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap(); // count=1

        let jsonl = concat!(
            r#"{"kind":"email_suffix","text":"qq.com","count":9,"last_used":100}"#,
            "\n",
            r#"not json"#,
            "\n",
            r#"{"kind":"no_such_kind","text":"x","count":1}"#,
            "\n",
            r#"{"kind":"email_suffix","count":1}"#,
            "\n",
        );
        let (imported, skipped) = s.import_completions_jsonl(jsonl).unwrap();
        assert_eq!(imported, 1);
        assert_eq!(skipped, 3, "坏行/未知 kind/缺 text 各跳一条");
        assert_eq!(
            s.get_completion(CompletionKind::EmailSuffix, "qq.com")
                .unwrap()
                .unwrap()
                .count,
            9,
            "已存在的条目取 max(count)"
        );
    }

    #[test]
    fn kind_str_roundtrip_is_closed() {
        for k in [CompletionKind::EmailSuffix, CompletionKind::UrlHistory] {
            assert_eq!(CompletionKind::from_key(k.as_str()), Some(k));
        }
        assert_eq!(CompletionKind::from_key("emial_suffix"), None);
    }

    /// 按键路径读写都只碰内存（redb 暂停照样补全、记录、裁剪）。
    #[test]
    fn completion_hot_path_does_not_touch_redb() {
        let s = store("hot");
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        s.pause().unwrap();
        assert!(
            s.record_completion(CompletionKind::EmailSuffix, "163.com")
                .unwrap()
        );
        assert_eq!(
            s.list_completions(CompletionKind::EmailSuffix, "", 0, 0)
                .unwrap()
                .1,
            2
        );
        assert_eq!(
            s.prune_completions(CompletionKind::EmailSuffix, 1, None)
                .unwrap(),
            1
        );
        s.resume().unwrap();
    }

    /// 不起落盘线程，释放 Store 时同步落盘，重开后记录在。
    #[test]
    fn completion_survives_drop_without_flusher() {
        let p = std::env::temp_dir().join(format!("wind_cmp_drop_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let s = Store::open(&p).unwrap();
            s.record_completion(CompletionKind::UrlHistory, "example.com")
                .unwrap();
            s.record_completion(CompletionKind::UrlHistory, "example.com")
                .unwrap();
        }
        let s = Store::open(&p).unwrap();
        let rec = s
            .get_completion(CompletionKind::UrlHistory, "example.com")
            .unwrap()
            .unwrap();
        assert_eq!(rec.count, 2);
        assert!(s.completion_path().is_file());
    }

    /// 审查 I-2：落盘（含 fsync）期间不持数据锁——按键路径的 list / record 不被一次写盘卡住。
    #[test]
    fn flush_does_not_block_readers() {
        let s = std::sync::Arc::new(store("flush_nonblock"));
        s.record_completion(CompletionKind::EmailSuffix, "qq.com")
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let rx = std::sync::Mutex::new(rx);
        let flusher = std::sync::Arc::new(std::sync::Mutex::new(None));
        let f2 = flusher.clone();
        let hook = wind_userdata::text::fold::add_before_rename_hook(Box::new(move || {
            if Some(std::thread::current().id()) == *f2.lock().unwrap() {
                // 模拟一次很慢的 fsync：等主线程读完再放行（最多 2 秒）
                let _ = rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2));
            }
        }));
        let s2 = s.clone();
        let fl = flusher.clone();
        let h = std::thread::spawn(move || {
            *fl.lock().unwrap() = Some(std::thread::current().id());
            s2.flush_small_files().unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let t = std::time::Instant::now();
        let n = s
            .list_completions(CompletionKind::EmailSuffix, "", 0, 0)
            .unwrap()
            .1;
        let waited = t.elapsed();
        let _ = tx.send(());
        h.join().unwrap();
        drop(hook);
        assert_eq!(n, 1);
        assert!(
            waited < std::time::Duration::from_millis(500),
            "读等了 {waited:?}"
        );
    }
}
