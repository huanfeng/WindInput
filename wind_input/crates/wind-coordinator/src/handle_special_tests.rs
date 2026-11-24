//! 直达热键进入特殊模式/临拼的单元测试（无头 Coordinator + 临时 store）。
//! 不依赖引擎的行为（id→idx 定位、空前缀进入、半成品上屏）直接用空 `Coordinator`。
//! 依赖引擎查询的用例**同样属于这里**，但必须自造 data_dir（见 `special_completion::data_dir`）
//! ——别指望 `build_dev/data`，那是集成层「缺数据则整族静默跳过而计数照绿」的来源。
use super::*;
use crate::coordinator::Coordinator;
use std::sync::Arc;
use wind_candidate::Candidate;
use wind_config::Config;
use wind_store::Store;

/// 造一个含若干 overlay 方案的 data_dir。
///
/// 实例集合的真相源已是**方案文件目录**（带 `[overlay]` 段者入表），不再是
/// `config.schema.special_modes` 数组——故装置从「造 Config」改成「造方案文件」。
fn data_dir_with_overlays(tag: &str, ids: &[&str]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_special_hk_data_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    for id in ids {
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!(
                "[schema]\nid = \"{id}\"\nname = \"{id}\"\nhidden = true\n\
                     [engine]\ntype = \"codetable\"\n\
                     [overlay]\nkind = \"special\"\n"
            ),
        )
        .unwrap();
    }
    dir
}

fn coord_with(tag: &str, cfg: Config) -> Arc<Coordinator> {
    coord_with_overlays(tag, cfg, &[])
}

fn coord_with_overlays(tag: &str, cfg: Config, ids: &[&str]) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_special_hk_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let dir = (!ids.is_empty()).then(|| data_dir_with_overlays(tag, ids));
    Coordinator::new_headless_with_store(cfg, dir.as_deref(), store)
}

/// id → 下标定位走 overlay 注册表（按方案 id 字典序），未知 id 返回 None。
///
/// ⚠️ **只断言相对顺序，不断言绝对下标**：`installed_schemas` 会一并扫描
/// `Config::user_config_dir()/schemas`，开发机上那里可能装着真实的快符方案，
/// 绝对下标会随之平移。这不是测试将就，而是下标语义本身就是「注册表内的位次」。
#[test]
fn special_mode_idx_locates_by_registry_order() {
    let c = coord_with_overlays("idx", Config::default(), &["zz_c", "zz_a", "zz_b"]);
    let (a, b, cc) = (
        c.special_mode_idx("zz_a").expect("zz_a 应在表内"),
        c.special_mode_idx("zz_b").expect("zz_b 应在表内"),
        c.special_mode_idx("zz_c").expect("zz_c 应在表内"),
    );
    // 文件写入序是 c/a/b，注册表按 id 字典序 ⇒ a < b < c。这条同时证明顺序
    // 不来自任何「配置顺序」——那正是本次改造要消除的东西。
    assert!(a < b && b < cc, "应按 id 字典序：a={a} b={b} c={cc}");
    // 未知 id → None（分发点据此安全吞键，不 panic）
    assert_eq!(c.special_mode_idx("nope"), None);
}

/// 进入模式时把 `[overlay]` 段快照进 `State`，退出时清掉。
///
/// 快照是布局/注释/进入即展示三处的取值来源，见 `State::overlay_spec`。
#[test]
fn entering_snapshots_overlay_spec_and_exit_clears_it() {
    let c = coord_with_overlays("snap", Config::default(), &["zz_snap"]);
    let idx = c.special_mode_idx("zz_snap").expect("应在表内");
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    assert!(st.overlay_spec.is_none(), "进入前无快照");

    c.enter_special_mode(&mut st, idx, 0);
    let spec = st.overlay_spec.as_ref().expect("进入后应有快照");
    assert_eq!(spec.kind, "special", "快照应取自方案文件的 [overlay] 段");

    c.exit_special_mode(&mut st);
    assert!(st.overlay_spec.is_none(), "退出后快照必须清掉");
}

