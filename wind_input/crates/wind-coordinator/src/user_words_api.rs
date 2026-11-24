//! 用户词管理（列表 / 加词 / 删词 / 清空）的存储域逻辑，桌面设置页与移动端门面共用。
//!
//! # 为什么在这里而不在 wind-webdata
//!
//! Android 闭包不含 wind-webdata（它带 C 依赖，见本 crate Cargo.toml 的注释），移动端
//! 又不能照抄一份（ü 归一、边界求解、搜索语义会两边各改各的），故把只依赖
//! [`WebDataHost`] 窄面的逻辑下沉到这里，两端各自委托。
//!
//! # 边界：只管存储域
//!
//! 收发真实文本与扁平码（`nihao` + boundary bitmask）。显示域投影（转义、JSON 形状）各端
//! 自己做；带音节空格的显示码用 [`display_code`] 得到。
//!
//! # 不在这里做的
//!
//! - 只读模式拒写：桌面只读时 `user_store()` 为 `None`，写口自然报错；移动端门面自己先查
//!   `storage_readonly_reason()`。
//! - 页缓存回收：属于 RPC 出口的策略（wind-webdata 的 `web_data_rpc`）。

use wind_store::user_words::UserWordRecord;

use crate::web_host::WebDataHost;

/// 列表的排序字段。排序是跨页全局的：给了它就要全量物化后再切页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserWordSort {
    Code,
    Text,
    Weight,
}

/// 一页用户词（存储域记录）与全表命中总数。
#[derive(Debug, Clone, Default)]
pub struct UserWordListing {
    pub items: Vec<UserWordRecord>,
    /// 命中总数（不是本页条数）：分页器显示「共 N 条」要的是它，故三条分流都扫满全表。
    pub total: usize,
}

/// 存储域记录 → 带音节空格的显示码（`nihao` + `{0,2}` → `ni hao`）。
///
/// 码表码 boundary 恒 0 → 原样返回。反方向由 [`normalize_add_code`] / [`remove`] 拆回扁平码，
/// 所以把这个串原样回传给加词 / 删词是安全的。
pub fn display_code(r: &UserWordRecord) -> String {
    wind_store::wdict::join_code_by_boundary(&r.code, r.boundary)
}

/// 分页列出 `schema_id`（未折叠的方案 id，内部按数据方案折叠）的用户词。
///
/// `query` 为空 = 浏览；非空 = 搜索（词条内容包含 / 编码前缀 / 编码中段，见函数体）。
/// `query` 接受带空格的显示码（用户会照着列表抄）。
pub fn list_paged<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    query: &str,
    sort: Option<(UserWordSort, bool)>,
    offset: usize,
    limit: usize,
) -> anyhow::Result<UserWordListing> {
    let schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    // 编码前缀须拆成扁平码并做 ü 归一，与写入侧 `normalize_add_code` 同域，否则搜 `xv`
    // 找不到已落成 `xu` 的词（见 `docs/design/pinyin-code-domains.md`）。词条内容搜索仍用原串。
    let (code_prefix, _) = wind_store::wdict::split_spaced_code(query);
    let code_prefix = normalize_pinyin_code(h, &schema, code_prefix);

    // 不搜也不排序（最常走的路）：按 key 序流式取一页，只物化 `limit` 条。仍须扫完整表
    // 以得到精确 `total`，省的是物化不是扫描。
    if query.is_empty() && sort.is_none() {
        let mut items = Vec::new();
        let mut total = 0usize;
        store.for_each_user_word(&schema, &code_prefix, &mut |w| {
            if total >= offset && items.len() < limit {
                items.push(w.to_record());
            }
            total += 1;
            true
        })?;
        return Ok(UserWordListing { items, total });
    }

    let mut all = if query.is_empty() {
        // 要排序：排序是跨页全局的，只能全量物化。
        store.search_user_words_prefix(&schema, &code_prefix, 0)?
    } else {
        // 有搜索词：一遍扫描分两组，编码前缀命中在前；其余组收词条内容包含（用原串）
        // 与编码中段包含（用扁平码，用户不知道搜索框只认前缀）。两组互斥，无需去重。
        let q = query.to_lowercase();
        let code_q = code_prefix.to_lowercase();
        let mut head = Vec::new();
        let mut tail = Vec::new();
        store.for_each_user_word(&schema, "", &mut |w| {
            if w.code.starts_with(code_prefix.as_str()) {
                head.push(w.to_record());
            } else if w.text.to_lowercase().contains(&q)
                || (!code_q.is_empty() && w.code.to_lowercase().contains(&code_q))
            {
                tail.push(w.to_record());
            }
            true
        })?;
        head.append(&mut tail);
        head
    };
    let total = all.len();
    // 有排序时在切片前排序，实现跨页全局排序
    if let Some((by, desc)) = sort {
        all.sort_by(|a, b| {
            let ord = match by {
                UserWordSort::Weight => a.weight.cmp(&b.weight),
                UserWordSort::Text => a.text.cmp(&b.text),
                UserWordSort::Code => a.code.cmp(&b.code),
            };
            if desc { ord.reverse() } else { ord }
        });
    }
    let items = all.into_iter().skip(offset).take(limit).collect();
    Ok(UserWordListing { items, total })
}

