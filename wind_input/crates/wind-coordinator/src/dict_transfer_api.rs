//! 词库导出 / 导入（多段 .wdict 与 Rime/TSV）的本体，桌面设置页与移动端门面共用。
//!
//! # 为什么在这里而不在 wind-webdata
//!
//! 原本是 wind-webdata 里 `dict.export` / `dict.import` 的 RPC 本体。移动端要做「个人词库
//! 备份 / 还原」且**文件要与电脑版互通**，但 Android 闭包刻意不含 wind-webdata（理由见
//! [`crate::user_words_api`] 的模块注释），照抄一份的话引擎类型校验、段交集、拼音准入契约
//! 迟早两边各改各的——而互通恰恰要求两边一字不差。它们只依赖 [`WebDataHost`] 窄面，故下沉
//! 到这里，两端各自委托（先例：`user_words_api`）。
//!
//! # 边界
//!
//! 这里只收发文本与 Rust 结构体。`path` 读写盘、`strategy` 参数解析、结果的 JSON 形状
//! 仍在 wind-webdata；[`DictImportOutcome`] 带足了让它**原样**拼回旧 JSON 的全部字段。

use wind_store::dict_export::DictSection;
use wind_store::user_words::WordsImportCounts;
use wind_store::wdict::WordIo;

use crate::user_words_api;
use crate::web_host::WebDataHost;

/// 解析方案的权威引擎类型（schema.toml 的 engine.type 可能为空，
/// 此时按 Schema::is_pinyin/is_mixed 依据默认词库类型推断）。
pub fn resolve_engine_type(s: &wind_config::Schema) -> &'static str {
    if s.engine.engine_type.eq_ignore_ascii_case("english") {
        // 必须显式分流：english 走 `is_pinyin()` 的兜底分支（主词库 dict_type 是 "english"
        // 而非 "rime_pinyin"）会落到 "codetable"，方案列表就会把英文标成「码表」。
        "english"
    } else if s.is_mixed() {
        "mixed"
    } else if s.is_pinyin() {
        "pinyin"
    } else {
        "codetable"
    }
}

/// 方案的引擎类型；方案读不出时按码表算（与改动前的 RPC 一致）。
fn schema_engine_type<H: WebDataHost + ?Sized>(h: &H, schema_id: &str) -> &'static str {
    h.engine_mgr()
        .schema_merged(schema_id)
        .map(|s| resolve_engine_type(&s))
        .unwrap_or("codetable")
}

/// 引擎类型 → 中文标签（错误/提示文案用）。
pub fn engine_type_label(t: &str) -> &'static str {
    match t {
        "pinyin" => "拼音",
        "mixed" => "混输",
        "codetable" => "码表",
        _ => "未知",
    }
}

/// 按引擎类型的默认适用数据段。
///
/// ⚠️ 与设置页的子标签（`wind-setting` 的 `pages::dict::state::tabs_for_domain_at`）**同源
/// 但不等价**，且这条跨仓契约**没有编译期约束**：
///
/// - 必须同增同减的是「**某类数据在这个引擎下存不存在**」。只改一边的表现是「设置页看得见、
///   导出文件里没有」，用户直到还原时才发现丢了数据。拼音曾是三段（无候选调整），因为那时
///   拼音下调位被整体屏蔽；置顶放开后两边同步补上 Shadow。
/// - 允许不同的是「**这里从宽**」：英文域在设置页不显示临时词库（那张表恒空），这里仍走
///   `_` 分支带上它。导出多一段空数据无害，少一段则是丢数据——两个方向的代价不对称。
pub fn default_dict_sections(engine_type: &str) -> Vec<DictSection> {
    use DictSection::*;
    match engine_type {
        "mixed" => vec![Shadow],
        _ => vec![UserWords, TempWords, Freq, Shadow],
    }
}

