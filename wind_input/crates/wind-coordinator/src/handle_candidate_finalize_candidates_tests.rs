//! 候选值展开汇聚点 `finalize_candidates`：所有输入方案共用，保证 `$` 语法一致生效。
use super::*;
use std::sync::Arc;
use wind_config::config::Config;

fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        ..Default::default()
    }
}

fn cand_code(text: &str, code: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        code: code.to_string(),
        ..Default::default()
    }
}

/// 词频记账的码：码表按输入码，拼音/英文按候选码。
///
/// 码表 `d`/`de`/`def` 是三个独立码位，若拿前缀补全带的全码 `def` 记账，打 `d` 时同一个字也会
/// 前移（`ProtectPolicy` 按输入码长分级保护以此为前提）。拼音反向：候选码是完整读音，
/// 打 `d` 选「东西」后打 `dongxi` 也该受益。
#[test]
fn freq_code_splits_codetable_from_pinyin() {
    let mut ct = cand_code("有", "def");
    ct.source = CandidateSource::CodeTable;
    assert_eq!(
        Coordinator::freq_code_with("de", &ct, false),
        "de",
        "码表须按输入码记账，否则 d/de/def 三个码位会互相串扰"
    );
    assert_eq!(Coordinator::freq_code_with("d", &ct, false), "d");
    assert_eq!(Coordinator::freq_code_with("def", &ct, false), "def");

    let mut py = cand_code("东西", "dongxi");
    py.source = CandidateSource::Pinyin;
    assert_eq!(
        Coordinator::freq_code_with("d", &py, false),
        "dongxi",
        "拼音按候选码——跨码位共享才是想要的行为"
    );
    assert_eq!(
        Coordinator::freq_code_with("d", &py, true),
        "dongxi",
        "英文开关不得波及拼音"
    );

    // 候选无码时退回输入缓冲（`cand_code` 的既有语义，两侧一致）
    let mut bare = cand_code("啊", "");
    bare.source = CandidateSource::Pinyin;
    assert_eq!(Coordinator::freq_code_with("a", &bare, false), "a");
}

/// 英文记账码口径可配（内部项 `english_code_scope`）：英文几乎全是前缀匹配，
/// 跨码位共享与码位独立各有道理，尚未定论，故留旋钮而非写死。
#[test]
fn freq_code_english_scope_is_switchable() {
    let mut en = cand_code("hello", "hello");
    en.source = CandidateSource::English;
    assert_eq!(
        Coordinator::freq_code_with("hel", &en, false),
        "hello",
        "默认 candidate：打 hel 选 hello 后，打 he 也该受益"
    );
    assert_eq!(
        Coordinator::freq_code_with("hel", &en, true),
        "hel",
        "切到 input：各前缀独立学习，与码表同侧"
    );
}