#[test]
fn commit_and_enter_special_writes_no_guide_prefix() {
    let c = coord_with_overlays("enter_empty", Config::default(), &["zz_rare"]);
    // 下标必须按 id 现取, 不能硬编码 0——见 `special_mode_idx_locates_by_registry_order`
    // 的说明: 注册表会一并收进用户目录里真实安装的 overlay 方案, 绝对下标随之平移。
    let idx = c.special_mode_idx("zz_rare").expect("zz_rare 应在表内");
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    // 空缓冲进入：无半成品可上屏 → 返回 UpdateComposition，组合区无引导符。
    let act = c.commit_and_enter_special_mode(&mut st, idx, 0);
    assert_eq!(st.active, Some(ModeKind::Special(idx)));
    assert!(
        st.special_prefix.is_empty(),
        "热键进入不应写引导符（special_prefix 应空）"
    );
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

#[test]
fn commit_and_enter_special_commits_pending_candidate() {
    let c = coord_with_overlays("enter_commit", Config::default(), &["zz_rare"]);
    // 同上：硬编码 0 会在装了真实 overlay 方案的开发机上落到别人的方案头上。
    // 本用例尤其致命——真实快符方案 `kf` 配了 `show_all_on_enter = true`，进入即装填
    // 候选，末尾的「候选应清空」断言必挂，而 CI 因无用户方案目录照常全绿。
    let idx = c.special_mode_idx("zz_rare").expect("zz_rare 应在表内");
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    // 模拟普通输入半成品：编码 + 高亮候选。
    st.input_buffer = "aa".to_string();
    st.candidates = vec![Candidate {
        text: "工".to_string(),
        ..Default::default()
    }];
    st.selected_index = 0;
    st.current_page = 0;
    let act = c.commit_and_enter_special_mode(&mut st, idx, 0);
    // 进入前的高亮候选应作为 InsertText 上屏，随后进入目标模式、组合区无引导符。
    match act {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "工"),
        other => panic!("应上屏半成品并进入模式，实际 {other:?}"),
    }
    assert_eq!(st.active, Some(ModeKind::Special(idx)));
    assert!(st.special_prefix.is_empty());
    assert!(st.candidates.is_empty());
}

#[test]
fn commit_and_enter_temp_pinyin_zero_keycode_has_no_prefix() {
    let c = coord_with("temp_zero", Config::default());
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    // key_code=0 哨兵：进入临拼但组合区无引导符（对齐特殊模式）。
    let _ = c.commit_and_enter_temp_pinyin(&mut st, 0, "pinyin".to_string());
    assert_eq!(st.active, Some(ModeKind::TempPinyin));
    assert!(
        st.temp_pinyin_prefix.is_empty(),
        "直达热键（key_code=0）进入临拼不应写引导符"
    );
}

/// 「顶屏 + 进模式」收尾按 top_commit_mode 分流（与顶码上屏统一）：
/// direct_commit（默认）+ 引导符新组合 → 真提交 + 延迟组合；新组合为空 → 直接真提交；
/// pre_confirm → InsertText 聚合。
#[test]
fn commit_then_new_composition_follows_top_commit_mode() {
    let c = coord_with("ctnc_direct", Config::default());
    match c.commit_then_new_composition("可能".to_string(), "`".to_string()) {
        KeyAction::CommitThenDeferComposition {
            commit_text,
            deferred_composition,
            ..
        } => {
            assert_eq!(commit_text, "可能");
            assert_eq!(deferred_composition, "`");
        }
        other => panic!("direct_commit 有新组合应走真提交+延迟组合，实际 {other:?}"),
    }
    match c.commit_then_new_composition("可能".to_string(), String::new()) {
        KeyAction::InsertText {
            text,
            new_composition,
            has_new_composition,
            ..
        } => {
            assert_eq!(text, "可能");
            assert!(new_composition.is_none() && !has_new_composition);
        }
        other => panic!("新组合为空应直接真提交，实际 {other:?}"),
    }

    let mut cfg = Config::default();
    cfg.input.top_commit_mode = wind_config::TopCommitMode::PreConfirm;
    let c = coord_with("ctnc_pre", cfg);
    match c.commit_then_new_composition("可能".to_string(), "`".to_string()) {
        KeyAction::InsertText {
            text,
            new_composition,
            has_new_composition,
            ..
        } => {
            assert_eq!(text, "可能");
            assert_eq!(new_composition.as_deref(), Some("`"));
            assert!(has_new_composition);
        }
        other => panic!("pre_confirm 应走 InsertText 聚合，实际 {other:?}"),
    }
}

/// 装一份最小常用字表（连同配套 registry），见 [`set_common_chars`]。
/// `retain_rare_admitted` 对空表整条早退（见那条测试），故凡要验过滤结果的用例都得先装表。
fn set_common(c: &Coordinator, chars: impl IntoIterator<Item = char> + Clone) {
    crate::charset_test_support::set_common_chars(c, chars);
}

/// ── 生僻字模式 ────────────────────────────────────────────────────────────
///
/// 这几条**刻意不依赖真实词库**：本 worktree 没有 `build_dev/data` 时，依赖词库的
/// 端到端用例会整族静默跳过而计数照绿（判据是耗时 0.00s）。模式的生命周期与准入
/// 判据是这轮的核心性质，不能挂在一个可能没跑的测试上。
mod rare_char {
    // 常用字表夹具上提到父模块：空码补全那族用例同样要装表，见 `set_common` 的说明。
    use super::set_common;
    use crate::coordinator::Coordinator;
    use crate::pipeline::ModeKind;
    use wind_config::Config;

    fn coord() -> std::sync::Arc<Coordinator> {
        Coordinator::new_headless(Config::default(), None)
    }