/// [`apply_pinyin_entry_contract`] 的处置统计，逐项对应导入预览的三档
/// （见 `docs/design/pinyin-entry-boundary-contract.md` §5）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntryContractStats {
    /// 不合法、未入库的行数。
    pub rejected: usize,
    /// 求解成功、补上了边界的行数。
    pub filled: usize,
    /// 多解已按读音权重择一的行数（是 `filled` 的子集）。
    pub ambiguous: usize,
    /// 切分已定、但 `text` 含无读音字符（符号 `←`、外文）而验证不了的行数
    /// （是 `filled` 的子集）。**照常入库**——见 `BoundaryResolution::NoReading`。
    ///
    /// 单列一档不是为了拦截，是为了保住一个信号：这类行从前按非法拒收并告警，
    /// 现在静默入库；若某次导入这个数很大，用户仍该有机会怀疑自己拿错了文件。
    pub no_reading: usize,
    /// 入库了但仍无边界的行数（码超 64 字节等）。
    pub no_boundary: usize,
    /// 用户选了「不补充」⇒ 本可补齐、但按其意愿**跳过未入库**的行数。
    ///
    /// 与 [`Self::rejected`] 分开计：那是「程序判定不合法」，这是「用户选择不要」。
    /// 合并成一个数会让 UI 只能说「少了 N 条」而讲不出是谁的决定。
    pub unfilled_skipped: usize,
    /// 被拒行的样例（至多 5 条）——UI 要能让用户看出「是不是选错了文件」。
    pub samples: Vec<String>,
}

/// 按求解裁决处置每一行：留下哪些、丢弃哪些、各自计几笔。
///
/// `verdicts` 与 `rows` **同序等长**；`None` = 层 1（文件自带空格，不进求解），一律放行。
///
/// ★ 从 [`apply_pinyin_entry_contract`] 里抽出来是为了**可测**：那个函数要拿
/// `engine_mgr` 求解，没有数据目录的测试夹具里引擎压根加载不起来，整段判据
/// 在测试里是空转的。分支全在这里，纯函数，喂裁决就能验。
fn dispose_contract_rows(
    rows: Vec<WordIo>,
    verdicts: &[Option<wind_engine::BoundaryResolution>],
    fill: bool,
) -> (Vec<WordIo>, EntryContractStats) {
    let mut st = EntryContractStats::default();
    let mut out = Vec::with_capacity(rows.len());
    for (i, mut r) in rows.into_iter().enumerate() {
        let Some(v) = verdicts.get(i).copied().flatten() else {
            out.push(r); // 层 1：切分是词库作者写下的真值，直接采信
            continue;
        };
        if v == wind_engine::BoundaryResolution::Unresolvable {
            st.rejected += 1;
            if st.samples.len() < 5 {
                st.samples.push(format!("{} {}", r.code, r.text));
            }
            continue;
        }
        let b = v.boundary();
        if b != 0 {
            if !fill {
                // 用户选了「不补充」：这些行整条跳过。**不能原样入库**——
                // boundary=0 的拼音词条正是本契约要消灭的东西，那等于开正门。
                st.unfilled_skipped += 1;
                continue;
            }
            // ★ 走 `WordIo::boundary` 而不是把 code 改写成带空格形态：空格载体表达不了
            // 单音节（`xian` 的 0b1 经 join→split 退化为 0），单字词的补齐会静默失效、
            // 而这里还会把它算进 filled。
            r.boundary = Some(b);
            st.filled += 1;
        } else {
            st.no_boundary += 1;
        }
        // 两档互斥（见 `BoundaryResolution::NoReading` 的 ⚠️）：读音表缺席时没有权重可择，
        // 「已按读音权重择一」讲不通，不能两边都记。
        if matches!(v, wind_engine::BoundaryResolution::Ambiguous(_)) {
            st.ambiguous += 1;
        } else if matches!(v, wind_engine::BoundaryResolution::NoReading(_)) {
            st.no_reading += 1;
        }
        out.push(r);
    }
    (out, st)
}

/// 按目标引擎挑编码归一化策略（`wind-store` 拿不到 `engine_mgr`，故由这一层决定）。
pub fn code_policy_for<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
) -> wind_store::import_formats::CodePolicy {
    if user_words_api::target_is_pinyin(h, schema_id) {
        wind_store::import_formats::CodePolicy::PINYIN
    } else {
        wind_store::import_formats::CodePolicy::CODETABLE
    }
}

