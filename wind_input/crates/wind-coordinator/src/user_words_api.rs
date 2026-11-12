//! 用户词管理（列表 / 加词 / 删词 / 清空）的存储域逻辑，桌面设置页与移动端门面共用。
//!
//! # 为什么在这里而不在 wind-webdata
//!
//! 这些逻辑原本是 wind-webdata 里 `dict.listPaged` / `dict.add` / `dict.remove` /
//! `dict.clear` 的 RPC 本体。移动端设置页要做同样的「词库」页，但 **Android 闭包刻意不含
//! wind-webdata**（它带 wind-transfer/fontdb 这些 C 依赖，见本 crate Cargo.toml 的 ⚠ 注释），
//! wind-mobile 依赖不了它。若移动端照抄一份，ü 归一、边界求解、三条分流的搜索语义迟早
//! 两边各改各的——本仓「写入归一了、查询入口没跟上」那类 bug 就是这么来的。
//!
//! 它们实际只依赖 [`WebDataHost`] 窄面（引擎管理器 + 用户库），所以下沉到这里，两端各自
//! 委托。先例是统计报表（`wind_store::stats_report`）。
//!
//! # 边界：只管存储域
//!
//! 这里收发的是**存储域**——真实文本、扁平码（`nihao` + boundary bitmask）。显示域的投影
//! 各端自己做：桌面的 `ui_text`/`store_text` 转义与列表项 JSON 形状仍在 wind-webdata；
//! 带音节空格的显示码可用 [`display_code`] 从记录得到。
//!
//! # 不在这里做的
//!
//! - **只读模式拒写**：桌面在只读模式下根本不打开 store（`user_store()` 为 `None`，写口
//!   自然报「无持久化存储」），本层不另加闸，以保桌面 RPC 的行为与错误文案一字不变。
//!   需要显式闸门的调用方（移动端门面）自己先查 `storage_readonly_reason()`。
//! - **页缓存回收**：全表扫描后的标记、批量写后的回收是 RPC 出口的策略（wind-webdata 的
//!   `web_data_rpc`），不属于单个操作。

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
    // 编码前缀须用扁平码（key 是扁平的），用户可能照着列表显示的 `ni hao` 来搜。
    // 下面的**词条内容**搜索仍用原串——那是拿汉字去匹配 text，与音节空格无关。
    //
    // ★ 再过一道 ü 归一：写入侧（`normalize_add_code`）已把 `xv` 落成 `xu`，而
    // `search_user_words_prefix` 是**裸字节前缀匹配**——不归一则用户手打 `xv` 搜
    // 自己刚加的词一条也出不来。写入归一了、查询入口没跟上，正是本仓踩过多次的
    // 那个形态（见 `docs/design/pinyin-code-domains.md`）。
    let (code_prefix, _) = wind_store::wdict::split_spaced_code(query);
    let code_prefix = normalize_pinyin_code(h, &schema, code_prefix);

    // ★ 不搜也不排序：按 key 序**流式**取一页，全表只物化 `limit` 条。
    //
    // 设置页打开词库标签走的就是这条，是三条路里最常被走的。从前它与另外两条一样
    // 「先全收进 Vec 再 skip/take」——19 万条的库要先造 38 万个 `String` 才切出 50 条。
    //
    // ⚠️ 仍然**扫完整表**，不能中途早退：`total` 要精确（分页器显示「共 N 条」）。
    // 省掉的是物化，不是扫描；redb 的读缓存该填还是填（见 wind-store 的
    // `tests/redb_cache_high_water.rs`）。那笔高水位另有出路，不在这里解决。
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
        // 有搜索词：**一遍**扫描分两组，编码前缀命中在前、其余命中在后。
        //   ① 词条内容包含搜索词（拿汉字匹配 text，用原串）
        //   ② **编码中段包含**搜索词（用拆过的扁平码）—— 前缀扫描只能命中开头，
        //      `haoya` 搜 `ya` 一条也出不来，而用户并不知道搜索框只认前缀。
        //
        // 从前这里扫**两遍**全表（一遍前缀、一遍全量），再拿一个全量 clone 的
        // `HashSet<(String,String)>` 去重——而前缀命中本就是全量扫描的子集，第二遍
        // 纯属重复，那个 HashSet 更是把整张表的 code/text 又抄了一份。分两组天然互斥，
        // 去重结构一并不需要了。顺序与从前逐条相同。
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
/// `text` 是**存储域**真实文本——桌面设置页传来的转义形态须由调用方先还原。`code` 可带
/// 音节空格。落库经 `Store::add_user_word`，它推进该方案的结构代次（`bump_words_gen`），
/// 引擎据此失效候选缓存——加完立刻打得出，各端走同一函数故自动一致。
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