    /// 进入后模式标识为 rare_char，且缓冲被清空（顶字重开的语义）。
    #[test]
    fn enters_and_reports_its_own_mode() {
        let c = coord();
        let mut st = c.state.lock().unwrap();
        c.enter_rare_char_mode(&mut st, 0);
        assert_eq!(st.active, Some(ModeKind::RareChar));
        assert!(st.special_buffer.is_empty(), "顶字重开：进入时缓冲为空");
        assert!(st.input_buffer.is_empty(), "主输入缓冲一并清空");
        // ⚠️ special 专属状态必须留在默认值：凡从 overlay_spec 取值的地方都会因此
        // 落到默认档（布局/注释跟随全局、show_all_on_enter=false），这是刻意的。
        assert!(st.overlay_spec.is_none(), "生僻字模式没有 [overlay] 段可读");
    }

    /// 退出复用 `exit_special_mode`，模式与缓冲一并清干净。
    ///
    /// 留下一个没清的 `active` 会让后续按键继续走 overlay 分派，表现为「Esc 之后
    /// 打字没反应」——而 `exit_special_mode` 是两个模式共用的那一份，这里钉住它对
    /// 生僻字模式同样有效。
    #[test]
    fn exits_cleanly() {
        let c = coord();
        let mut st = c.state.lock().unwrap();
        c.enter_rare_char_mode(&mut st, 0);
        c.exit_special_mode(&mut st);
        assert_eq!(st.active, None);
        assert!(st.special_buffer.is_empty());
        assert!(st.preedit.is_empty());
    }

    /// ★ 配置真的走到了判据。
    ///
    /// 本仓的经典失效形态是「配置四层就位、消费点却在不可达的调用点上」——开关配了
    /// 毫无反应，且没有任何报错。这条测试从**配置**出发一路走到**候选列表**，
    /// 中间任何一环断掉都会红：
    /// `input.rare_char.include_blocks` → `charset_assembly` 装配期打 `in_rare` 标记
    /// → `EngineManager::charsets()` → `rare_admits` 的 extra 参数。
    #[test]
    fn include_blocks_config_reaches_the_verdict() {
        let mut cfg = Config::default();
        cfg.input.rare_char.include_blocks = vec!["emoji".to_string()];
        // ★ `emoji` 不是内置区块组，它的成员来自出厂 charsets/emoji.yaml。
        // 不给 data_dir 的话这个名字解析不出来，测试会以「配置没生效」的形态失败
        // ——与真实故障同形，看不出根因是夹具缺文件。
        let dir = crate::charset_test_support::charsets_only_data_dir("rare_include");
        let c = Coordinator::new_headless(cfg, Some(&dir));
        // 常用字表必须非空，否则 `retain_rare_admitted` 走「表未加载」那条早退。
        set_common(&c, ['我', '你', '好']);

        let mut cands = vec![
            cand("我"),   // 常用汉字 → 滤掉
            cand("龘"),   // 生僻汉字 → 留下
            cand("😀"),   // 域外字符，靠 include_blocks 纳入 → 留下
            cand("ㄅ"),   // 域外字符，没配它那一档 → 滤掉
            cand("你好"), // 多字词 → 滤掉
        ];
        c.retain_rare_admitted(&mut cands);
        let got: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(got, vec!["龘", "😀"], "配置未生效或判据接错");
    }

    /// ★★ 改 `input.rare_char.include_blocks` **不必切方案**就生效。
    ///
    /// 它在 input 段，改它不会把 `schema_dirty` 标脏。registry 若只在
    /// `reload_from_config`（受 schema_dirty 门控）里重装，用户改完毫无反应、要切一次
    /// 方案或重启才生效——本仓反复出现的「运行时镜像态没回灌」那类缺陷，且全程无报错。
    ///
    /// 本条从「改配置」出发直接看候选列表，中间那一步（`rebuild_charsets` 挂在无条件
    /// 重建 ConfigBundle 的路径上）断掉就会红。
    #[test]
    fn include_blocks_takes_effect_without_a_schema_change() {
        let dir = crate::charset_test_support::charsets_only_data_dir("rare_hot");
        let c = Coordinator::new_headless(Config::default(), Some(&dir));
        set_common(&c, ['我']);

        let mut before = vec![cand("😀")];
        c.retain_rare_admitted(&mut before);
        assert!(
            before.is_empty(),
            "没配之前 emoji 不该进（否则本条测不出东西）"
        );

        // 只动 input 段——schema 不脏。
        c.refresh_config_in_memory(|cfg| {
            cfg.input.rare_char.include_blocks = vec!["emoji".to_string()];
        });

        let mut after = vec![cand("😀")];
        c.retain_rare_admitted(&mut after);
        assert_eq!(
            after.len(),
            1,
            "改完 include_blocks 应当立即生效，不必切方案"
        );
    }