/// 拼音词条入库契约：补齐音节边界、剔除不合法行。
/// 见 `docs/design/pinyin-entry-boundary-contract.md`。
///
/// 非拼音方案原样放行——码表词组码没有音节语义，`boundary = 0` 是**正确语义**。
///
/// ★ 求解出的边界写进 **`WordIo::boundary`**，不走「把 code 改写成带空格形态」那条。
///
/// ⚠️ 本段曾经写反过（说的是走空格载体）。空格确实是本仓别处的边界载体，但它
/// **表达不了单音节**——`xian` 的 `0b1` 经 join→split 会退化成 0，于是单字词的补齐
/// 静默失效、而统计那边还照样记进 `filled`。判据见 `dispose_contract_rows` 里的
/// 同名 ★ 注释，那里是真正落笔的地方。
///
/// `fill` = 用户在导入对话框里的二选一（见 §5）：`true` 由程序补充边界；`false` 则
/// **跳过**这些行、不入库，把处置权交回用户。⚠️ `false` 不等于「照原样导入」——
/// 那是给不变量开正门，设计上有意不提供第三个选项。
pub fn apply_pinyin_entry_contract<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    rows: Vec<WordIo>,
    fill: bool,
) -> (Vec<WordIo>, EntryContractStats) {
    if !user_words_api::target_is_pinyin(h, schema_id) {
        return (rows, EntryContractStats::default());
    }
    // ü 拼写归一（`xv` → `xu`、`nue` → `nve`），与 `normalize_add_code` 同款。
    //
    // ★ 必须改在 `rows` 上、不能只归一下面那份 `flats`：`flats` 只喂
    // `resolve_boundaries`，**从不回写**，落库 key 自始至终取自 `r.code`。
    // 只动 flats 的结果是「boundary 求解对了、词还是打不出」——比不修更难查，
    // 因为导入统计会把它记进 `filled`，一切看起来都成功了。
    //
    // 归一化是等长替换且判据只看紧邻字节，故对层 1 那些带空格的码同样安全：
    // `nu e` 的 `u` 后继是空格（不触发）、`n ue` 的 `u` 前驱是空格（不触发），
    // 作者写下的切分不会被改写。
    let mut rows = rows;
    for r in rows.iter_mut() {
        r.code = user_words_api::normalize_pinyin_code(h, schema_id, std::mem::take(&mut r.code));
    }
    // 层 1：文件自带空格的行，切分是词库作者写下的真值 —— 直接采信，不进求解。
    let mut flats: Vec<String> = Vec::with_capacity(rows.len());
    let mut pending: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let (flat, explicit) = wind_store::wdict::split_spaced_code(&r.code);
        if explicit == 0 {
            pending.push(i);
        }
        flats.push(flat);
    }
    let pairs: Vec<(&str, &str)> = pending
        .iter()
        .map(|&i| (flats[i].as_str(), rows[i].text.as_str()))
        .collect();
    let solved = h.engine_mgr().resolve_boundaries(schema_id, &pairs);

    let mut verdicts: Vec<Option<wind_engine::BoundaryResolution>> = vec![None; rows.len()];
    for (k, &i) in pending.iter().enumerate() {
        verdicts[i] = solved.get(k).copied();
    }
    dispose_contract_rows(rows, &verdicts, fill)
}

/// 导出方案数据为单个多段 wdict 文本。`sections` 缺省按引擎类型取默认适用段。
///
/// `schema_id` 是未折叠的方案 id：数据按数据方案折叠（拼音族 → `pinyin`），文件头的
/// `engine_type` 取该方案的引擎类型，供导入时校验来源（防五笔词库导入拼音致编码错乱）。
pub fn export<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    sections: Option<&[DictSection]>,
) -> anyhow::Result<String> {
    let data_schema = h.engine_mgr().data_schema_id(schema_id); // 拼音族折叠到 "pinyin"
    let etype = schema_engine_type(h, schema_id);
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    let sections = match sections {
        Some(s) => s.to_vec(),
        None => default_dict_sections(etype),
    };
    store.export_dict_sections_wdict(
        &data_schema,
        &sections,
        &chrono::Local::now().to_rfc3339(),
        etype,
    )
}

/// 导入结果的一段。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DictSectionOutcome {
    /// 段的 RPC 标识（`userWords` / `tempWords` / `freq` / `shadow`）。
    pub key: &'static str,
    /// 用户词库分类计数（仅 `userWords`）；其余段看 [`Self::imported`]。
    pub words: Option<WordsImportCounts>,
    /// 实际导入条数（准入闸之后）。
    pub imported: usize,
    /// 未导入的总数：解析期跳过 + 准入拒收 + 用户选「不补充」而跳过。
    pub skipped: usize,
    /// 准入契约的分类统计（只有词条段过闸：`userWords` / `tempWords`）。
    pub contract: Option<EntryContractStats>,
}