/// 守门：所有 `record_selection` 生产调用点的记账码都必须经 [`Coordinator::freq_code`]。
///
/// 漏掉一处是静默的：写进 FREQ 表的是孤儿键（双拼击键 / 带分隔符 / 前缀补全码），读端按候选码
/// 永远查不到，而写入本身成功，常规测试不会失败。用 `include_str!` 编译期嵌入而非运行时读盘：
/// test 跑 Linux 宿主、clippy 交叉编 Windows，运行时拼源码路径两边语义不同。
#[test]
fn every_record_selection_call_goes_through_freq_code() {
    // 判据是「实参文本里出现 freq_code」，顺带强制中转变量必须命名为 `freq_code`。
    // 白名单：确有理由不走 freq_code 的调用点，键为其首个实参的源码文本。
    const ALLOWED: &[(&str, &str)] = &[
        (
            "prefix",
            "commit_top_text：顶码机制归属码表，prefix 即被顶出的输入码，本就是码表口径",
        ),
        // 纳入 `record_selection_in` 扫描后暴露的三处既有调用点，都属下面两类之一。
        // 新增以 `code` 为名的调用点时，必须回到这里确认自己确实属于其中一类，
        // 而不是顺手借用这条例外——本条按实参文本匹配，无法区分文件。
        (
            "code",
            "两类：① 转发层——`record_selection` / `record_selection_cand` 把自己的形参 \
                 原样传给 `_in`，分流发生在它们的调用方（那些调用点本测试都扫得到）；\
                 ② handle_special 的特殊模式记账——特殊方案是码表语义，`a`/`ab`/`abc` \
                 是三个独立码位，用输入码正是 `freq_code` 对 CodeTable 来源的口径",
        ),
    ];
    let sources: &[(&str, &str)] = &[
        ("coordinator.rs", include_str!("coordinator.rs")),
        ("handle_candidate.rs", include_str!("handle_candidate.rs")),
        ("handle_mode.rs", include_str!("handle_mode.rs")),
        ("handle_special.rs", include_str!("handle_special.rs")),
        ("handle_temp.rs", include_str!("handle_temp.rs")),
    ];
    let mut checked = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for (name, src) in sources {
        // 只看 `#[cfg(test)]` 之前的部分：测试自己用字面量直接构造键是合法的。
        let prod = crate::source_scan::production_code(src);
        let prod = prod.as_str();
        // 三个名字都扫：`record_selection_cand` 是分流 emoji 的薄包装，口径约束同样成立；
        // `record_selection_in` 多一个前置的 schema 实参，记账码是第 2 个——不按位置分流的话扫到的是
        // 归属方案而非码，判据失效且全绿。
        let calls = [
            ".record_selection(",
            ".record_selection_cand(",
            ".record_selection_in(",
            ".record_selection_cand_in(",
            // 词频文本与历史文本分开给的完整版（英文大小写投影用）。名字不登记进来
            // 就等于给记账码开了一扇后门——本守卫是按调用点扫的，扫不到的名字
            // 可以随便传一个没过 `freq_code` 的串。
            ".record_selection_cased_in(",
        ];
        let sites = calls
            .iter()
            .flat_map(|pat| prod.match_indices(*pat).map(move |(off, _)| (off, *pat)));
        for (off, pat) in sites {
            let args = &prod[off + pat.len()..];
            // 带 `_in` 的两个多一个前置 schema 实参，记账码在第 2 位。
            let code_pos = usize::from(
                pat == ".record_selection_in("
                    || pat == ".record_selection_cand_in("
                    || pat == ".record_selection_cased_in(",
            );
            // 按括号深度切顶层逗号，取第 `code_pos` 个实参（实参可能是 `self.freq_code(a, b)`）。
            let mut depth = 0i32;
            let mut cur = 0usize;
            let mut start = 0usize;
            let mut found: Option<&str> = None;
            for (i, ch) in args.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' if depth == 0 => {
                        if cur == code_pos {
                            found = Some(&args[start..i]);
                        }
                        break;
                    }
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        if cur == code_pos {
                            found = Some(&args[start..i]);
                            break;
                        }
                        cur += 1;
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            let Some(arg) = found else { continue };
            let arg = arg.trim().trim_start_matches('&').trim();
            checked += 1;
            if arg.contains("freq_code") {
                continue;
            }
            if !ALLOWED.iter().any(|(a, _)| *a == arg) {
                // 收齐再报：逐个 assert 只暴露第一处，改完重跑才发现还有下一处。
                bad.push(format!("{name}: `{arg}`"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "以下 record_selection 的记账码未经 freq_code：\n  {}\n\
             码表须按输入码、拼音/英文须按候选码——直接传击键缓冲会在双拼/分隔符/\n\
             前缀补全下写成读不到的孤儿键。改用 `&self.freq_code(buf, &cand)`（中转变量\n\
             命名为 `freq_code`），确有理由不走的请加进本测试的 ALLOWED 并写明理由。",
        bad.join("\n  ")
    );
    // 反向保证：调用点被重命名/重构掉时，本测试不得退化成空跑而静默变绿。下限只在有意合并调用点时
    // 下调，下调前务必确认是合并而非漏调；`record_selection_cand` 的调用点计入同一总数。
    assert!(
        checked >= 21,
        "只扫到 {checked} 个 record_selection 调用点，远少于预期——\
             调用点被改名或本测试的扫描方式失效了，先修测试再说"
    );
}

/// 方案归属的读 / 写 / 调试三处必须取自同一处（`effective_data_schema`）。
///
/// 读写不同源时记账看着成功而候选顺序永远不动；调试不同源会在排查时把人带偏。判据是「首个实参
/// ∈ 允许集合」，顺带强制中转变量必须叫 `owner`，逼新增调用点的人回来说明它取的是哪个方案。
#[test]
fn schema_scoped_reads_writes_and_debug_share_one_source() {
    // `None` = 显式走 active 路径（普通输入）；其余必须能追溯到 effective_data_schema。
    const ALLOWED: &[&str] = &[
        "None",
        "owner.as_deref()",
        "self.effective_data_schema(state).as_deref()",
        // 转发层：`record_selection` 薄包装与 `apply_freq_rerank_with_extra` 把自己的参数
        // 原样传下去。
        "schema_override",
        // 后处理步骤表的转发层：`run_passes` 原样传 `PassCtx.owner`；归属在构造 `PassCtx`
        // 处受本测试检查（见下方对 `PassCtx` 字面量 owner 字段的扫描）。
        "ctx.owner",
        // 临英：归属恒是内置英文方案，与 `effective_data_schema` 对 `TempEnglish` 的
        // 返回值是同一个常量（那边写成 `Some(ENGLISH_SCHEMA.to_string())`）。三处读写
        // 端直接用常量而不绕 state，取值同源，故等同于走了那个函数。
        "Some(ENGLISH_SCHEMA)",
        // 登记的例外：临拼的用户数据归属取自 `overlay_engine_schema`（`effective_data_schema` 对
        // `TempPinyin` 返回 `None`）。站得住是因为读写四处（`update_temp_pinyin_candidates` 的两处读与
        // 两个上屏出口的 `record_selection_in`）一律取本变量，折叠后同落 `"pinyin"` 桶。不能落 active：
        // 拼音与码表的编码域与码位结构不同，归到主方案桶等于把拼音码混进五笔码位。
        "temp_pinyin_owner.as_deref()",
        // 同类例外：mix（快捷输入）的用户数据归属取成员方案。读端在 `update_mix_candidates` 里逐成员段
        // 应用，写端按来源反查（`mix_candidate_owner`），两端落同一个桶；按 active 归属会把拼音成员学到的
        // 东西记进主方案的桶。
        "mix_member_owner.as_deref()",
    ];
    const CALLS: &[&str] = &[
        ".record_selection_in(",
        // emoji 守卫版：同样以 schema 为首个实参，归属约束一字不差地适用。
        ".record_selection_cand_in(",
        // 大小写投影版：首个实参同样是归属方案。
        ".record_selection_cased_in(",
        ".apply_freq_rerank_in(",
        ".apply_shadow_in(",
        ".build_debug_schema_ctx(",
    ];
    let sources: &[(&str, &str)] = &[
        ("coordinator.rs", include_str!("coordinator.rs")),
        ("handle_candidate.rs", include_str!("handle_candidate.rs")),
        ("handle_special.rs", include_str!("handle_special.rs")),
        // 临英 / 临拼的候选装配与记账都在这里，此前整个文件不受本守卫覆盖。
        ("handle_temp.rs", include_str!("handle_temp.rs")),
        // mix（快捷输入）同理。
        ("handle_mode.rs", include_str!("handle_mode.rs")),
        // 后处理步骤表（转发）与经它读写的两条路径。
        ("postprocess.rs", include_str!("postprocess.rs")),
        ("handle_direct_aux.rs", include_str!("handle_direct_aux.rs")),
        ("handle_reverse.rs", include_str!("handle_reverse.rs")),
    ];
    let mut checked = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for (name, src) in sources {
        let prod = crate::source_scan::production_code(src);
        let prod = prod.as_str();
        for call in CALLS {
            for (off, _) in prod.match_indices(call) {
                let args = &prod[off + call.len()..];
                // 切首个实参：按括号深度找顶层逗号（实参可能自带括号调用）。
                let mut depth = 0i32;
                let mut end = args.len();
                for (i, ch) in args.char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' if depth == 0 => {
                            end = i;
                            break;
                        }
                        ')' => depth -= 1,
                        ',' if depth == 0 => {
                            end = i;
                            break;
                        }
                        _ => {}
                    }
                }
                let arg = args[..end].trim();
                checked += 1;
                if !ALLOWED.contains(&arg) {
                    bad.push(format!("{name}: {call}…) 的方案实参 `{arg}`"));
                }
            }
        }
    }
    // 经后处理步骤表读写的路径，归属写在 `PassCtx { owner: <方案>, .. }` 里。
    for (name, src) in sources {
        let prod = crate::source_scan::production_code(src);
        for val in crate::source_scan::pass_ctx_field_values(&prod, "owner") {
            let val = val.unwrap_or_else(|| "<缺 owner 字段>".to_string());
            checked += 1;
            if !ALLOWED.contains(&val.as_str()) {
                bad.push(format!("{name}: PassCtx 的 owner `{val}`"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "以下调用点的方案归属不是取自 effective_data_schema：\n  {}\n\
             读(apply_freq_rerank_in) / 写(record_selection_in) / 调试(build_debug_schema_ctx)\n\
             三处必须同源，否则「写进 A、读的是 B」——记账看着成功，排序永远不动。",
        bad.join("\n  ")
    );
    // 反向保证：调用点被改名或扫描失效时，本测试不得退化成空跑而静默变绿。下限须随 sources 扩容
    // 一起上调，否则余量变大等于容忍悄悄丢掉调用点；变动前先确认是「合并/删除」还是「漏调」。
    assert!(
        checked >= 33,
        "只扫到 {checked} 个方案归属调用点，少于预期——扫描方式失效了，先修测试"
    );
}

/// 候选装配的平行实现必须一起接 emoji 扩展。
///
/// `[input.emoji]` 是全局开关，临拼、快捷输入里同样生效；它按候选文本查表追加，与引擎无关。
/// 本条只锁「调用点存在」，锁不住行为：`load_emoji_dict` 取 `Config::data_dir()` 而非注入的
/// data_dir，集成测试里 emoji 表恒不加载（主路径同样测不出）。
#[test]
fn overlay_candidate_paths_apply_emoji_suggestions() {
    const TARGETS: &[(&str, &str, &str)] = &[
        ("handle_candidate.rs", "fn build_candidates", "主输入路"),
        (
            "handle_temp.rs",
            "fn refill_temp_pinyin_candidates",
            "临时拼音",
        ),
        ("handle_mode.rs", "fn refill_mix_candidates", "快捷输入/mix"),
    ];
    // 临英刻意不在表内：英文候选查 emoji 表恒不命中（表键是中文词），接了等于每键白跑
    // 一趟查询。这与 `mark_common`/`apply_filter` 对临英无作用是同一条理由。
    let src = |n: &str| -> &'static str {
        match n {
            "handle_candidate.rs" => include_str!("handle_candidate.rs"),
            "handle_temp.rs" => include_str!("handle_temp.rs"),
            _ => include_str!("handle_mode.rs"),
        }
    };
    let mut bad: Vec<String> = Vec::new();
    for (file, sig, label) in TARGETS {
        let full = src(file);
        let prod = crate::source_scan::production_code(full);
        let prod = prod.as_str();
        let Some(off) = prod.find(sig) else {
            panic!("{label}: 找不到 `{sig}` —— 函数被改名了，先修测试再说");
        };
        let rest = &prod[off + sig.len()..];
        // 函数体 = 到下一个同级 fn 为止。
        let end = rest
            .find("\n    pub(crate) fn ")
            .unwrap_or(rest.len())
            .min(rest.find("\n    fn ").unwrap_or(rest.len()));
        if !rest[..end].contains("self.apply_emoji_suggestions(") {
            bad.push(format!("{label}（{file} 的 `{sig}`）"));
        }
    }
    assert!(
        bad.is_empty(),
        "以下候选装配路径没有接 emoji 扩展：\n  {}\n\
             `[input.emoji]` 是全局开关，用户开了它就该处处生效；临时模式没有独立开关，\n\
             不接的表现是「正式方案里打「你好」有 🙂️、临时模式里没有」，且完全静默。",
        bad.join("\n  ")
    );
}

/// shadow 的码必须处处归一：读端 `apply_shadow` 与写端 `candidate_op_scope` 若各取各的，
/// 失配完全静默（双拼下规则写进 `hao`、读的却是击键 `hc`，只表现为「置顶了没反应」）。
///
/// 判据是「第二个实参 ∈ 允许集合」，顺带强制中转变量必须叫 `shadow_code`。「N 个调用点都要做
/// 同一件事」这类不变量靠注释守不住，故用机械扫描（同上面两个测试）。
#[test]
fn every_shadow_read_goes_through_normalized_code() {
    // 主输入路：一律走 `shadow_code_of` 取出的中转变量。
    // 特殊模式：码在 `special_buffer`（码表方案，无第二编码域），恒等即正确。
    // 空串：特殊模式浏览态的合法键位，见 `apply_shadow_in` 的守卫。
    const ALLOWED: &[&str] = &[
        "shadow_code",
        "state.special_buffer",
        "\"\"",
        // overlay（临拼、mix 成员段）的中转变量：取的是该段引擎的归一码，见各自调用点。
        "code",
        // 菜单灰显走 `candidate_op_scope` 的产物，其 `code` 已在那里归一（同写端一处取值）。
        "scope.code",
        // 直接辅助码的前缀池：前缀下的归一码为空时回落到前缀击键本身（与 `shadow_code_of`
        // 同一条回落规则，只是作用在前缀而非整串缓冲上）。
        "prefix_shadow",
    ];
    const CALLS: &[&str] = &[
        ".apply_shadow(",
        ".apply_shadow_in(",
        // 「恢复默认」的可用性判据也必须同码，否则菜单项灰着而规则其实存在。
        ".shadow_has_rule(",
    ];
    let sources: &[(&str, &str)] = &[
        ("coordinator.rs", include_str!("coordinator.rs")),
        ("handle_candidate.rs", include_str!("handle_candidate.rs")),
        ("handle_special.rs", include_str!("handle_special.rs")),
        ("handle_menu.rs", include_str!("handle_menu.rs")),
        // overlay 的候选装配是主输入路的平行实现，各自持一份 shadow 读取点
        // （临英取小写化缓冲、临拼取引擎归一码）。此前本表漏了这个文件，那两处
        // 一直不受守卫——正是「N 个调用点做同一件事」最容易走散的地方。
        ("handle_temp.rs", include_str!("handle_temp.rs")),
        // mix 的读取点在成员段内，码同样取引擎归一码。
        ("handle_mode.rs", include_str!("handle_mode.rs")),
        // 后处理步骤表：`run_passes` 里的那次读取，码从 `PassCtx` 原样解包成 `shadow_code`。
        ("postprocess.rs", include_str!("postprocess.rs")),
        // 直接辅助码的前缀池（经 `PassCtx`）。
        ("handle_direct_aux.rs", include_str!("handle_direct_aux.rs")),
    ];
    let mut checked = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for (name, src) in sources {
        let prod = crate::source_scan::production_code(src);
        let prod = prod.as_str();
        for call in CALLS {
            // `apply_shadow_in` 多一个前置的 schema 实参，码是第 3 个；`apply_shadow` 是第 2 个。
            let code_pos = if *call == ".apply_shadow_in(" { 2 } else { 1 };
            for (off, _) in prod.match_indices(call) {
                let args = &prod[off + call.len()..];
                // 按括号深度切顶层逗号，取第 code_pos 个实参（实参可能自带括号调用）。
                let mut depth = 0i32;
                let mut cur = 0usize;
                let mut start = 0usize;
                let mut found: Option<&str> = None;
                for (i, ch) in args.char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' if depth == 0 => {
                            if cur == code_pos {
                                found = Some(&args[start..i]);
                            }
                            break;
                        }
                        ')' => depth -= 1,
                        ',' if depth == 0 => {
                            if cur == code_pos {
                                found = Some(&args[start..i]);
                                break;
                            }
                            cur += 1;
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
                let Some(arg) = found else { continue };
                let arg = arg.trim().trim_start_matches('&').trim();
                checked += 1;
                if !ALLOWED.contains(&arg) {
                    // 收齐再报：逐个 assert 只暴露第一处，改完重跑才发现还有下一处。
                    bad.push(format!("{name}: {call}…) 的码实参 `{arg}`"));
                }
            }
        }
        // 经后处理步骤表读 shadow 的路径，码写在 `PassCtx { .., shadow_code: Some(<码>), .. }`
        // 里；`None` = 该 profile 不读 shadow，不计数。
        for val in crate::source_scan::pass_ctx_field_values(prod, "shadow_code") {
            let Some(val) = val else {
                bad.push(format!("{name}: PassCtx 字面量缺 shadow_code 字段"));
                continue;
            };
            if val == "None" {
                continue;
            }
            let inner = val
                .strip_prefix("Some(")
                .and_then(|v| v.strip_suffix(')'))
                .map(|v| v.trim().trim_start_matches('&').trim());
            checked += 1;
            if !inner.is_some_and(|v| ALLOWED.contains(&v)) {
                bad.push(format!("{name}: PassCtx 的 shadow_code `{val}`"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "以下 shadow 读取点的码未经归一：\n  {}\n\
             双拼与全拼折叠为同一个 schema，码若仍取击键，`hc` 与 `hao` 会落成两个互不相认\n\
             的键。改用 `Self::shadow_code_of(state)`（中转变量命名为 `shadow_code`），\n\
             确有理由不走的请加进本测试的 ALLOWED 并写明理由。",
        bad.join("\n  ")
    );
    // 同上：`handle_temp.rs` 进 sources 后实扫 9（原 7、下限留在 5）；mix 接入后 10。
    // 步骤表改造后 shadow 码写在 `PassCtx { shadow_code }`，扫字面量 + direct_aux 进表后 12。
    assert!(
        checked >= 12,
        "只扫到 {checked} 个 shadow 读取点，少于预期——调用点被改名或扫描失效了，先修测试"
    );
}

#[test]
fn aa_group_expands_inline_when_code_absent() {
    let c = coord();
    // 无码信息（code 空）→ 视为精确，逐成员炸开。
    let out = c.finalize_candidates(vec![cand(r#"$AA("数字", "①②③")"#)], "sz");
    let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["①", "②", "③"], "$AA 应一对多炸开为逐字符候选");
    assert!(out.iter().all(|c| !c.is_command && !c.is_group));
}

#[test]
fn aa_group_expands_inline_at_exact_code() {
    let c = coord();
    // 精确码（候选码 == 输入 "arrx"）→ 逐成员炸开。
    let out = c.finalize_candidates(vec![cand_code(r#"$AA("箭头", "←↑→↓")"#, "arrx")], "arrx");
    let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["←", "↑", "→", "↓"]);
}

#[test]
fn aa_group_collapses_to_name_at_prefix() {
    let c = coord();
    // 前缀（候选码 "arrx" 长于输入 "arr"）→ 折叠为组名候选，不炸开。
    let out = c.finalize_candidates(vec![cand_code(r#"$AA("箭头", "←↑→↓")"#, "arrx")], "arr");
    assert_eq!(out.len(), 1, "前缀应折叠为单个组名候选");
    assert_eq!(out[0].text, "箭头", "折叠候选显示组名");
    assert!(out[0].is_group, "折叠候选标 is_group");
    assert_eq!(
        out[0].group_code, "arrx",
        "group_code 为完整码，选中补全后重查展开"
    );
    assert!(!out[0].is_command);
}

#[test]
fn marks_cc_command_and_keeps_source() {
    let c = coord();
    let src = r#"$CC("切简繁", ime.toggle("s2t"))"#;
    let out = c.finalize_candidates(vec![cand(src)], "co");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].text, "切简繁", "$CC display 作候选文本");
    assert!(out[0].is_command, "$CC 应标 is_command");
    assert_eq!(out[0].phrase_template, src, "命令源留存供选中执行");
}

#[test]
fn plain_candidates_pass_through_unchanged() {
    let c = coord();
    // 普通词 + 含 $ 但非语法文本（价格$5）：均原样保留，零干预。
    let out = c.finalize_candidates(vec![cand("你好"), cand("价格$5")], "nh");
    let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["你好", "价格$5"]);
    assert!(out.iter().all(|c| !c.is_command));
}

#[test]
fn already_command_candidate_is_not_re_expanded() {
    let c = coord();
    // 已是 is_command 的候选（短语命中）：跳过二次展开，原样保留。
    let mut pre = cand(r#"$AA("x","ab")"#);
    pre.is_command = true;
    let out = c.finalize_candidates(vec![pre], "x");
    assert_eq!(out.len(), 1, "已标命令不应被再炸开");
    assert!(out[0].is_command);
}

/// 本组用例的输入串。传给 `candidate_display_order` 的跨来源档位：取长度判「候选是否
/// 消费了整串」、取全串判「码 == 输入」（档 0）；`mixed=false` 的用例里该档不参与，
/// 取值无影响。
const XU_LEN: &str = "xu";

fn cand_ordered(text: &str, base_order: i32, natural_order: i32, weight: i32) -> Candidate {
    Candidate {
        text: text.into(),
        code: "y".into(),
        base_order,
        natural_order,
        weight,
        ..Default::default()
    }
}

/// 混输打 `xu` 的三档：码表精确「弱」(`xu`, 二简码) → 拼音精确「需」(`code==xu`) →
/// 码表前缀补全「弹幕」(`xuaj`)。
///
/// 权重取真实值：码表带 `PARTIAL_MATCH_BOOST`(500K)、拼音已 `÷PINYIN_TIER_SCALE`(100)，
/// 「需」纯按权重必输——这一条锁的正是「层级先于权重」。
fn xu_scene() -> (Candidate, Candidate, Candidate) {
    let ct_exact = Candidate {
        text: "弱".into(),
        code: "xu".into(),
        weight: 9950, // 真实词频（混输引擎已不再加成，见 truncation_tier）
        is_common: true,
        is_exact_code: true,
        source: CandidateSource::CodeTable,
        ..Default::default()
    };
    let py_exact = Candidate {
        text: "需".into(),
        code: "xu".into(),
        weight: 69, // 6999 / 100
        is_common: true,
        source: CandidateSource::Pinyin,
        ..Default::default()
    };
    let ct_prefix = Candidate {
        text: "弹幕".into(),
        code: "xuaj".into(),
        weight: 1554, // 真实词频（混输引擎已不再加成，见 truncation_tier）
        is_common: true,
        source: CandidateSource::CodeTable,
        ..Default::default()
    };
    (ct_exact, py_exact, ct_prefix)
}

/// 混输：拼音精确档插在「码表精确」与「码表前缀补全」之间。
#[test]
fn mixed_pinyin_exact_sits_between_codetable_exact_and_prefix() {
    let (ct_exact, py_exact, ct_prefix) = xu_scene();
    // 故意以最不利顺序放入，确保结果由排序而非原序决定。
    let mut cands = [ct_prefix, py_exact, ct_exact];
    cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
    let order: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        order,
        vec!["弱", "需", "弹幕"],
        "混输应为「码表精确 → 拼音精确 → 码表前缀补全」"
    );
}

/// 反向锁一：`mixed=false`（纯拼音/纯码表）时本档不得生效。
/// 纯拼音下全体候选同为 Pinyin 来源，该键会退化成「is_common 优先」，把含生僻字的多字词
/// 硬降到全部常用单字之后 —— 那是明显回归。此处以码表权重序验证键未参与。
#[test]
fn non_mixed_keeps_weight_order_for_pinyin() {
    let (_, py_exact, ct_prefix) = xu_scene();
    let mut cands = [py_exact, ct_prefix];
    cands.sort_by(|a, b| candidate_display_order(a, b, false, false, XU_LEN));
    assert_eq!(
        cands[0].text, "弹幕",
        "非混输时应回落到纯权重序（1554 > 69），拼音精确档不得生效"
    );
}

/// 前缀短语排在码表前缀补全之后，且不依赖权重量级。
///
/// 二者比的是「码表词频 vs 用户设定的短语权重」，量纲不同，不能硬比，故拆成两档写死。
/// 用短语权重高于码表的取值，确保测的是档位而非权重。
#[test]
fn mixed_prefix_phrase_sits_below_codetable_prefix() {
    let (_, _, ct_prefix) = xu_scene();
    let prefix_phrase = Candidate {
        text: "短语正文".into(),
        weight: 99_999, // 远高于码表的 1554
        is_phrase: true,
        is_exact_code: false, // lookup_prefix 命中
        source: CandidateSource::Phrase,
        ..Default::default()
    };
    let mut cands = [prefix_phrase, ct_prefix];
    cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
    assert_eq!(
        cands[0].text, "弹幕",
        "码表前缀补全须先于前缀短语，且与两者的权重取值无关"
    );
}

/// 反向锁二：生僻拼音字（`is_common=false`）不提档，仍留在码表前缀补全之后。
/// 这一条锁住「不设条数上限、由检索范围把关」的设计——`xu` 有 329 条同音字，
/// 若生僻字一并提档会把码表候选整片挤出候选配额。
#[test]
fn mixed_rare_pinyin_char_stays_below_codetable_prefix() {
    let (_, py_exact, ct_prefix) = xu_scene();
    let rare = Candidate {
        text: "𬣙".into(),
        weight: 0,
        is_common: false,
        ..py_exact
    };
    let mut cands = [rare, ct_prefix];
    cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
    assert_eq!(
        cands[0].text, "弹幕",
        "生僻同音字不得越过码表前缀补全（否则 329 条同音字会挤爆配额）"
    );
}

/// 档内仍按权重竞争：两条都在拼音精确档时，高频字在前（本键在同档不表态）。
#[test]
fn mixed_within_pinyin_exact_tier_weight_still_decides() {
    let (_, py_exact, _) = xu_scene();
    let xu_low = Candidate {
        text: "序".into(),
        weight: 13, // 1321 / 100
        ..py_exact.clone()
    };
    let mut cands = [xu_low, py_exact];
    cands.sort_by(|a, b| candidate_display_order(a, b, false, true, XU_LEN));
    assert_eq!(cands[0].text, "需", "同档内仍按权重降序（69 > 13）");
}

/// 跨词库排序须以 `base_order` 隔离，`natural_order` 只在同档内当 tiebreaker。复刻 flypy「y」：
/// 主库「一」(base_order=0, natural_order 大) vs 一简次选库「有时」(base_order=2, natural_order 小)，
/// 「一」应稳居首位。两种模式（含/不含权重）都成立（此处权重均 0）。
#[test]
fn base_order_wins_over_cross_dict_natural_order() {
    let yi = cand_ordered("一", 0, 57285, 0);
    let youshi = cand_ordered("有时", 2, 24, 0);
    for ignore_weight in [false, true] {
        // 故意以「有时」在前的顺序放入，确保是排序而非原序决定结果。
        let mut cands = [youshi.clone(), yi.clone()];
        cands.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, false, XU_LEN));
        assert_eq!(
            cands[0].text, "一",
            "base_order=0 主库候选应排在 base_order=2 次选库候选之前（ignore_weight={ignore_weight}）"
        );
        assert_eq!(cands[1].text, "有时");
    }
}

/// natural 模式忽略权重：主库低权重条目（base_order 0）须排在次选库高权重条目（base_order 1）
/// 之前，与引擎 `by_natural` 一致；weight 模式则相反（高权重靠前）。证明 ignore_weight 生效。
#[test]
fn natural_mode_ignores_weight_weight_mode_respects_it() {
    let main_low = cand_ordered("主低", 0, 100, 1); // 主库、低权重
    let extra_high = cand_ordered("扩高", 1, 5, 999); // 次选库、高权重
    // weight 模式：权重降序主导 → 扩高(999) 在前。
    let mut w = [main_low.clone(), extra_high.clone()];
    w.sort_by(|a, b| candidate_display_order(a, b, false, false, XU_LEN));
    assert_eq!(w[0].text, "扩高", "weight 模式高权重应靠前");
    // natural 模式：忽略权重 → base_order 升序主导，主库(0) 在前。
    let mut n = [main_low, extra_high];
    n.sort_by(|a, b| candidate_display_order(a, b, true, false, XU_LEN));
    assert_eq!(
        n[0].text, "主低",
        "natural 模式忽略权重、按 base_order 升序，主库应靠前"
    );
}

/// 码表精确匹配须先于高权重前缀词组，且该优先级不得跨匹配层提拔。
///
/// 复刻古精86五笔 `usr`：简码「新」(usr, 11777) vs 前缀词组「新的」(usrq, 47487)。本函数会
/// 无条件重排全部候选，若不复刻 `is_exact_code` 键，引擎结果会被按纯权重推翻。前缀枚举短语
/// （`is_prefix=true`）仍留在精确层之下。
///
/// 精确档内部的先后由权重裁决：引导键组短语（`$SS`/`$AA`）与码表精确候选真比权重，先后取决于
/// 配置。下方两个方向都断言——只测一个方向证明不了裁决者是权重。
#[test]
fn exact_code_outranks_prefix_but_stays_within_match_layer() {
    let exact = Candidate {
        text: "新".into(),
        code: "usr".into(),
        weight: 11777,
        // natural_order 拉开档次，使 ignore_weight（natural 模式）下精确档内也有确定序，
        // 否则平局落到稳定排序取输入序，断言会随入表顺序漂移。
        natural_order: 10,
        is_exact_code: true,
        ..Default::default()
    };
    let prefix_word = Candidate {
        text: "新的".into(),
        code: "usrq".into(),
        weight: 47487,
        natural_order: 20,
        ..Default::default()
    };
    // 前缀枚举短语：is_prefix=true 使其落在更低匹配层。权重取真实量级（系统短语 800~2000），
    // 高于「新」也不该把它拉到精确候选之上——`cmp_match_layers` 在权重之前。
    let prefix_phrase = Candidate {
        text: "短语".into(),
        weight: 2000,
        is_phrase: true,
        is_prefix: true,
        ..Default::default()
    };
    // 引导键导航候选（$SS/$AA 组）：三个匹配层标志均为 false，须显式属精确档，
    // 否则会被每一条码表精确候选压下去——用户按引导键时首选会变成五笔单字。
    let guide_group = |weight: i32| Candidate {
        text: "组名".into(),
        weight,
        is_phrase: true,
        is_group: true,
        is_exact_code: true,
        ..Default::default()
    };
    // 故意以「最不利」的顺序放入，确保结果由排序而非原序决定。
    let order_of = |group: Candidate, ignore_weight: bool| -> Vec<String> {
        let mut cands = vec![
            prefix_phrase.clone(),
            prefix_word.clone(),
            exact.clone(),
            group,
        ];
        cands.sort_by(|a, b| candidate_display_order(a, b, ignore_weight, false, XU_LEN));
        cands.into_iter().map(|c| c.text).collect()
    };

    // ① 系统组短语的真实权重（1230）低于该简码 → 组名落到简码之后，但仍在精确档内、
    //    整体先于前缀词组。这是删掉 40M 后的新行为。
    assert_eq!(
        order_of(guide_group(1230), false),
        vec!["新", "组名", "新的", "短语"],
        "精确档内按权重：组短语 1230 < 简码 11777 ⇒ 排其后；两者仍先于前缀词组"
    );
    // ② 反向对照：把组短语权重调到简码之上，它就回到首位。证明裁决者确实是 weight。
    assert_eq!(
        order_of(guide_group(20000), false),
        vec!["组名", "新", "新的", "短语"],
        "精确档内按权重：组短语 20000 > 简码 11777 ⇒ 回到首位"
    );
    // ③ natural 模式忽略权重 ⇒ 精确档内退化为 base_order/natural_order（组名 0 < 新 10），
    //    两个权重取值结果相同——这一档的次序与权重无关。
    for w in [1230, 20000] {
        assert_eq!(
            order_of(guide_group(w), true),
            vec!["组名", "新", "新的", "短语"],
            "natural 模式下精确档按 natural_order，与权重 {w} 无关"
        );
    }
}