    /// 不配 include_blocks 时 emoji 进不来（与上一条互为对照，锁住「是配置起的作用」）。
    #[test]
    fn without_include_blocks_emoji_stays_out() {
        let c = coord();
        set_common(&c, ['我']);
        let mut cands = vec![cand("龘"), cand("😀")];
        c.retain_rare_admitted(&mut cands);
        let got: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(got, vec!["龘"], "没配区块时域外字符不该进来");
    }

    /// `$rare_char` 必须被进入门卫认作有效成员。
    ///
    /// 它不是真实方案，`ensure_schema` 对它必然失败 ⇒ 会被 `mix_members`（真实方案
    /// 列表）滤掉。若门卫只看那份列表，一个只配了 `$rare_char` 的 mix 就**进都进不去**
    /// ——按引导键毫无反应，且没有任何报错。
    #[test]
    fn rare_char_member_is_recognized_by_the_entry_gate() {
        let mut cfg = Config::default();
        let m = cfg
            .schema
            .mix_modes
            .get_mut(0)
            .expect("出厂应有内置 quick_mix");
        m.members = vec![wind_config::config::MIX_MEMBER_RARE_CHAR.to_string()];
        let c = Coordinator::new_headless(cfg, None);

        assert!(c.mix_has_rare_char(0), "门卫须认得生僻字成员");
        assert!(
            c.mix_members(0).is_empty(),
            "它不是真实方案，不该出现在真实方案列表里"
        );
        // 两者合起来才是完整判据：真实方案列表为空，但门卫仍应放行。
        assert!(
            !c.mix_has_quick_input(0),
            "本用例已把 members 换成只有生僻字，不应再含 quick 来源"
        );
    }

    /// 生僻字成员**不该**触发强制竖排。
    ///
    /// 竖排是 `mix_has_quick_input` 的语义（内置来源的候选是长文本，横排放不下），
    /// 而生僻字成员产出的是普通单字。两个谓词合并的话，只配生僻字的 mix 会平白变竖排。
    #[test]
    fn rare_char_member_does_not_force_vertical() {
        let mut cfg = Config::default();
        let m = cfg.schema.mix_modes.get_mut(0).unwrap();
        m.members = vec![wind_config::config::MIX_MEMBER_RARE_CHAR.to_string()];
        let c = Coordinator::new_headless(cfg, None);
        assert!(c.mix_has_rare_char(0));
        assert!(
            !c.mix_has_quick_input(0),
            "生僻字成员不属于 quick 来源族，不应带来竖排"
        );
    }