/// 导入结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DictImportOutcome {
    /// 识别出的文件格式（`WindDict` 走多段，其余只有 `userWords` 一段）。
    pub format: wind_store::import_formats::DictFormat,
    pub sections: Vec<DictSectionOutcome>,
}

/// 导入。WindDict 多段：`sections` 选要应用的类型（缺省 = 文件所含全部段，给了则取与文件
/// 所含段的交集），文件头 `engine_type` 与目标方案不一致即拒绝；Rime/TSV：仅用户词库。
///
/// `replace` = 先清空对应段再导入（否则合并）；`fill_boundary` 见
/// [`apply_pinyin_entry_contract`] 的 `fill`。
pub fn import<H: WebDataHost + ?Sized>(
    h: &H,
    schema_id: &str,
    content: &str,
    replace: bool,
    fill_boundary: bool,
    sections: Option<Vec<DictSection>>,
) -> anyhow::Result<DictImportOutcome> {
    let data_schema = h.engine_mgr().data_schema_id(schema_id);
    let store = h
        .user_store()
        .ok_or_else(|| anyhow::anyhow!("无持久化存储"))?;
    let fmt = wind_store::import_formats::detect_dict_format(content);
    if fmt == wind_store::import_formats::DictFormat::WindDict {
        // 校验来源引擎类型：防跨类型误导（如五笔词库导入拼音方案致编码错乱）。
        let target = schema_engine_type(h, schema_id);
        if let Some(src) = wind_store::wdict::read_header_field(content, "engine_type")
            && !src.is_empty()
            && src != target
        {
            return Err(anyhow::anyhow!(
                "该文件为「{}」类型词库，与当前「{}」方案不一致，导入会导致编码错乱，已阻止。",
                engine_type_label(&src),
                engine_type_label(target),
            ));
        }
        // 文件实际含的段 ∩ 用户所选（缺省=全部所含段）。
        let present: Vec<DictSection> = wind_store::wdict::sections_present(content)
            .iter()
            .filter_map(|t| DictSection::from_key(t))
            .collect();
        let sections: Vec<DictSection> = match sections {
            Some(sel) => sel.into_iter().filter(|s| present.contains(s)).collect(),
            None => present,
        };
        // 准入契约注入闸口：store 拿不到引擎，求解链只能由这一层供给（同 `CodePolicy`）。
        // 逐段收集统计——两段各报各的，混成一份会把临时词的拒收算到用户词头上。
        let mut contract: Vec<(&'static str, EntryContractStats)> = Vec::new();
        let rep = store.import_dict_sections_wdict(
            &data_schema,
            content,
            &sections,
            replace,
            &mut |sec, rows| {
                let (rows, st) = apply_pinyin_entry_contract(h, schema_id, rows, fill_boundary);
                contract.push((sec.key(), st));
                rows
            },
        )?;
        let sections = rep
            .sections
            .into_iter()
            .map(|s| DictSectionOutcome {
                // store 已把闸口丢掉的行并进 skipped，契约统计只补「为什么少」的分类。
                contract: contract
                    .iter()
                    .position(|(k, _)| *k == s.key)
                    .map(|i| contract.swap_remove(i).1),
                key: s.key,
                words: s.words,
                imported: s.imported,
                skipped: s.skipped,
            })
            .collect();
        Ok(DictImportOutcome {
            format: fmt,
            sections,
        })
    } else {
        // Rime/TSV：仅用户词库。
        let (fmt, rows, skipped) =
            wind_store::import_formats::parse_words_auto(content, code_policy_for(h, schema_id))
                .map_err(|e| anyhow::anyhow!(e))?;
        let (rows, contract) = apply_pinyin_entry_contract(h, schema_id, rows, fill_boundary);
        if replace {
            store.clear_user_words(&data_schema)?;
        }
        let c = store.import_user_words(&data_schema, &rows)?;
        Ok(DictImportOutcome {
            format: fmt,
            sections: vec![DictSectionOutcome {
                key: DictSection::UserWords.key(),
                words: Some(c),
                imported: rows.len(),
                // 解析期跳过的（列数不足/乱码）、准入判据拒收的、以及用户选「不补充」
                // 而跳过的，三者合计——与 WindDict 路径「store 把闸口丢掉的行并进 skipped」
                // 同口径；按类别的分列在 `contract` 里。
                skipped: skipped + contract.rejected + contract.unfilled_skipped,
                contract: Some(contract),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼音词条准入契约的**处置表**：六种裁决 × 两种 fill 选择，各落到哪一档。
    ///
    /// ⚠️ 这条测试之所以直接喂 `dispose_contract_rows` 而不走 `import`：没有数据目录的
    /// 夹具引擎加载不起来 ⇒ `target_is_pinyin` 恒 false ⇒ 契约整段空转。走导入口写出来的
    /// 用例会**永远绿着却一个分支都没进**，是本仓最典型的假绿形态。
    #[test]
    fn contract_disposition_table_covers_both_fill_choices() {
        use wind_engine::BoundaryResolution as B;
        let row = |code: &str, text: &str| WordIo {
            code: code.into(),
            text: text.into(),
            weight: 100,
            count: 0,
            boundary: None,
        };
        let rows = vec![
            row("ni hao", "你好"),   // 层 1：自带空格
            row("nihao", "你好"),    // Derived
            row("angan", "安甘"),    // Ambiguous
            row("wgkq", "工"),       // Unresolvable
            row("verylong", "超长"), // NoInfo（码超 64 字节等）
            row("zuo", "←"),         // NoReading（issue #97：拼音码 → 符号候选）
        ];
        let verdicts = [
            None,
            Some(B::Derived(0b101)),
            Some(B::Ambiguous(0b101)),
            Some(B::Unresolvable),
            Some(B::NoInfo),
            Some(B::NoReading(0b1)),
        ];

        // ① 补充（默认）
        let (kept, st) = dispose_contract_rows(rows.clone(), &verdicts, true);
        assert_eq!(kept.len(), 5, "只有 Unresolvable 不入库");
        assert_eq!(st.rejected, 1);
        assert_eq!(st.filled, 3, "Derived / Ambiguous / NoReading 都算补齐");
        assert_eq!(st.ambiguous, 1, "Ambiguous 是 filled 的子集");
        assert_eq!(
            st.no_reading, 1,
            "★ NoReading 单列一档：它入库，但读音表验证不了，用户该知道有多少条"
        );
        assert_eq!(st.no_boundary, 1);
        assert_eq!(st.unfilled_skipped, 0);
        assert_eq!(kept[0].boundary, None, "层 1 不该被改写");
        assert_eq!(
            kept[1].boundary,
            Some(0b101),
            "求解结果必须落到 boundary 字段"
        );
        assert_eq!(
            kept[4].boundary,
            Some(0b1),
            "★ #97：符号词条要带着解出的单音节边界入库，落 0 会让简拼索引静默失效"
        );
        assert_eq!(
            st.samples,
            vec!["wgkq 工".to_string()],
            "拒收要留样例；★ NoReading 不该混进来——它没被拒"
        );

        // ② 不补充：可补的那三条**跳过**，而不是原样入库
        let (kept, st) = dispose_contract_rows(rows.clone(), &verdicts, false);
        assert_eq!(kept.len(), 2, "层 1 与 NoInfo 仍入库，可补的三条被跳过");
        assert_eq!(st.unfilled_skipped, 3);
        assert_eq!(st.filled, 0);
        assert_eq!(st.rejected, 1, "拒收与用户的选择无关");
        assert!(
            kept.iter().all(|r| r.text != "安甘"),
            "★ 不补充 ≠ 照原样导入：boundary=0 的拼音词条正是契约要消灭的东西"
        );
    }

    /// 拼音的默认导出段必须含**候选调整**，与设置页子标签同增同减。
    ///
    /// 这条跨仓契约没有编译期约束：设置页给了拼音「候选调整」tab、而导出默认不含该段时，
    /// 用户在设置页看得见规则，导出的文件里却没有，直到还原时才发现丢了数据。
    #[test]
    fn pinyin_default_export_sections_include_shadow() {
        let py = default_dict_sections("pinyin");
        assert!(
            py.contains(&DictSection::Shadow),
            "拼音默认导出段须含候选调整，实际: {py:?}"
        );
        // 反向对照：混输只有候选调整这一段，证明本函数确实在按引擎类型分流，
        // 而不是被改成了「一律返回全部段」。
        assert_eq!(
            default_dict_sections("mixed"),
            vec![DictSection::Shadow],
            "混输仍只导出候选调整"
        );
    }
}