/// 手动加词（已存在则按 store 的合并语义：权重取 max、旧边界为 0 时补上）。
///
/// `text` 是存储域真实文本（转义形态须由调用方先还原），`code` 可带音节空格。落库会推进
/// 该方案的结构代次，引擎据此失效候选缓存，加完立刻打得出。
pub fn add<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    code: &str,
    text: &str,
    weight: i32,
) -> anyhow::Result<()> {
    let schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    let (code, boundary) = normalize_add_code(h, &schema, code, text);
    store.add_user_word(&schema, &code, text, weight, boundary)?;
    clear_deleted_for_added(h, &schema, &[(&code, text)]);
    Ok(())
}

/// 一批 [`add`]（`(code, text, weight)`，同方案）：编码规范化与边界求解整批一次，落库一次
/// （`Store::add_user_words`：任一条过长整批不写），撤删除规则一次。
pub fn add_batch<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    rows: &[(String, String, i32)],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    let pairs: Vec<(&str, &str)> = rows
        .iter()
        .map(|(c, t, _)| (c.as_str(), t.as_str()))
        .collect();
    let words: Vec<(String, String, i32, u64)> = normalize_add_codes(h, &schema, &pairs)
        .into_iter()
        .zip(rows)
        .map(|((code, boundary), (_, text, weight))| (code, text.clone(), *weight, boundary))
        .collect();
    store.add_user_words(&schema, &words)?;
    let added: Vec<(&str, &str)> = words
        .iter()
        .map(|(c, t, _, _)| (c.as_str(), t.as_str()))
        .collect();
    clear_deleted_for_added(h, &schema, &added);
    Ok(())
}

/// 单条显式加词（加词对话框、快捷加词、命令栏 `dict.add`、设置页加改词、单条手动转正）
/// 成功后的收口：撤掉同 (数据方案, 编码, 文本) 的候选调整删除规则。
///
/// `apply_shadow` 按文本过滤、不看来源，不撤的话删过再手动加回的词永远打不出来。
/// 自动学词 / 晋升不走这里（删过的词不该因再打到一次而复活）；批量导入、全部转正也不撤，
/// 改用 [`count_hidden_by_shadow`] 报数。
///
/// 规则的匹配范围见 [`delete_rule_buckets`] 与 [`delete_rule_key_forms`]。
///
/// 清理失败只记日志：词已经加进去了，不能因为规则文件被外部编辑占着就报加词失败。
pub fn clear_deleted_for_added<H: WebDataHost + ?Sized>(
    h: &H,
    data_schema: &str,
    words: &[(&str, &str)],
) {
    let Some(store) = h.user_store() else {
        return;
    };
    if words.is_empty() {
        return;
    }
    let key_forms = delete_rule_key_forms(data_schema);
    for b in delete_rule_buckets(h, data_schema) {
        match store.clear_deleted_for_words(&b, words, &key_forms) {
            Ok(0) => {}
            Ok(n) => tracing::debug!("加词：撤销 {n} 条删除规则 schema={b}"),
            Err(e) => tracing::warn!("加词：撤销删除规则失败 schema={b}: {e}"),
        }
    }
}