    /// ── 真实词库端到端 ──────────────────────────────────────────────────
    ///
    /// ⚠️ 上面那些用例走的都是自造夹具，验的是判据与生命周期；**真实词库下这个模式
    /// 到底还剩几个候选**是另一回事，只有这里能答。缺数据时整族静默跳过而计数照绿
    /// （本仓的老坑），故守卫里带 eprintln，别只看绿灯。
    /// 补数据：worktree 根建 `build_dev` 符号链接指向主仓，或跑 `dev.ps1 gd`。
    fn wubi_coord(tag: &str) -> Option<std::sync::Arc<Coordinator>> {
        let d =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
            eprintln!("跳过 {tag}：五笔词库不存在（build_dev/data 缺失）");
            return None;
        }
        // ⚠️ charsets/ 缺失时**常用性判定整个不生效**（无人表态 ⇒ 一切兜底判常用）
        // ⇒ 生僻字模式一条候选都留不下，测试会以「过滤写错了」的形态失败。
        // 它是构建产物的一部分（`Copy-Item data -Recurse`），过时的 build_dev 才会缺。
        assert!(
            d.join("charsets").is_dir(),
            "build_dev/data/charsets 缺失（构建产物过时），重跑一次数据组装"
        );
        let mut cfg = Config::default();
        cfg.schema.active = "wubi86".into();
        cfg.schema.available = vec!["wubi86".into()];
        Some(Coordinator::new_headless(cfg, Some(&d)))
    }

    /// ★ 下推给引擎的准入与调用方兜底的过滤**必须同源**。
    ///
    /// 两者现在都走 `wind_candidate::rare_admits`，但它们是两处独立的调用点，
    /// 将来任何一侧「顺手改一下判据」都不会有编译错误。失效是静默且方向相反的：
    /// 引擎侧更严 ⇒ 调用方想要的候选压根没产出；引擎侧更松 ⇒ 白占配额还是被滤掉。
    #[test]
    fn engine_side_admit_matches_caller_side_filter() {
        let c = coord();
        set_common(&c, ['工', '水']);
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        c.enter_rare_char_mode(&mut st, 0);
        let f = c.rare_admit_fn(&st).expect("生僻字模式应给出准入闭包");
        c.exit_special_mode(&mut st);
        drop(st);
        // 逐条比对两侧结论：常用字、生僻字、多字词、空串、空白。
        for t in ["工", "水", "沝", "龘", "工作", "", " "] {
            let engine_side = f(t);
            let mut v = vec![cand(t)];
            c.retain_rare_admitted(&mut v);
            let caller_side = !v.is_empty();
            assert_eq!(
                engine_side, caller_side,
                "{t:?} 两侧结论不一致：引擎侧={engine_side} 调用方={caller_side}"
            );
        }
    }

    /// 常用字表未加载时不下推准入——那时全体候选都会被判成「非常用」，
    /// 筛了等于没筛，还白付一次判定。与 `retain_rare_admitted` 的同款保护成对。
    #[test]
    fn no_admit_pushdown_when_common_table_is_missing() {
        let c = coord();
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        c.enter_rare_char_mode(&mut st, 0);
        assert!(c.rare_admit_fn(&st).is_none(), "常用字表为空时不该下推准入");
        c.exit_special_mode(&mut st);
    }

    /// 非生僻字模式不下推——普通 special 模式的候选不该被筛掉。
    #[test]
    fn no_admit_pushdown_outside_rare_mode() {
        let c = coord();
        set_common(&c, ['工']);
        let st = c.state.lock().unwrap();
        assert!(c.rare_admit_fn(&st).is_none(), "非生僻字模式不该下推准入");
    }

    /// 拼音方案下的生僻字模式。与 `wubi_coord` 分开是因为两者暴露的问题不同：
    /// 码表方案的候选总数够不着取数上限，「截断 → 过滤」那个缺陷在五笔端到端测试里
    /// **完全看不出来**。
    fn pinyin_coord(tag: &str) -> Option<std::sync::Arc<Coordinator>> {
        let d =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
        if !d
            .join("schemas/pinyin/rime_frost.dict.merged.wdat")
            .exists()
        {
            eprintln!("跳过 {tag}：拼音词库不存在（build_dev/data 缺失）");
            return None;
        }
        // ⚠️ charsets/ 缺失时**常用性判定整个不生效**（无人表态 ⇒ 一切兜底判常用）
        // ⇒ 生僻字模式一条候选都留不下，测试会以「过滤写错了」的形态失败。
        // 它是构建产物的一部分（`Copy-Item data -Recurse`），过时的 build_dev 才会缺。
        assert!(
            d.join("charsets").is_dir(),
            "build_dev/data/charsets 缺失（构建产物过时），重跑一次数据组装"
        );
        let mut cfg = Config::default();
        cfg.schema.active = "pinyin".into();
        cfg.schema.available = vec!["pinyin".into()];
        Some(Coordinator::new_headless(cfg, Some(&d)))
    }

    /// ★★★ 高频音节在修复前只能出个位数候选。
    ///
    /// `yi` 这个音有 1183 个非常用字，而取数上限 100 施加在过滤**之前**、引擎又按
    /// 常用度排序 ⇒ 修复前只漏出 4 个（漏掉 99.7%）。用户会认为功能坏了。
    ///
    /// 断言取「远多于修复前」而不是某个精确条数：条数随词库版本变，而 4 → 上百这个
    /// 数量级差异不会因换词库而消失。
    #[test]
    fn pinyin_high_frequency_syllable_is_not_starved_by_the_limit() {
        let Some(c) = pinyin_coord("yi") else { return };
        let got = rare_cands(&c, "yi");
        assert!(
            got.len() > 50,
            "高频音节 yi 应能出大量生僻字，实得 {} 条：{:?}",
            got.len(),
            got.iter().take(10).collect::<Vec<_>>()
        );
        assert!(got.iter().all(|t| t.chars().count() == 1), "仍须只出单字");
    }

    /// 候选总数**够不着**上限的码，不受重取影响（也不该白付一次重取）。
    ///
    /// `zhui` 全部候选 72 条 < 100，第一次就取全了；过滤后 57 条也够一页，
    /// 于是 `refill_rare_if_short` 直接返回。这条钉住「常见情况不劣化」。
    #[test]
    fn pinyin_short_candidate_list_needs_no_refill() {
        let Some(c) = pinyin_coord("zhui") else {
            return;
        };
        let got = rare_cands(&c, "zhui");
        assert!(got.len() >= 10, "zhui 应有足量生僻字，实得 {}", got.len());
        assert!(
            got.contains(&"沝".to_string()),
            "沝 是 zhui 的生僻字，应在列"
        );
    }

    /// ★ 码表方案**也**受益于重取——这条测试写下时我以为它会证明「码表不变」，
    /// 结果反过来证伪了那个假设：`ii` 从 1 条变 15 条。
    ///
    /// 留着它是为了钉住这个认知：截断的影响取决于**同码候选有多少**，与方案类型
    /// 无关。二级简码 `ii` 同码字多，切掉大半；四码全码 `sivg` 只有一个解，不受影响
    /// （由 `real_dict_keeps_only_the_rare_homograph` 钉住那一侧）。
    #[test]
    fn codetable_short_code_also_gains_from_refill() {
        let Some(c) = wubi_coord("ii-refill") else {
            return;
        };
        let got = rare_cands(&c, "ii");
        assert!(
            got.len() > 5,
            "二级简码 ii 的同码生僻字远不止一个，实得 {got:?}"
        );
        assert!(got.contains(&"沝".to_string()), "原有的沝仍须在列");
        assert!(got.iter().all(|t| t.chars().count() == 1), "仍须只出单字");
    }

    /// 取某个码在生僻字模式下的候选文本。
    fn rare_cands(c: &Coordinator, code: &str) -> Vec<String> {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        c.enter_rare_char_mode(&mut st, 0);
        st.special_buffer = code.to_string();
        st.special_cursor = code.len();
        c.update_special_candidates(&mut st);
        let out = st.candidates.iter().map(|x| x.text.clone()).collect();
        c.exit_special_mode(&mut st);
        out
    }

    /// 取某个码在**普通输入路**的候选文本（对照组）。
    fn normal_cands(c: &Coordinator, code: &str) -> Vec<String> {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        st.input_buffer = code.to_string();
        c.update_candidates(&mut st);
        let out = st.candidates.iter().map(|x| x.text.clone()).collect();
        st.input_buffer.clear();
        st.candidates.clear();
        out
    }

    /// ★ 真实五笔词库下，同码位的常用字被滤掉、生僻字留下。
    ///
    /// `sivg` 是本仓反复出现的样本：常用「档」与生僻「桜」同码，检索范围放宽那一轮
    /// 也用的它。这里精确断言而非只验性质——它稳定到足以当回归锁，词库升级真把它改了
    /// 也该有人来看一眼。
    #[test]
    fn real_dict_keeps_only_the_rare_homograph() {
        let Some(c) = wubi_coord("sivg") else { return };
        assert_eq!(
            normal_cands(&c, "sivg"),
            vec!["档"],
            "对照组：普通输入只出常用字"
        );
        assert_eq!(
            rare_cands(&c, "sivg"),
            vec!["桜"],
            "生僻字模式应只留同码的生僻字"
        );
    }

    /// ★ 真实词库下的性质断言：候选**非空**、**全是单字**、**全部判非常用**。
    ///
    /// 非空这一条最要紧——「严格过滤空了就空着」是设计取舍，但如果连 `a` 这种一级简码
    /// 都滤空了，这个模式就是个摆设，而所有自造夹具的测试都不会红。
    #[test]
    fn real_dict_yields_single_uncommon_chars() {
        let Some(c) = wubi_coord("a") else { return };
        let got = rare_cands(&c, "a");
        assert!(!got.is_empty(), "一级简码下不该一个候选都没有");

        let cc = c.common_chars.read().unwrap();
        let cs = c.engine_mgr.charsets();
        for t in &got {
            assert!(
                wind_candidate::single_markable_char(t).is_some(),
                "「{t}」不是单字——严格只出单字这条没生效"
            );
            assert!(
                !cc.is_string_common(t, &cs),
                "「{t}」是常用字，不该出现在生僻字模式"
            );
        }
        // 普通输入路里那些常用字，一个都不该漏进来。
        let normal = normal_cands(&c, "a");
        assert!(normal.len() > got.len(), "普通候选应远多于生僻候选");
        for t in ["工", "戈", "式"] {
            assert!(normal.contains(&t.to_string()), "对照组应含常用字「{t}」");
            assert!(
                !got.contains(&t.to_string()),
                "常用字「{t}」漏进了生僻字模式"
            );
        }
    }

    /// 多字词一个都不该进来（`ii` 的普通候选里有「洋洋洒洒」这类）。
    #[test]
    fn real_dict_drops_multi_char_words() {
        let Some(c) = wubi_coord("ii") else { return };
        let normal = normal_cands(&c, "ii");
        assert!(
            normal.iter().any(|t| t.chars().count() > 1),
            "对照组里应当有多字词，否则这条测试什么都没验到"
        );
        for t in rare_cands(&c, "ii") {
            assert_eq!(
                wind_candidate::semantic_units(&t),
                1,
                "「{t}」是多字词，不该进生僻字模式"
            );
        }
    }

    /// ★ 生僻字模式不污染普通输入路：进出一趟之后，同一个码的普通候选逐字节不变。
    ///
    /// 「完全不记词频」那条取舍的端到端体现——模式内选过什么都不该改变正常输入的顺序。
    #[test]
    fn real_dict_normal_path_unaffected_by_the_mode() {
        let Some(c) = wubi_coord("roundtrip") else {
            return;
        };
        let before = normal_cands(&c, "ii");
        let _ = rare_cands(&c, "ii");
        let after = normal_cands(&c, "ii");
        assert_eq!(before, after, "进出生僻字模式不应改变普通输入的候选");
    }

    fn cand(text: &str) -> wind_candidate::Candidate {
        wind_candidate::Candidate {
            text: text.to_string(),
            ..Default::default()
        }
    }

    /// ★ 常用字表未加载时**整条不过滤**，而不是放行全部。
    ///
    /// 判据与 `apply_filter` 那道 `common_chars.is_empty()` 同源：表没加载时全体候选
    /// 都会被判「非常用」，准入于是放行**全部**候选——那不是生僻字模式，是一个没有
    /// 过滤的普通列表，而用户完全看不出区别（他会以为这个码下所有字都是生僻字）。
    #[test]
    fn no_filtering_when_common_table_is_missing() {
        let c = coord();
        let mut cands = vec![
            wind_candidate::Candidate {
                text: "我".to_string(),
                ..Default::default()
            },
            wind_candidate::Candidate {
                text: "你好".to_string(),
                ..Default::default()
            },
        ];
        c.retain_rare_admitted(&mut cands);
        assert_eq!(cands.len(), 2, "表未加载时不得过滤（宁可不滤，不给假列表）");
    }
}
/// ── 特殊模式的空码补全 ────────────────────────────────────────────────────
///
/// 「精确匹配 + 空码补全」（`single_code_input` + `single_code_complete`）下，当前码
/// 无精确解、更长编码有解时，引擎把更长那条备进 `ConvertResult::completion_hints`，
/// 由 `update_special_candidates` 判空后择一采纳。这一族钉的是**采纳路上的几道过滤
/// 不能把它吃掉**，以及生僻准入那道只对生僻字模式生效。
///
/// ⚠️ 全族必须先 `set_common` 装表：生僻准入对空表整条早退（见
/// `no_filtering_when_common_table_is_missing`），不装表的话那道过滤根本没跑，用例
/// 照绿而缺陷原样活着 —— 论坛 t125 报的「快符空码不显示后续编码」正是这样躲过了
/// 一整轮自动化测试，只在装了字表的真机上复现。
///
/// 刻意**不依赖 `build_dev/data`**：自造两张两条词条的小码表，缺数据也不会静默跳过。
mod special_completion {
    use super::set_common;
    use crate::coordinator::Coordinator;
    use crate::pipeline::ModeKind;
    use std::sync::Arc;
    use wind_config::Config;