/// **单条显式**加词（加词对话框 / 快捷加词 / 命令栏 `dict.add`、设置页加词改词、单条手动转正）
/// 成功后的收口：撤掉同 (数据方案, 编码, 文本) 的候选调整删除规则。
///
/// 读端 `apply_shadow` 按文本过滤、不看来源：用户删过某个系统词后再手动加回同码同文的词，
/// 规则不撤就永远打不出来。**自动**学词 / 自动晋升不走这里——删过的词不该因为又打到一次就复活。
/// **批量**操作（导入词库、全部转正）也不撤：一批词里混着用户删过的，不该悄悄全复活；它们改用
/// [`count_hidden_by_shadow`] 在结果里报数。
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
/// 两条边界来源，按可信度取：
/// 1. **用户在码里打了空格**（`ni hao`）—— 显式声明的切分，直接采信。同时必须拆成扁平
///    码：带空格的串若原样落库，key 就成了 `ni hao`，前缀查询再也匹配不到它。
/// 2. 无空格 → 交给 [`wind_engine::Engine::resolve_boundary`] 的四层求解链。
///
/// ★ 这里原本是一个私有的 `infer_boundary_for`，只做「手输码 == 引擎推导码则借用其
/// 切分」这一条判据——**那正是求解链的层 3**，是重复实现。收敛掉之后手输码额外获得
/// 层 2（词典点查）与层 4（字数约束求解）两条来源：以前用户手打 `xianning` +「西安宁」
/// 因推导码对不上而拿 0，现在能解出 `xi|an|ning`。
///
/// ⚠️ **有意不在这里拒收** `Unresolvable`：本函数服务于手动加词，那是用户明确
/// 的意图，静默拒绝会变成「点了保存没反应」。合法性拦截只放在导入闸口（那里有预览
/// 可以如实告知）。此处非法码照旧落 `boundary = 0`，与改动前等价。
pub fn normalize_add_code<H: WebDataHost + ?Sized>(
    h: &H,
    schema: &str,
    code: &str,
    text: &str,
) -> (String, u64) {
    let (flat, explicit) = wind_store::wdict::split_spaced_code(code);
    // ü 拼写归一（`xv` → `xu`、`nue` → `nve`）。**落库 key 与查询侧必须同域**：
    // 查询侧 `PinyinEngine::convert` 已把击键归一成正字法形态，key 若停在 `xv`
    // 就成了「加得进、打不出」——用户看得见这条词，永远召不回。
    //
    // ⚠️ 必须先于 `explicit` 那条早退：带空格的码（`nue yao`）走的是另一条 return，
    // 漏了它就只有「手打空格声明切分」的用户中招，而那恰恰是最难复现的一档。
    // 归一化是等长替换，故 `explicit` 这个按位置算的 bitmask 不受影响。
    let flat = normalize_pinyin_code(h, schema, flat);
    if explicit != 0 {
        return (flat, explicit);
    }
    let res = h
        .engine_mgr()
        .resolve_boundaries(schema, &[(flat.as_str(), text)])
        .into_iter()
        .next()
        .unwrap_or(wind_engine::BoundaryResolution::NoInfo);
    // 探测器：手输码落在契约之外时留一条痕。**不改变行为**（照旧落 boundary=0），
    // 它是「有多少手输码求解不出」的唯一观测点——这类词的简拼会退化成 DAG 现猜，
    // 而那在歧义码上必错，用户侧的表现是「加了词但简拼召不回」，无从追溯。
    //
    // ⚠️ 用 `debug!` 而非 `info!`：本行含 code 与 text，属用户词库内容。
    // INFO 生产默认开启，不得记录用户输入类信息。
    if res == wind_engine::BoundaryResolution::Unresolvable {
        tracing::debug!(
            "加词：拼音码求解失败，按无边界落库 code={} text={}",
            flat,
            text
        );
    }
    (flat, res.boundary())
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
/// 落点在这一层而非 `wind_store::import_formats::CodePolicy`（那是导入解析期的
/// 编码归一策略，概念上更贴），是因为 `wind-store` 在依赖图里位于 `wind-engine`
/// **下游**，拿不到 `normalize_u_umlaut`；复制一份规则则必然随时间漂移。
///
/// ⚠️ 判据必须是「目标方案是不是拼音」而不是「码里有没有 v」：码表/五笔/快符的
/// 码里 `v` 是正经码元（五笔 `v` = 女字旁），动它会把用户的词改成打不出的码。
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