/// `words` 里有几条被候选调整的删除规则隐藏（只读，不撤）。批量导入 / 全部转正在结果里报这个数。
/// 匹配范围与 [`clear_deleted_for_added`] 相同；同一个词在几个桶、几个编码形态上都有规则只算一次。
/// 读规则失败按 0 计并记日志——计数是提示，不该让已完成的导入报错。
pub fn count_hidden_by_shadow<H: WebDataHost + ?Sized>(
    h: &H,
    data_schema: &str,
    words: &[(&str, &str)],
) -> usize {
    let Some(store) = h.user_store() else {
        return 0;
    };
    if words.is_empty() {
        return 0;
    }
    let key_forms = delete_rule_key_forms(data_schema);
    let mut hidden = std::collections::HashSet::new();
    for b in delete_rule_buckets(h, data_schema) {
        match store.words_hidden_by_shadow(&b, words, &key_forms) {
            Ok(set) => hidden.extend(set),
            Err(e) => tracing::warn!("统计被删除规则隐藏的词失败 schema={b}: {e}"),
        }
    }
    hidden.len()
}

/// 词条落到 `data_schema`（已折叠）时，可能隐藏它的删除规则所在的桶：它自己，加上已启用（及当前）
/// 的混输方案里以它为成员的——混输方案的规则记在混输自己的桶里、词却落主码表 / 拼音，用户可能在
/// 混输下删、切到纯码表后再加。
fn delete_rule_buckets<H: WebDataHost + ?Sized>(h: &H, data_schema: &str) -> Vec<String> {
    let em = h.engine_mgr();
    let mut buckets = vec![data_schema.to_string()];
    let mut schemas = em.available_schemas();
    schemas.push(em.active_schema_id());
    for s in &schemas {
        if em.schema_engine_type(s).as_deref() != Some("mixed") {
            continue;
        }
        let member = [
            wind_candidate::CandidateSource::CodeTable,
            wind_candidate::CandidateSource::Pinyin,
        ]
        .into_iter()
        .any(|src| em.write_data_schema_id(s, src).as_deref() == Some(data_schema));
        let b = em.data_schema_id(s);
        if member && !buckets.contains(&b) {
            buckets.push(b);
        }
    }
    buckets
}

/// 规则键（输入码，`Coordinator::shadow_code_of`）在词条码域里的候选形态，任一与词条码相同即命中。
///
/// 码表与词条码同形，原样比；双拼完整转换时键已是连写全拼。拼音桶的键先去掉手动分隔符 `'`，
/// 再补一个 ü 归一形态（全拼敲 `xv` / `lue` 删的词，词条码归一成 `xu` / `lve`）。ü 形态只在归一后
/// 能整段切成全拼音节时才补：双拼未完整转换时键是原始击键（`v` 是布局键），那类串归一后多半切不成
/// 全拼，不该据此命中。简拼、前缀等别的编码上的规则不算。
fn delete_rule_key_forms(data_schema: &str) -> impl Fn(&str) -> Vec<String> {
    let pinyin = data_schema == wind_engine::manager::PINYIN_DATA_SCHEMA;
    move |k: &str| {
        if !pinyin {
            return vec![k.to_string()];
        }
        let flat = k.replace('\'', "");
        let mut forms = Vec::with_capacity(2);
        if let std::borrow::Cow::Owned(u) = wind_engine::pinyin::spelling::normalize_u_umlaut(&flat)
            && wind_engine::pinyin::syllable::is_full_pinyin_code(&u, 0)
        {
            forms.push(u);
        }
        forms.push(flat);
        forms
    }
}

/// 删词。`code` 接受带音节空格的显示码，`text` 是存储域真实文本。不存在静默成功。
pub fn remove<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    code: &str,
    text: &str,
) -> anyhow::Result<()> {
    let schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    // 列表项的 code 带音节空格（见 `display_code`），而 key 是扁平的——不拆就删不掉。
    let (code, _) = wind_store::wdict::split_spaced_code(code);
    store.remove_user_word(&schema, &code, text)
}

/// 一批 [`remove`]（`(code, text)`，同方案）：落库一次。不存在的静默跳过。
pub fn remove_batch<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    keys: &[(String, String)],
) -> anyhow::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    let keys: Vec<(String, String)> = keys
        .iter()
        .map(|(code, text)| (wind_store::wdict::split_spaced_code(code).0, text.clone()))
        .collect();
    store.remove_user_words(&schema, &keys)
}