    /// overlay（快符）方案 id。`zz_` 前缀避开开发机上真实安装的方案。
    const KF: &str = "zz_kfx";
    /// 主方案 id：生僻字模式用的是**活跃方案**的引擎，故它也要开这两个开关。
    const MAIN: &str = "zz_main";

    /// 造数据目录：一个主方案 + 一个 overlay 方案，两者都是「精确匹配 + 空码补全」，
    /// 码表里 `x` 本身**没有编码**，只有 `xab` / `xcd` 这些更长的。
    ///
    /// ⚠️ 开关写在**方案自己名下**而不是全局：overlay 方案不继承全局
    /// `schema.codetable`（`EngineManager::codetable_baseline` 按 `[overlay]` 段分流，
    /// 取内置基线），写全局对它一点作用都没有。
    fn data_dir(tag: &str) -> std::path::PathBuf {
        // 带 pid：并发跑同一条用例时，一方的 `remove_dir_all` 会在另一方正扫
        // `schemas/` 时把目录抽走，表现为偶发红在下面那句 `expect` 上、而错误信息
        // 完全指不到真因。同 `handle_punct.rs` 夹具的做法。
        let dir = std::env::temp_dir().join(format!(
            "wind_special_completion_{}_{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let schemas = dir.join("schemas");
        std::fs::create_dir_all(schemas.join(KF)).unwrap();
        std::fs::create_dir_all(schemas.join(MAIN)).unwrap();
        let codetable = "[engine.codetable]\nmax_code_length = 4\n\
                             single_code_input = true\nsingle_code_complete = true\n";
        std::fs::write(
            schemas.join(format!("{KF}.schema.toml")),
            format!(
                "[schema]\nid = \"{KF}\"\nname = \"快符测试\"\nhidden = true\n\
                     [engine]\ntype = \"codetable\"\n{codetable}\
                     [overlay]\nkind = \"special\"\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{KF}/{KF}.dict.yaml\"\ndefault = true\n"
            ),
        )
        .unwrap();
        // 快符表里是**符号**——正是 `rare_admits` 恒判 false 的那一类：它落在默认字表
        // 管辖域之外，`is_string_common` 对它恒真（语义是「忽略」而非「常用」），
        // 又没被 `include_blocks` 点名 ⇒ 生僻准入一施加就整批清空。
        std::fs::write(
            schemas.join(KF).join(format!("{KF}.dict.yaml")),
            "---\nname: kfx\nversion: \"1\"\n...\n——\txab\n……\txcd\n",
        )
        .unwrap();
        std::fs::write(
            schemas.join(format!("{MAIN}.schema.toml")),
            format!(
                "[schema]\nid = \"{MAIN}\"\nname = \"主测试\"\n\
                     [engine]\ntype = \"codetable\"\n{codetable}\
                     [[dictionaries]]\nid = \"main\"\npath = \"{MAIN}/{MAIN}.dict.yaml\"\ndefault = true\n"
            ),
        )
        .unwrap();
        // 同一个码前缀下**一常用一生僻**：生僻字模式那条对照据此分辨准入是真的在判，
        // 而不是「全留」或「全滤」。
        std::fs::write(
            schemas.join(MAIN).join(format!("{MAIN}.dict.yaml")),
            "---\nname: main\nversion: \"1\"\n...\n我\txab\n龘\txcd\n",
        )
        .unwrap();
        dir
    }

    fn coord(tag: &str) -> Arc<Coordinator> {
        let dir = data_dir(tag);
        let mut cfg = Config::default();
        cfg.schema.available = vec![MAIN.into()];
        cfg.schema.active = MAIN.into();
        Coordinator::new_headless(cfg, Some(&dir))
    }

    /// 敲一个无精确解的码，返回当前候选文本。
    fn cands_for(
        c: &Coordinator,
        enter: impl FnOnce(&Coordinator, &mut crate::coordinator::State),
    ) -> Vec<String> {
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;
        enter(c, &mut st);
        st.special_buffer = "x".to_string();
        st.special_cursor = 1;
        c.update_special_candidates(&mut st);
        let out = st.candidates.iter().map(|x| x.text.clone()).collect();
        c.exit_special_mode(&mut st);
        out
    }

    /// ★ 主用例（论坛 t125 的回归）：快符模式下空码补全必须出得来。
    ///
    /// 缺陷形态：补全旁路直接调了裸的 `retain_rare_admitted`，生僻准入于是施加到了
    /// **所有**特殊模式上。快符候选是符号 ⇒ 准入恒判 false ⇒ 整批滤空 ⇒ 用户看到
    /// 「有 `xab` 的编码，敲 `x` 却什么都不显示」。
    #[test]
    fn quick_symbol_completion_survives_rare_admission() {
        let c = coord("kf");
        set_common(&c, ['我']);
        let idx = c
            .special_mode_idx(KF)
            .expect("快符方案应在 overlay 注册表内");
        let got = cands_for(&c, |c, st| {
            c.enter_special_mode(st, idx, 0);
        });
        assert_eq!(
            got,
            vec!["——".to_string()],
            "快符模式下 `x` 应补出更长编码 `xab` 的候选，实得 {got:?}"
        );
    }

    /// ★ 反向对照：生僻字模式下，同一条旁路**仍然**要筛。
    ///
    /// 没有这一条，把守卫连同过滤一起删掉也能让上一条绿——而那会让生僻字模式补出
    /// 常用字来（原注释「本码没有生僻字时，补出来的却是个常用字」说的就是它）。
    /// 这里一常用（我 `xab`）一生僻（龘 `xcd`）同码前缀，留下的必须只有生僻那个。
    #[test]
    fn rare_char_mode_still_filters_its_completion() {
        let c = coord("rare");
        set_common(&c, ['我']);
        let got = cands_for(&c, |c, st| {
            c.enter_rare_char_mode(st, 0);
        });
        assert_eq!(
            got,
            vec!["龘".to_string()],
            "生僻字模式的补全候选应只剩生僻字（常用的「我」要被滤掉），实得 {got:?}"
        );
    }

    /// **夹具自检**，不是回归用例：缺陷存在时它照绿。上面两条主用例的分流全靠
    /// 「两种模式各自查的是哪张码表」，这里把那件事本身钉住。
    ///
    /// 光断言 `st.active` 是在复述 `enter_*` 自己那行赋值，测不出东西；真正会让主
    /// 用例**静默空转**的是引擎选错表——`special_mode_idx` 在装了真实 overlay 方案的
    /// 开发机上指到别人的方案、或生僻字模式没落回活跃方案，两条主用例都会变成在错
    /// 误的码表上断言。故断言落在 `overlay_engine_schema` 上。
    #[test]
    fn the_two_modes_query_their_own_schema() {
        let c = coord("modes");
        let idx = c
            .special_mode_idx(KF)
            .expect("快符方案应在 overlay 注册表内");
        let mut st = c.state.lock().unwrap();
        st.chinese_mode = true;

        c.enter_special_mode(&mut st, idx, 0);
        assert_eq!(st.active, Some(ModeKind::Special(idx)));
        assert_eq!(
            c.overlay_engine_schema(&st).as_deref(),
            Some(KF),
            "快符模式该查 overlay 方案自带的那张码表"
        );
        c.exit_special_mode(&mut st);

        c.enter_rare_char_mode(&mut st, 0);
        assert_eq!(st.active, Some(ModeKind::RareChar));
        assert_eq!(
            c.overlay_engine_schema(&st).as_deref(),
            Some(MAIN),
            "生僻字模式该回落到**活跃方案**的码表，而不是任何 overlay 方案"
        );
        c.exit_special_mode(&mut st);
    }
}