/// 清空该方案（折叠后的数据方案）的全部用户词，返回删除条数。
pub fn clear<H: WebDataHost + ?Sized>(h: &H, schema_id: &str) -> anyhow::Result<usize> {
    let schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    store.clear_user_words(&schema)
}

/// 规范化手动提交的编码，返回 `(扁平 code, boundary)`。`schema` 为已折叠的数据方案 id。
///
/// 边界来源按可信度取：
/// 1. 码里带空格（`ni hao`）：显式切分，直接采信，并拆成扁平码落库（否则前缀查询匹配不到）。
/// 2. 无空格：交给 [`wind_engine::Engine::resolve_boundary`] 的求解链。
///
/// 有意不拒收 `Unresolvable`：手动加词是用户明确意图，静默拒绝会变成「点了保存没反应」；
/// 合法性拦截只放在有预览的导入闸口。求解不出照旧落 `boundary = 0`。
pub fn normalize_add_code<H: WebDataHost + ?Sized>(
    h: &H,
    schema: &str,
    code: &str,
    text: &str,
) -> (String, u64) {
    normalize_add_codes(h, schema, &[(code, text)])
        .pop()
        .expect("一条进一条出")
}

/// 一批 [`normalize_add_code`]（`(code, text)`），与入参同序等长；边界求解链整批走一次。
pub fn normalize_add_codes<H: WebDataHost + ?Sized>(
    h: &H,
    schema: &str,
    pairs: &[(&str, &str)],
) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = pairs
        .iter()
        .map(|(code, _)| {
            let (flat, explicit) = wind_store::wdict::split_spaced_code(code);
            // ü 归一：落库 key 须与 `PinyinEngine::convert` 归一后的查询同域，否则加得进打不出。
            // 须先于「显式边界不求解」的分流，带空格的码也要归一；等长替换，不影响 `explicit`。
            (normalize_pinyin_code(h, schema, flat), explicit)
        })
        .collect();
    let todo: Vec<usize> = (0..out.len()).filter(|&i| out[i].1 == 0).collect();
    if todo.is_empty() {
        return out;
    }
    let ask: Vec<(&str, &str)> = todo
        .iter()
        .map(|&i| (out[i].0.as_str(), pairs[i].1))
        .collect();
    let res = h.engine_mgr().resolve_boundaries(schema, &ask);
    let resolved: Vec<(usize, wind_engine::BoundaryResolution)> = todo
        .iter()
        .enumerate()
        .map(|(k, &i)| {
            (
                i,
                res.get(k)
                    .copied()
                    .unwrap_or(wind_engine::BoundaryResolution::NoInfo),
            )
        })
        .collect();
    for (i, r) in resolved {
        // 求解不出的唯一观测点（这类词的简拼会退化成现猜）。含用户词条，只能用 `debug!`。
        if r == wind_engine::BoundaryResolution::Unresolvable {
            tracing::debug!(
                "加词：拼音码求解失败，按无边界落库 code={} text={}",
                out[i].0,
                pairs[i].1
            );
        }
        out[i].1 = r.boundary();
    }
    out
}

/// 目标方案是否拼音族——决定归一化策略与是否执行准入判据。
pub fn target_is_pinyin<H: WebDataHost + ?Sized>(h: &H, schema_id: &str) -> bool {
    h.engine_mgr()
        .schema_engine_type(schema_id)
        .map(|t| t == "pinyin")
        .unwrap_or(false)
}

/// 拼音方案的 ü 拼写归一（`xv` → `xu`、`nue` → `nve`）；非拼音方案原样返回。
///
/// 不放在 `wind_store::import_formats::CodePolicy`：`wind-store` 拿不到 `wind-engine` 的
/// `normalize_u_umlaut`，复制规则会漂移。
///
/// 判据必须是「目标方案是不是拼音」而不是「码里有没有 v」：码表的 `v` 是正经码元。
pub fn normalize_pinyin_code<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    code: String,
) -> String {
    if !target_is_pinyin(h, schema_id) {
        return code;
    }
    match wind_engine::pinyin::spelling::normalize_u_umlaut(&code) {
        std::borrow::Cow::Borrowed(_) => code,
        std::borrow::Cow::Owned(s) => s,
    }
}
