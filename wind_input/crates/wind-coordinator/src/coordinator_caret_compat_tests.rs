//! caret_use_top 兼容变换：微信等 WebView 下把候选窗定位基准从 rect.bottom 改为 rect.top。
use super::*;

fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

fn caret(y: i32, height: i32) -> CaretData {
    CaretData {
        x: 100,
        y,
        height,
        composition_start_x: 100,
        composition_start_y: y,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    }
}

/// 组合起点锚定的 500px 校验必须只在 caret 与组合起点同源时生效。
fn far_comp_start(source: i32) -> CaretData {
    // 桌面输入实测形态：caret (0,1388) 是 GUI 回退取到的任务栏残留光标，
    // compStart (473,217) 才是真实组合位置，两者 dy=1171 ≥500px。
    CaretData {
        x: 0,
        y: 1388,
        height: 20,
        composition_start_x: 473,
        composition_start_y: 217,
        source,
        composition_rect: None,
    }
}

fn lock_comp_start_with(source: i32) -> bool {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "ab".to_string(); // 置为组合中，否则 caret_update 只落缓存
    }
    c.handle_caret_update(&far_comp_start(source));
    c.composition_start.lock().unwrap().2
}

#[test]
fn non_tsf_caret_skips_composition_start_distance_check() {
    // caret 来自 GUI 回退时，它与组合起点根本不是一个语义域，距离比较无意义。
    // 旧行为在此把唯一正确的组合起点当异常丢弃了（桌面输入定位到任务栏的直接原因）。
    assert!(
        lock_comp_start_with(wind_ipc::protocol::caret_source::GUI_CARET),
        "caret 为 GUI 回退源时应跳过距离校验、直接锁定组合起点"
    );
}

#[test]
fn tsf_caret_still_rejects_far_composition_start() {
    // 反向对照：同样的距离，caret 若来自 TSF 域则 500px 保护仍须生效——同源却相差离谱，
    // 那才是它本来要抓的坐标系不一致。缺了这条，上面那个测试无法区分「按来源放行」
    // 与「干脆不再校验」，把保护删光也能让它变绿。
    assert!(
        !lock_comp_start_with(wind_ipc::protocol::caret_source::TSF_SELECTION),
        "TSF 同源时超 500px 仍应判为坐标系不一致而丢弃"
    );
}

/// 宿主默认位置（`TSF_DEFAULT_POS`）同样跳过 500px 同源校验。
///
/// 它出自 TSF context 却刻意判为非 TSF 域（见 `caret_source::is_tsf`）。与
/// `tsf_caret_still_rejects_far_composition_start` 互为反例，两条一起才能区分「按来源放行」
/// 与「干脆不再校验」。
#[test]
fn tsf_default_pos_caret_skips_composition_start_distance_check() {
    assert!(
        lock_comp_start_with(wind_ipc::protocol::caret_source::TSF_DEFAULT_POS),
        "宿主默认位置不是真插入点，与组合起点不同源，须跳过距离校验直接锁定"
    );
}

/// Illustrator 画布文字的形态：宿主报「没有插入点」，选区矩形 / 组合起点 / 组合矩形三者同值，
/// 落在工作区右下角最后一个像素（见 `docs/architecture/caret-position-host-compat-notes.md`
/// 的「非 TSF 坐标来源」）。`height` 由 DLL 合成后上报，本参数用来把这一点单独测出来。
fn illustrator_default_pos(height: i32) -> CaretData {
    CaretData {
        x: 2559,
        y: 1367,
        height,
        composition_start_x: 2559,
        composition_start_y: 1367,
        source: wind_ipc::protocol::caret_source::TSF_DEFAULT_POS,
        composition_rect: Some((2559, 1367, 2560, 1367)),
    }
}

fn composing_and_awaiting_first_show() -> Arc<Coordinator> {
    let c = coord();
    c.state.lock().unwrap().input_buffer = "ab".to_string();
    *c.pending_first_show.lock().unwrap() = true;
    c
}

/// 宿主默认位置不得置 `caret_cache_verified`，否则下一次组合会先在旧位置闪一下。
///
/// 该标志问的是「缓存对应当前插入点吗」，而这条来源按定义是「宿主不知道插入点在哪」；
/// 置真会让下次首显走 25ms 短兜底，先拿组合前那次 `gui_caret` 首显再跳到默认位置。
/// 组合前与组合期两个入口都能置位，`is_tsf` 判否只挡住了前者。
///
/// `was_verified=true` 那两组是核心：只拦置真的单向写法在 `false` 起步时同样全绿，而
/// 「先真后假」正是本宿主的日常路径。对照组同坐标只改 `source`。
#[test]
fn host_default_pos_must_not_verify_the_caret_cache() {
    let after = |source: i32, was_verified: bool| {
        let c = coord();
        c.state.lock().unwrap().input_buffer = "a".to_string(); // composing ⇒ 走组合期那条路
        c.caret_cache_verified
            .store(was_verified, std::sync::atomic::Ordering::Relaxed);
        // 另两个标志必须预置成 true，否则下面那两条断言是假的：它们的初值就是
        // false，`store(false)` 被跳过时结果一样，断言照样绿（变异检验实测过这一点）。
        c.caret_cache_is_idle_report
            .store(true, std::sync::atomic::Ordering::Relaxed);
        c.awaiting_first_authority_after_focus
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut data = illustrator_default_pos(20);
        data.source = source;
        c.handle_caret_update(&data);
        (
            c.caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed),
            c.caret_cache_is_idle_report
                .load(std::sync::atomic::Ordering::Relaxed),
            c.awaiting_first_authority_after_focus
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    };
    const DEFAULT_POS: i32 = wind_ipc::protocol::caret_source::TSF_DEFAULT_POS;
    const REAL: i32 = wind_ipc::protocol::caret_source::TSF_SELECTION;

    assert!(
        !after(DEFAULT_POS, false).0,
        "宿主默认位置不对应任何插入点；置真会让下一次组合退化成 25ms 短兜底、先闪一下旧坐标"
    );
    assert!(
        !after(DEFAULT_POS, true).0,
        "★ 更要紧的一半：上一帧真插入点留下的 true 必须被**清掉**——走到那一行时缓存已被\
             默认位置覆盖，留着 true 就是同一句谎换个来路，闪跳照旧（画布↔面板来回切必踩）"
    );
    assert!(
        after(REAL, false).0,
        "对照组：真插入点照常置真（否则本测试证明不了差别来自 source）"
    );

    // 同段另两个标志刻意保持无条件，各自问的都不是「是不是插入点」。
    // 没有这两条，后人把三者一起挪进条件里不会有任何测试变红，而后果是默认位置宿主上
    // `awaiting_first_authority_after_focus` 永久为真 ⇒ idle_anchor 逃生口永久锁死。
    let (_, idle_report, awaiting) = after(DEFAULT_POS, false);
    assert!(
        !idle_report,
        "缓存自此装的是本次组合的位置，不再是「组合前的空闲上报」——与来源无关"
    );
    assert!(
        !awaiting,
        "默认位置同样是「本次焦点下已拿到过一帧组合期坐标」，不清位会白丢 idle_anchor 提速"
    );
}

/// 宿主默认位置必须当场消费首显等待，而不是被丢弃后空等兜底到期——到期时手里仍是
/// 同一个退化矩形，没有任何东西可等。
#[test]
fn tsf_default_pos_consumes_pending_first_show_at_once() {
    let c = composing_and_awaiting_first_show();
    c.handle_caret_update(&illustrator_default_pos(20));
    assert!(
        !*c.pending_first_show.lock().unwrap(),
        "首显等待必须被这一帧消费掉，否则又要空等一整个兜底窗口"
    );
    assert!(
        *c.candidate_shown.lock().unwrap(),
        "消费首显等待就要真的把候选窗下发出去，否则等待白消费了"
    );
    assert_eq!(
        *c.composition_start.lock().unwrap(),
        (2559, 1367, true),
        "锚点要锁到宿主给的那个位置——候选窗落到右下角任务栏之上靠的就是它\
             （place_window 的上翻 + 左移 + 钳到 rcWork 会自己把它收进屏内）。\
             ⚠ 本条**区分不了走的是哪一支**：Illustrator 的 compStart 与 caret 同值，\
             跳过校验与通过 500px 校验结果相同。哪一支由 \
             `tsf_default_pos_caret_skips_composition_start_distance_check` 单独钉"
    );
    let st = c.state.lock().unwrap();
    assert_eq!(
        (st.caret_x, st.caret_y, st.caret_source),
        (
            2559,
            1367,
            wind_ipc::protocol::caret_source::TSF_DEFAULT_POS
        ),
        "坐标与来源都要如实落缓存：来源一旦被改写成 TSF 权威源，\
             跟行/漂移校正就会拿这个恒定不动的屏幕角落像素当插入点"
    );
}

/// 高度 0 的帧照旧整条丢弃——这正是 DLL 侧必须合成一个高度的理由。
///
/// 两道闸门各自都能吃掉它：`handle_caret_update` 入口的 `data.height == 0` 与
/// `caret_is_valid` 的 `height > 0`，只放宽其中一道本测试不会变色。
#[test]
fn a_zero_height_frame_is_dropped_even_for_tsf_default_pos() {
    let c = composing_and_awaiting_first_show();
    c.handle_caret_update(&illustrator_default_pos(0));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "h=0 是退化矩形的原值，服务端必须照旧丢弃；DLL 得自己补高度"
    );
    assert!(
        !*c.candidate_shown.lock().unwrap(),
        "被丢弃的帧不得把候选窗下发出去"
    );
}

#[test]
fn overflow_sentinel_composition_start_is_rejected_without_panicking() {
    let c = coord();
    c.state.lock().unwrap().input_buffer = "ab".to_string();
    c.handle_caret_update(&CaretData {
        x: 100,
        y: 200,
        height: 20,
        composition_start_x: i32::MIN,
        composition_start_y: 200,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    assert!(
        !c.composition_start.lock().unwrap().2,
        "i32::MIN 级脏数据必须被距绝，不得在距离计算中溢出 panic"
    );
}

#[test]
fn caret_use_top_shifts_y_to_top_and_keeps_real_line_height() {
    let c = coord();
    // 模拟焦点进程命中 caret_use_top 规则。
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1234,
        caret_use_top: true,
        ..Default::default()
    };
    c.handle_caret_update(&caret(200, 20));
    let s = c.state.lock().unwrap();
    // bottom(200) → top：200 - 20 = 180（下方显示锚此稳定值）。
    assert_eq!(s.caret_y, 180);
    // 保留真实行高 20（> 下限）供上方显示避让正文，而非压成 1。
    assert_eq!(s.caret_height, 20);
}

#[test]
fn caret_use_top_degenerate_height_floored_to_min() {
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1234,
        caret_use_top: true,
        ..Default::default()
    };
    // 退化帧 height=1：top 仍稳定（bottom-1），但行高落到下限避免上方遮挡。
    c.handle_caret_update(&caret(200, 1));
    let s = c.state.lock().unwrap();
    assert_eq!(s.caret_y, 199);
    assert_eq!(s.caret_height, CARET_USE_TOP_MIN_LINE_H);
}

/// 走一次 notify_ui_update 的首显闸门，返回「是否 arm 了等待」。
/// 缓冲非空是必要前提，否则会先命中「空则隐藏」守卫、根本到不了闸门。
fn armed_after_first_frame(c: &Arc<Coordinator>) -> bool {
    {
        let mut s = c.state.lock().unwrap();
        s.input_buffer = "a".to_string();
    }
    let s = c.state.lock().unwrap();
    c.notify_ui_update(&s);
    drop(s);
    *c.pending_first_show.lock().unwrap()
}

/// 造一个「正等首显、且已有上一轮权威坐标」的局面，返回喂入 probe 后是否仍在等待。
fn still_waiting_after_probe(c: &Arc<Coordinator>, probe: CaretData) -> bool {
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
    }
    *c.last_authoritative_caret.lock().unwrap() = (500, 300, true);
    // 与生产代码同源：`last_authoritative_caret` 与 `caret_cache_verified` 在
    // `handle_caret_update` 里是同一判据下的两个动作。二者不合并是因为清位不同：前者跨焦点
    // 仍为 true，后者在焦点到达/移动光标时清零；probe 判据要的是后者，拿前者判会在焦点切换后
    // 把另一个单元格的坐标当基准。
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    *c.pending_first_show.lock().unwrap() = true;
    c.handle_caret_probe(&probe);
    *c.pending_first_show.lock().unwrap()
}

fn probe_at(x: i32, y: i32, height: i32) -> CaretData {
    CaretData {
        x,
        y,
        height,
        composition_start_x: x,
        composition_start_y: y,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    }
}

fn set_mode(c: &Arc<Coordinator>, mode: wind_config::app_compat::FirstShowMode) {
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1234,
        first_show_mode: Some(mode),
        ..Default::default()
    };
}

/// 只改全局默认档、不配任何 per-app 规则的协调器（`ActiveCompat::default()` 的
/// `first_show_mode` 恰是 `None`＝跟随全局，正是菜单第四档写盘后的状态）。
fn coord_with_global(mode: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.ui.candidate.first_show_mode = mode.to_string();
    Coordinator::new_headless(cfg, None)
}

/// per-app 未配时，生效档位必须跟着 `ui.candidate.first_show_mode` 走。这是全局默认档的
/// 唯一接线点，漏接表现为「设置改了没反应」且无报错。
#[test]
fn unset_per_app_rule_follows_global_config() {
    use wind_config::app_compat::FirstShowMode;
    for (cfg_val, want) in [
        ("wait", FirstShowMode::Wait),
        ("instant", FirstShowMode::Instant),
        ("fast", FirstShowMode::Fast),
    ] {
        let c = coord_with_global(cfg_val);
        assert_eq!(
            c.effective_first_show_mode(),
            want,
            "全局配置 {cfg_val} 未被读取"
        );
    }
    // 认不出的值回落枚举默认档——「写错了」与「没写」行为一致。
    assert_eq!(
        coord_with_global("turbo").effective_first_show_mode(),
        FirstShowMode::default()
    );
}

/// per-app 显式档压过全局。两者取不同值才测得出优先级——若断言里两边恰好同档，
/// 这条测试对「全局赢了 per-app」这种反转完全无感。
#[test]
fn per_app_rule_overrides_global_config() {
    use wind_config::app_compat::FirstShowMode;
    let c = coord_with_global("wait");
    assert_eq!(c.effective_first_show_mode(), FirstShowMode::Wait);
    set_mode(&c, FirstShowMode::Instant);
    assert_eq!(
        c.effective_first_show_mode(),
        FirstShowMode::Instant,
        "per-app 覆盖必须压过全局默认"
    );
}

/// fast 档的兜底必须远短于 wait 档：Word 这类宿主不发 OnLayoutChange、组合坐标 60~190ms
/// 才到，而连打时组合只活 27~57ms，150ms 兜底永远等不到到期 ⇒ fast 退化成 wait、候选窗不显示。
#[test]
fn fast_mode_uses_short_first_show_fallback() {
    use wind_config::app_compat::FirstShowMode;
    let c = coord();
    set_mode(&c, FirstShowMode::Wait);
    assert_eq!(c.first_show_fallback_ms(), 150, "wait 档保持既有 150ms");
    set_mode(&c, FirstShowMode::Instant);
    assert_eq!(
        c.first_show_fallback_ms(),
        150,
        "instant 档走逃生口不 arm，取值无所谓但不应被 fast 的短值污染"
    );
    set_mode(&c, FirstShowMode::Fast);
    let cfg = c.rt().config.ui.candidate.fast_first_show_fallback_ms;
    assert_eq!(c.first_show_fallback_ms(), cfg);
    assert!(cfg < 150, "fast 档兜底必须短于 wait 档，否则本修复失效");
}

/// DLL 的「坐标待定」握手会把 wait 档延长到 600ms。fast 档必须拒绝这次延长，
/// 否则短兜底当场作废、又变回干等。观察点取 token：arm 会 bump 它，early return 不会。
#[test]
fn caret_pending_does_not_extend_fast_mode_timeout() {
    use wind_config::app_compat::FirstShowMode;
    let c = coord();
    set_mode(&c, FirstShowMode::Fast);
    *c.pending_first_show.lock().unwrap() = true;
    let before = *c.pending_first_show_token.lock().unwrap();
    c.handle_caret_pending();
    assert_eq!(
        *c.pending_first_show_token.lock().unwrap(),
        before,
        "fast 档不得重 arm（token 未变即未重 arm）"
    );
}

/// 上一条的对照：wait 档必须照旧延长，证明那条不是被别的守卫挡住的。
#[test]
fn caret_pending_still_extends_wait_mode_timeout() {
    use wind_config::app_compat::FirstShowMode;
    let c = coord();
    set_mode(&c, FirstShowMode::Wait);
    *c.pending_first_show.lock().unwrap() = true;
    let before = *c.pending_first_show_token.lock().unwrap();
    c.handle_caret_pending();
    assert_ne!(
        *c.pending_first_show_token.lock().unwrap(),
        before,
        "wait 档应重 arm 到 600ms"
    );
}

// ── ui.status.show_on_focus：焦点气泡与坐标可信度闸门 ────────────────────────────

/// 宿主登记的配置改写在从磁盘重载之后仍然生效（移动端设置页每改一项就重载一次）。
/// 用一个盘上不可能出现的哨兵值，不受本机用户配置影响；对照组证明「不登记就没有」。
#[test]
fn config_overlay_survives_reload() {
    fn sentinel(cfg: &mut Config) {
        cfg.input.association.max_count = 4242;
    }
    let (c, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    assert!(c.set_config_overlay(sentinel));
    assert!(!c.set_config_overlay(sentinel), "只能登记一次");
    c.reload_user_config();
    assert_eq!(c.rt().config.input.association.max_count, 4242);

    let (plain, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    plain.reload_user_config();
    assert_ne!(plain.rt().config.input.association.max_count, 4242);
}

/// 造一个开了 `show_on_focus` 的协调器，并保留 UI 通道接收端——「气泡有没有真的发出去」
/// 只能从 `ui_tx` 上观察；用 debug 方法按同样规则再算一遍是假测试。
fn coord_focus_tip(
    show_on_focus: bool,
    position_mode: &str,
) -> (Arc<Coordinator>, std::sync::mpsc::Receiver<UiCommand>) {
    let mut cfg = Config::default();
    cfg.ui.status.enabled = true;
    cfg.ui.status.show_on_focus = show_on_focus;
    cfg.ui.status.display_mode = "temp".to_string();
    cfg.ui.status.position_mode = position_mode.to_string();
    Coordinator::new_headless_with_ui(cfg, None)
}

/// 通道里是否收到了「显示状态气泡」指令。
fn got_status_tip(rx: &std::sync::mpsc::Receiver<UiCommand>) -> bool {
    rx.try_iter()
        .any(|c| matches!(c, UiCommand::ShowStatusTip { .. }))
}

/// 把坐标缓存设成指定来源。
fn set_caret(c: &Arc<Coordinator>, x: i32, y: i32, source: i32) {
    let mut st = c.state.lock().unwrap();
    st.caret_x = x;
    st.caret_y = y;
    st.caret_height = 25;
    st.caret_source = source;
}

/// 宿主自己摆窗口（Wayland input popup，`caret_independent`）时，坐标来源不是 TSF 也直接弹：
/// 坐标根本不用，等权威坐标只会永远等不到（Wayland 上「焦点变化时显示」曾因此从不出现）。
#[test]
fn focus_tip_shows_without_tsf_caret_when_host_places_window() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(&c, 0, 0, wind_ipc::protocol::caret_source::UNKNOWN);
    c.set_caret_independent(true);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "caret_independent 宿主不该挂起等坐标");
}

/// 宿主声明光标可信（Linux addon）时，来源 UNKNOWN 也直接弹（Linux 上「焦点变化时显示」
/// 曾因此从不出现）。对照：没声明的普通宿主仍挂起，见 `focus_tip_defers_when_caret_source_is_not_tsf`。
#[test]
fn focus_tip_shows_for_trusted_host_caret() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(&c, 100, 200, wind_ipc::protocol::caret_source::UNKNOWN);
    c.set_host_caret_trusted(true);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "宿主声明光标可信时不该挂起");
}

/// 两个不同宿主的 client_token。用具名常量而非字面量，是因为下面「同宿主不重复弹」
/// 那组用例的全部含义就在于这两个值相不相等，字面量会让它退化成看不出意图的魔数。
const TOKEN_A: u64 = 0x1111_0000_0001;
const TOKEN_B: u64 = 0x2222_0000_0001;

/// 同一宿主内换 docMgr（Excel 单元格 ↔ 公式编辑栏）不得重复弹气泡。
/// 这是「输入一次闪两下」的直接成因——闪的时机与用户的操作节奏对不上。
#[test]
fn focus_tip_skips_same_host_docmgr_switch() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(
        &c,
        100,
        200,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "首次进入该宿主应弹一次");

    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(
        !got_status_tip(&rx),
        "同一 token = 同一宿主内换 docMgr，不得重复弹"
    );
}

/// 反向对照：换了宿主必须照弹。
/// 缺了这条，上一条用「弹过一次就再也不弹」的实现也能通过——那会让切换应用时
/// 气泡彻底消失，比重复弹更糟。
#[test]
fn focus_tip_shows_again_for_different_host() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(
        &c,
        100,
        200,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx));

    c.show_focus_status_if_enabled(TOKEN_B);
    assert!(got_status_tip(&rx), "换宿主必须重新提示一次");
}

/// 离开宿主（Thread 级失焦）后再回来，应当重新提示。
/// 只有 Thread 档清去重记录：CtxLost/DocChanged 是宿主内换 docMgr 的噪声，
/// 若也清就等于按 docMgr 计数，Excel 下会退回「输入一次闪两下」。
#[test]
fn focus_tip_resets_after_leaving_host() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(
        &c,
        100,
        200,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx));

    // docMgr 级失焦：不清记录，回来仍不弹
    c.handle_focus_lost(TOKEN_A, wind_bridge::handler::FocusLostReason::CtxLost);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(!got_status_tip(&rx), "CtxLost 属 docMgr 噪声，不该解除去重");

    // 真正离开宿主：清记录，回来重新弹
    c.handle_focus_lost(TOKEN_A, wind_bridge::handler::FocusLostReason::Thread);
    set_caret(
        &c,
        100,
        200,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "离开宿主后再进入应重新提示");
}

/// follow_caret 下，坐标来自 GUI 回退时不得直接弹气泡——那正是用户反馈的
/// 「还没输入时定位非常不准」：`OnSetFocus` 拿不到同步锁，回退链交出的是跨窗口的
/// Win32 光标（Word 标题行实测偏差 814px）。应转为挂起等权威坐标。
#[test]
fn focus_tip_defers_when_caret_source_is_not_tsf() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(
        !got_status_tip(&rx),
        "GUI 回退坐标不可作气泡锚点，此时不得下发显示"
    );
    assert!(
        c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed),
        "应转为挂起，等 DLL 补来的权威坐标"
    );
}

/// 上一条的续集：权威坐标到达后必须补显示，且挂起位清掉。
///
/// 消费点必须在 `handle_caret_update` 的 `composing` 闸门之前——焦点刚到达时 `composing`
/// 恒 false，放在闸门之后就永远不执行。`input_buffer` 特意留空来钉这个顺序。
#[test]
fn focus_tip_shows_when_authoritative_caret_arrives() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(!got_status_tip(&rx));

    c.handle_caret_update(&CaretData {
        x: 473,
        y: 217,
        height: 28,
        composition_start_x: 0,
        composition_start_y: 0,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    assert!(got_status_tip(&rx), "等到 TSF 权威坐标后应补显示气泡");
    assert!(
        !c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed),
        "补显示后挂起位必须清掉，否则下一帧坐标会再弹一次"
    );
}

/// 反向对照：非 TSF 域的坐标即便到达也不得解除挂起。
/// 少了这条，上一条用「任何 caret_update 都补显示」的实现也能通过。
#[test]
fn focus_tip_stays_pending_for_non_tsf_caret_update() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN_A);
    let _ = got_status_tip(&rx); // 排空

    c.handle_caret_update(&CaretData {
        x: 10,
        y: 20,
        height: 20,
        composition_start_x: 0,
        composition_start_y: 0,
        source: wind_ipc::protocol::caret_source::GUI_CARET,
        composition_rect: None,
    });
    assert!(!got_status_tip(&rx), "又一个 GUI 回退坐标，仍不该显示");
    assert!(
        c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed),
        "挂起必须保持，直到真的等到 TSF 坐标"
    );
}

/// 坐标本就可信时立即显示，不该被闸门误伤。
#[test]
fn focus_tip_shows_immediately_for_tsf_caret() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(
        &c,
        473,
        217,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "TSF 域坐标应立即显示");
    assert!(
        !c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed),
        "无需挂起"
    );
}

/// fixed 模式压根不读 caret（用 custom_x/custom_y），故不受可信度闸门约束。
/// 把闸门一刀切地套到所有模式上，会让固定位置的用户永远看不到焦点气泡。
#[test]
fn focus_tip_ignores_caret_source_in_fixed_mode() {
    let (c, rx) = coord_focus_tip(true, "fixed");
    set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "fixed 模式不读 caret，应照常显示");
}

/// 反向对照：开关关闭时一律不显示。
/// 少了这条，「无条件显示」的实现能让上面四条里的三条通过。
#[test]
fn focus_tip_silent_when_disabled() {
    let (c, rx) = coord_focus_tip(false, "follow_caret");
    set_caret(
        &c,
        473,
        217,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(!got_status_tip(&rx), "show_on_focus=false 时不得显示");
    assert!(
        !c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed),
        "开关关闭时连挂起都不该发生"
    );
}

/// 焦点气泡必须绕过 `show_status` 的文本去重：焦点切换正是「状态文本没变但仍要提示」的场景。
///
/// 与 [`focus_tip_skips_same_host_docmgr_switch`] 的宿主去重是两回事：这里换的是宿主
/// （TOKEN_A → TOKEN_B），本就该弹；那里是同一宿主内换 docMgr，不该弹。
#[test]
fn focus_tip_bypasses_text_dedup() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(
        &c,
        473,
        217,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(got_status_tip(&rx), "第一次焦点切换应显示");
    // 状态一字未改，模拟切到另一个宿主的输入框
    c.show_focus_status_if_enabled(TOKEN_B);
    assert!(
        got_status_tip(&rx),
        "文本相同也必须再显示一次——文本去重会让这个开关形同虚设"
    );
}

/// 失焦要作废挂起中的焦点气泡，否则权威坐标晚到时会在已经切走之后才弹出来。
#[test]
fn hide_tip_cancels_pending_focus_tip() {
    let (c, rx) = coord_focus_tip(true, "follow_caret");
    set_caret(&c, 0, 1388, wind_ipc::protocol::caret_source::GUI_CARET);
    c.show_focus_status_if_enabled(TOKEN_A);
    assert!(
        c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    c.hide_tip();
    assert!(
        !c.pending_focus_tip
            .load(std::sync::atomic::Ordering::Relaxed),
        "失焦后挂起必须作废"
    );
    let _ = got_status_tip(&rx); // 排空
    c.handle_caret_update(&CaretData {
        x: 473,
        y: 217,
        height: 28,
        composition_start_x: 0,
        composition_start_y: 0,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    assert!(
        !got_status_tip(&rx),
        "失焦之后到达的权威坐标不得再触发补显示"
    );
}

/// 焦点 caret 的 `height == 0`（宿主尚未 reflow 的退化矩形）不得进缓存。
///
/// 重型段 `handle_focus_gained` 必然晚于同步段执行，它若自己直写 `state.caret_*`，
/// 同步段的守卫就被整个抹掉，表现为定位偏一行。
#[test]
fn focus_caret_degenerate_rect_does_not_overwrite_cache() {
    let c = coord();
    set_caret(
        &c,
        473,
        217,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.apply_focus_caret(
        &CaretData {
            x: 9999,
            y: 9999,
            height: 0, // 退化矩形
            composition_start_x: 0,
            composition_start_y: 0,
            source: wind_ipc::protocol::caret_source::GUI_CARET,
            composition_rect: None,
        },
        "test",
    );
    let st = c.state.lock().unwrap();
    assert_eq!(st.caret_x, 473, "退化帧不得覆盖已有的好坐标");
    assert_eq!(st.caret_y, 217);
    assert_eq!(
        st.caret_source,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
        "来源必须与坐标同进退——只回滚其一等于伪造了一个不存在的组合"
    );
}

/// 上一条只证明守卫存在于 `apply_focus_caret`；这条走 `handle_focus_gained` 生产入口，
/// 它一旦退回直写 `state.caret_*` 即红。顺带钉住 `caret_use_top` 也在重型段生效。
#[test]
fn handle_focus_gained_routes_caret_through_shared_guard() {
    let c = coord();
    set_caret(
        &c,
        473,
        217,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    );
    c.handle_focus_gained(&FocusData {
        x: 9999,
        y: 9999,
        height: 0, // 退化矩形：同步段会挡，重型段直写则不会
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: 0,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: wind_ipc::protocol::caret_source::GUI_CARET,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    let st = c.state.lock().unwrap();
    assert_eq!(
        st.caret_x, 473,
        "重型段必须经 apply_focus_caret；直写会让退化帧覆盖好坐标"
    );
    assert_eq!(st.caret_y, 217);
    assert_eq!(
        st.caret_source,
        wind_ipc::protocol::caret_source::TSF_SELECTION
    );
}

/// 焦点事件必须作废组合起点锚定。
///
/// 锚定「同一组合只锁一次」的前提是起点不会移动，而 focus_gained 意味着换了 docMgr
/// （如 Excel 在单元格与编辑栏之间切换，组合整体迁移）。不作废的话候选窗钉死在旧 docMgr 上，
/// 日志表现为「reshow 说要重定位、UI 位置纹丝不动」。
#[test]
fn focus_gained_invalidates_composition_start_anchor() {
    let c = coord();
    *c.composition_start.lock().unwrap() = (593, 572, true);
    c.handle_focus_gained(&FocusData {
        x: 1457,
        y: 959,
        height: 37,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: TOKEN_A,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    assert!(
        !c.composition_start.lock().unwrap().2,
        "换 docMgr 后组合起点必须作废，交由下一帧 caret_update 就地重锁"
    );
}

/// `caret_use_top` 变换在重型段同样要生效。
/// 该变换原先只在同步段做，重型段的直写把它抹掉，表现为微信一类宿主定位差一个行高。
#[test]
fn handle_focus_gained_applies_caret_use_top() {
    let c = coord();
    {
        let mut ac = c.active_compat.lock().unwrap();
        ac.caret_use_top = true;
    }
    c.handle_focus_gained(&FocusData {
        x: 100,
        y: 300,
        height: 30,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: 0,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    let st = c.state.lock().unwrap();
    assert_eq!(
        st.caret_y,
        300 - 30,
        "caret_use_top 应把 Y 上移一个行高；重型段直写则原样落缓存"
    );
}

/// `handle_focus_gained` 内 `update_active_compat` 必须先于它自己那次 `apply_focus_caret`，
/// 否则本次焦点事件的第一份坐标会拿上一个进程的规则（如 `caret_offset_y`）去变换。
/// 用 `pid_names` 预置该 pid 的名字（同
/// `update_active_compat_prefers_cached_name_over_process_lookup`），无需真实 `OpenProcess`。
#[test]
fn handle_focus_gained_applies_new_process_caret_offset_on_first_caret() {
    let c = coord();
    let pid = 8848u32;
    let token = (pid as u64) << 32 | 1;
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "windowsterminal.exe".to_string());
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "windowsterminal.exe".into(),
        caret_offset_y: 12,
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

    c.handle_focus_gained(&FocusData {
        x: 100,
        y: 300,
        height: 30,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: token,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    let st = c.state.lock().unwrap();
    assert_eq!(
        st.caret_y, 312,
        "新进程的 caret_offset_y 必须在本次焦点事件的第一份坐标上就生效，\
             不能等到下一次 caret_update 才校正"
    );
}

/// 造一个 fast 档协调器并指定坐标缓存可信与否。
fn fast_coord(verified: bool) -> Arc<Coordinator> {
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 25;
    }
    c.caret_cache_verified
        .store(verified, std::sync::atomic::Ordering::Relaxed);
    c
}

/// 首帧信任门：坐标缓存未经当前插入点验证时不得走短兜底——拿旧坐标首显正是
/// Excel「进单元格第一个字漂移」的成因（手里那份属于上一个单元格）。
#[test]
fn untrusted_caret_arms_long_fallback() {
    let c = fast_coord(false);
    c.arm_pending_first_show();
    assert!(
        c.first_show_extended
            .load(std::sync::atomic::Ordering::Relaxed),
        "坐标不可信时应进入长兜底等待"
    );
    assert!(*c.pending_first_show.lock().unwrap());
}

/// 反向对照：坐标可信时必须照常走短兜底，否则信任门就成了无差别拖慢，
/// fast 档整个失去意义。
#[test]
fn trusted_caret_keeps_short_fallback() {
    let c = fast_coord(true);
    c.arm_pending_first_show();
    assert!(
        !c.first_show_extended
            .load(std::sync::atomic::Ordering::Relaxed),
        "坐标可信时不应进入长兜底"
    );
    assert_eq!(
        c.first_show_fallback_ms(),
        c.rt().config.ui.candidate.fast_first_show_fallback_ms
    );
}

/// 长等待不得被后续按键重置。闸门对每个字母都会调 arm，若照常 bump token 重新计时，
/// 宿主建上下文期间多打几个字母就让长兜底静默退化回短兜底。
/// 这是「兜底超时长于组合寿命 ⇒ 永不到期」死结的镜像。
#[test]
fn long_fallback_survives_subsequent_keystrokes() {
    let c = fast_coord(false);
    c.arm_pending_first_show();
    let token = *c.pending_first_show_token.lock().unwrap();
    // 用户继续输入：闸门对第 2、3 个字母同样调 arm
    c.arm_pending_first_show();
    c.arm_pending_first_show();
    assert_eq!(
        *c.pending_first_show_token.lock().unwrap(),
        token,
        "后续按键不得重置长兜底计时，否则等待被无限推后"
    );
}

/// 反向对照：坐标可信的正常连打必须照旧每次重新计时（既有行为，不能被上一条误伤）。
#[test]
fn short_fallback_still_rearms_per_keystroke() {
    let c = fast_coord(true);
    c.arm_pending_first_show();
    let token = *c.pending_first_show_token.lock().unwrap();
    c.arm_pending_first_show();
    assert_ne!(
        *c.pending_first_show_token.lock().unwrap(),
        token,
        "短兜底路径的既有行为是每次按键重新计时"
    );
}

/// 单向陷阱：`caret_cache_verified` 的清位有两个日常入口（切窗口、点击移光标），置位却只有
/// 「组合期间收到权威 caret_update」。fast 档下组合常等不到权威坐标，一旦被清就再也回不来，
/// 此后每个组合都 arm 600ms 长兜底。
///
/// 组合前宿主主动上报的空闲光标是同样够格的第二个置位来源：它直接测量当前插入点，
/// 恰好就是本次组合的起点。
#[test]
fn idle_caret_report_clears_the_trust_gate() {
    let c = fast_coord(false);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer.clear();
        st.candidates.clear();
    }
    c.handle_caret_update(&caret(400, 20));
    assert!(
        c.caret_cache_verified
            .load(std::sync::atomic::Ordering::Relaxed),
        "组合前的 TSF 空闲上报必须解除信任门，否则快速输入下 verified 永远回不来"
    );
}

/// 反向：GUI 回退通道不够格解除信任门。实测拿到过任务栏残留的 Win32 光标 (0,1388)——
/// 它够当「没有更好选择时的兜底位置」，不够让 fast 档判定可以跳过等待。
#[test]
fn non_tsf_idle_report_keeps_the_trust_gate() {
    let c = fast_coord(false);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer.clear();
        st.candidates.clear();
    }
    c.handle_caret_update(&CaretData {
        source: wind_ipc::protocol::caret_source::GUI_CARET,
        composition_rect: None,
        ..caret(400, 20)
    });
    assert!(
        !c.caret_cache_verified
            .load(std::sync::atomic::Ordering::Relaxed),
        "GUI 回退通道的坐标不够格让 fast 档跳过等待"
    );
}

/// 驱动一次首帧候选下发，走真实的 `notify_ui_update` 闸门——判据接在闸门上，
/// 只断言 `caret_cache_is_fresh_idle_report()` 的返回值等于没测接线。
fn drive_first_frame(c: &Arc<Coordinator>) {
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
    }
    let st = c.state.lock().unwrap();
    c.notify_ui_update(&st);
}

/// idle_anchor 逃生口：缓存是组合前的 TSF 空闲上报时不必等——兜底到期后用的就是它，
/// 25ms 内也等不到更好的。补的是 `coords_ready` 在连打时的死角（组合起点每次上屏被
/// `reset_first_show` 清掉，重锁又要等权威坐标）。
#[test]
fn fresh_idle_report_skips_the_wait() {
    let c = fast_coord(true);
    // 本逃生口只给 probe 不可信的宿主（见 probe_is_untrusted）：probe 可信时等 25ms
    // 让它说话更准，空闲上报可能滞后一拍。
    c.active_compat.lock().unwrap().stale_probe_guard = true;
    c.caret_cache_is_idle_report
        .store(true, std::sync::atomic::Ordering::Relaxed);
    drive_first_frame(&c);
    assert!(
        !*c.pending_first_show.lock().unwrap(),
        "组合前空闲上报已够格当锚点，首帧不该再等"
    );
    assert!(
        c.first_show_was_provisional
            .load(std::sync::atomic::Ordering::Relaxed),
        "必须标 provisional：首显用的是按键前的位置，组合一起首字母就落进编辑区，\
             真实光标随即前移一个字符宽（记事本实测 15px）。不标则 tol 只有 3px，那一次\
             权威坐标必然 reshow——首显是准的，却要当着用户的面跳一格"
    );
}

/// 反向对照：缓存虽可信、但出身不是组合前空闲上报（如组合期间收到的权威坐标）时，
/// 逃生口不成立，仍走既有等待路径。守住「两个标志缺一不可」。
#[test]
fn verified_cache_alone_does_not_open_the_escape_hatch() {
    let c = fast_coord(true);
    drive_first_frame(&c);
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "只有 verified、出身不是空闲上报时，必须维持既有的等待行为"
    );
}

/// 逃生口不得把信任门本来要堵的洞重新顶开。`focus_gained` 清 `caret_cache_verified` 是为了
/// 「焦点后第一个字必须等权威坐标」；空闲上报在组合之间可信，在焦点刚到达时最不可信
/// （宿主还没 reflow，报的是上一处的位置）。
#[test]
fn idle_report_right_after_focus_does_not_skip_the_wait() {
    let c = fast_coord(true);
    c.caret_cache_is_idle_report
        .store(true, std::sync::atomic::Ordering::Relaxed);
    c.awaiting_first_authority_after_focus
        .store(true, std::sync::atomic::Ordering::Relaxed);
    drive_first_frame(&c);
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "焦点后的第一个组合必须照旧等待——那时的空闲上报最不可信"
    );
}

/// 反向：本次焦点下拿到过一帧组合期权威坐标后，宿主已 reflow、坐标体系稳定，
/// 逃生口解禁。否则这道闸门就成了「焦点后永远不提速」，把收益全吃掉。
#[test]
fn first_authority_after_focus_unlocks_the_escape_hatch() {
    let c = fast_coord(false);
    c.awaiting_first_authority_after_focus
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // fast_coord 已把 input_buffer 设成 "a"，故这一帧走组合期的权威坐标分支
    c.handle_caret_update(&caret(300, 25));
    assert!(
        !c.awaiting_first_authority_after_focus
            .load(std::sync::atomic::Ordering::Relaxed),
        "组合期权威坐标到达后必须解禁，否则焦点后再也提不了速"
    );
}

/// 排空并取全部 `UpdateCandidates` 下发位置。
fn drain_positions(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Vec<(i32, i32)> {
    let mut found = Vec::new();
    while let Ok(cmd) = rx.try_recv() {
        if let UiCommand::UpdateCandidates {
            caret_x, caret_y, ..
        } = cmd
        {
            found.push((caret_x, caret_y));
        }
    }
    found
}

/// 取最近一条 `UpdateCandidates` 下发的位置。
fn last_pos(rx: &std::sync::mpsc::Receiver<UiCommand>) -> Option<(i32, i32)> {
    // 排空取最后一条：一次刷新会发多条 UI 命令
    drain_positions(rx).last().copied()
}

/// 非坐标原因的重绘不得移动候选窗。`state.caret_x/y` 会被 probe 与被 settle 吸收的那帧
/// 悄悄改写，悬停 / 翻页 / 候选变化触发的重绘若读缓存，就会把这段差额一次性补上。
#[test]
fn non_positional_redraw_keeps_the_candidate_where_it_is() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    // instant 档：让首帧直接下发，本用例专注测「首显之后的重绘」
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (483, 1017, 20);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 483;
        st.caret_y = 1017;
        st.caret_height = 20;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    let first = last_pos(&rx).expect("首显应当下发一条 UpdateCandidates");

    // 缓存被悄悄改写（真机里由 probe 或被 settle 吸收的权威坐标造成），候选窗并未跟着动
    c.state.lock().unwrap().caret_x = 496;
    c.mouse_hover(0);
    let after = last_pos(&rx).expect("悬停变化应当触发一次重绘");
    assert_eq!(
        first, after,
        "悬停只改高亮项，不是坐标事件，候选窗必须留在原处"
    );
}

/// 组合起点常在首显之后才由宿主报来，而 `notify_ui_update` 里 `cs.2` 那条分支一旦成立就接管
/// 位置。锚点若排在它后面，悬停会把候选窗挪到组合起点，等于绕过 settle 刚做的判定。
/// 上一条用例的 `cs.2` 全程为 false，覆盖不到这里。
#[test]
fn late_composition_start_does_not_move_the_shown_candidate() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (483, 987, 20);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 483;
        st.caret_y = 987;
        st.caret_height = 20;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    let first = last_pos(&rx).expect("首显应当下发一条 UpdateCandidates");
    assert_eq!(first, (483, 987), "首显位置应取当时的坐标");

    // 宿主迟到的组合起点（真机里由 handle_caret_update 的锚定逻辑写入）
    *c.composition_start.lock().unwrap() = (496, 989, true);
    c.mouse_hover(0);
    let after = last_pos(&rx).expect("悬停变化应当触发一次重绘");
    assert_eq!(
        first, after,
        "组合起点迟到不构成移动候选窗的理由——那 13px 已被 settle 判定不值得校正"
    );
}

/// 信任门否决 probe 做显示决策，但不否决它当坐标来源。此前这里直接 return，
/// 兜底到期时就只剩焦点切换前的旧缓存可用：Excel 实测 probe 连报三次正确的 (475,579)
/// 全被丢弃，兜底拿 (1918,831) 首显，错 1443px，53ms 后才由 reshow 跳回来。
#[test]
fn rejected_probe_still_refreshes_the_cache_for_fallback() {
    let c = fast_coord(false); // 缓存未验证 ⇒ 信任门命中
    *c.pending_first_show.lock().unwrap() = true;
    c.handle_caret_probe(&probe_at(475, 579, 22));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "信任门命中时不得让 probe 提前首显"
    );
    let st = c.state.lock().unwrap();
    assert_eq!(
        (st.caret_x, st.caret_y),
        (475, 579),
        "被否决的 probe 坐标仍须收进缓存——兜底到期时它是唯一比旧值更新的来源"
    );
}

/// 坐标校正的判据必须比「候选窗实际画在哪」，不能比坐标缓存：缓存会被 probe 抢先刷成新值，
/// 此后比较恒得「没变化」，错位再也无法自愈（如 Excel 先在编辑栏建上下文、再切到单元格）。
#[test]
fn stale_shown_position_is_corrected_even_when_cache_already_moved() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (326, 314, 31);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 326;
        st.caret_y = 314;
        st.caret_height = 31;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(
        last_pos(&rx),
        Some((326, 314)),
        "首显应落在编辑栏那份坐标上"
    );

    // probe 抢先把缓存刷成单元格位置（真机里由 absorb_probe_coords 写入），候选窗未动
    {
        let mut st = c.state.lock().unwrap();
        st.caret_x = 1307;
        st.caret_y = 803;
    }
    // 随后同值的权威坐标到达：与缓存比 dx=0，与候选窗实际位置比 dx=981
    c.handle_caret_update(&probe_at(1307, 803, 28));
    assert_eq!(
        last_pos(&rx),
        Some((1307, 803)),
        "候选窗必须跟到新 docMgr——判据比的是它自己画在哪，不是缓存"
    );
}

/// `settle` 说的是「这次偏差不值得校正」，不是「以后也不值得」。被吸收的那一帧坐标必须写进
/// 基准，否则下一帧又是同样的偏差，而放宽容差已被 `swap` 消费掉，候选窗自己挪一格。
///
/// 同时钉住 `caret_baseline` 与 `shown_anchor` 不可合并：合并后必失败。
#[test]
fn absorbed_drift_does_not_accumulate_into_a_later_jump() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (483, 987, 20);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 483;
        st.caret_y = 987;
        st.caret_height = 20;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((483, 987)), "首显位置");

    // 第一帧权威坐标偏 13px：settle 容差 0.8×20=16px，应被吸收
    c.handle_caret_update(&probe_at(496, 988, 20));
    assert_eq!(last_pos(&rx), None, "13px 在 settle 容差内，不得移动候选窗");

    // 第二帧同值再来：与已更新的基准比 dx=0，仍不该动
    c.handle_caret_update(&probe_at(496, 988, 20));
    assert_eq!(
        last_pos(&rx),
        None,
        "被吸收的偏差不得累积——同一位置重复上报不该把候选窗挪走"
    );
}

/// 空闲上报坐标就是组合起点（按键前的光标位置），idle_anchor 首显时必须抢先锁上。
/// 微信报的 compStart 恒等于当前光标 x，随每个字母右移；不抢先锁，reshow 会让候选窗自己挪。
#[test]
fn idle_anchor_claims_the_composition_start() {
    let c = fast_coord(true);
    c.active_compat.lock().unwrap().stale_probe_guard = true;
    c.caret_cache_is_idle_report
        .store(true, std::sync::atomic::Ordering::Relaxed);
    drive_first_frame(&c);
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        cs,
        (100, 200, true),
        "idle_anchor 首显后组合起点应锁在按键前的光标位置，不留给宿主那份偏右的值"
    );
}

/// 首帧校正的容差在水平与垂直上不对称。
///
/// 水平吸收「上屏文本宽度 ≠ 编码文本宽度」（量级一个字符宽）；垂直必须分辨换行（偏差恰好
/// 一个行高），所以 ratio 必须 < 1.0。共用一个容差会让垂直的约束把水平也卡死。
#[test]
fn settle_tolerance_absorbs_one_char_width_but_still_follows_a_line_break() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (1684, 747, 20);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 1684;
        st.caret_y = 747;
        st.caret_height = 20;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((1684, 747)), "首显位置");

    // 上屏后的真实插入点比缓存靠左 20px（汉字比 4 个字母窄），纯水平
    c.handle_caret_update(&probe_at(1664, 747, 20));
    assert_eq!(
        last_pos(&rx),
        None,
        "一个字符宽的水平偏差应被吸收——否则每次自动上屏都要当着用户的面校正一次"
    );

    // 反向对照：同样量级的垂直偏差是换行，必须跟过去
    let (c2, rx2) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c2, wind_config::app_compat::FirstShowMode::Instant);
    *c2.last_valid_caret.lock().unwrap() = (1684, 747, 20);
    {
        let mut st = c2.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 1684;
        st.caret_y = 747;
        st.caret_height = 20;
    }
    {
        let st = c2.state.lock().unwrap();
        c2.notify_ui_update(&st);
    }
    let _ = last_pos(&rx2);
    c2.handle_caret_update(&probe_at(1684, 767, 20));
    assert_eq!(
        last_pos(&rx2),
        Some((1684, 767)),
        "换行的偏差恰好是一个行高，垂直容差必须留在 1.0 行高以内、让它跟过去"
    );
}

/// 行高退化的宿主上，settle 容差不得跟着塌缩。
///
/// 微信等 WebView 的 height 在 1↔20px 之间跳变；只取当前帧时 `(1 × 0.8) as i32 == 0`，
/// 容差落到下限 3px，放宽多少倍都没用，一个字符宽的偏差必然触发校正。
/// 行高是宿主的属性，不是单帧的属性。
#[test]
fn settle_tolerance_survives_a_degenerate_caret_height() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (901, 988, 20);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 901;
        st.caret_y = 988;
        st.caret_height = 20;
    }
    // 宿主先报过一帧正常行高（真机上必然有：空闲上报那一帧 h=20）
    c.last_sane_caret_height
        .store(20, std::sync::atomic::Ordering::Relaxed);
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((901, 988)), "首显位置");

    // 随后宿主开始报退化高度 h=1，且插入点回退了一个字符宽
    {
        let mut st = c.state.lock().unwrap();
        st.caret_height = 1;
    }
    c.handle_caret_update(&probe_at(881, 988, 1));
    assert_eq!(
        last_pos(&rx),
        None,
        "行高退化不得让容差塌缩到 3px——一个字符宽的偏差仍应被吸收"
    );
}

/// 被 settle 吸收的偏差不得从组合起点那条路复活。
///
/// 组合起点若在同一帧锁成了新坐标，后续任何一次 reshow 都会把候选窗拉过去（reshow 位置取
/// `cs`），那一跳只是推迟到了下一个字母。
#[test]
fn absorbed_drift_does_not_come_back_through_the_composition_start() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (537, 988, 20);
    c.last_sane_caret_height
        .store(20, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 537;
        st.caret_y = 988;
        st.caret_height = 20;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((537, 988)), "首显于上屏前的缓存位置");

    // 真实插入点比缓存靠左 20px，且宿主在这一帧报出组合起点——settle 应吸收，候选窗不动
    c.handle_caret_update(&probe_at(517, 988, 20));
    assert_eq!(last_pos(&rx), None, "20px 应被 settle 吸收");

    // 下一个字母：插入点再前移，触发 reshow。位置必须留在候选窗当前处，
    // 不得跳回被吸收掉的那 20px。
    c.handle_caret_update(&probe_at(531, 988, 20));
    let after = last_pos(&rx).expect("超出容差应当校正一次");
    assert_eq!(
        after.0, 537,
        "被 settle 吸收的偏差不得从组合起点复活——否则那一跳只是推迟到下一个字母"
    );
}

/// 首显位置整个错掉时，组合起点必须能重锁，否则候选窗再也回不来。
///
/// 「组合起点本组合内不再更新」是建立在不保证正确的输入上的不可撤销决定：陈旧的空闲上报、
/// 上一个单元格的坐标、滞后一拍的上报都会把它锁在错处。
/// 判据是量级不是方向：字宽差异恒在一个行高上下，真实错位差一个数量级。
#[test]
fn a_grossly_wrong_first_show_can_relock_the_composition_start() {
    // QQ 修复是显式 per-app 帧对保护，未开启时大偏移逃生阀必须保持原行为：无论宿主
    // 报的是新 compStart、没有 compStart，还是继续回显陈旧 compStart，都以当前 caret
    // 自愈。否则会让微信/WPS/EverEdit/Excel 的历史错锚点重新变成不可撤销。
    for (case, reported_cs_x) in [
        ("新 compStart", 493),
        ("缺失 compStart", 0),
        ("陈旧 compStart", 1030),
    ] {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
        *c.last_valid_caret.lock().unwrap() = (1030, 987, 20);
        c.last_sane_caret_height
            .store(20, std::sync::atomic::Ordering::Relaxed);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".into();
            st.candidates = vec![wind_candidate::Candidate {
                text: "测".into(),
                ..Default::default()
            }];
            st.caret_x = 1030;
            st.caret_y = 987;
            st.caret_height = 20;
        }
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        assert_eq!(last_pos(&rx), Some((1030, 987)), "首显于陈旧坐标");
        // 首显那一刻把它锁成了组合起点（idle_anchor 路径的行为）
        *c.composition_start.lock().unwrap() = (1030, 987, true);

        // 真实插入点在 537px 之外——远超字宽量级，说明首显位置整个是错的
        let mut update = probe_at(493, 988, 20);
        update.composition_start_x = reported_cs_x;
        if reported_cs_x == 0 {
            update.composition_start_y = 0;
        }
        c.handle_caret_update(&update);
        assert_eq!(
            last_pos(&rx),
            Some((493, 988)),
            "偏移远超字宽量级时组合起点必须重锁，否则候选窗永远停在错处；\
                 case={case}"
        );
    }
}

/// QQNT 的嵌入组合在同一按键后依次上报起点降级帧（`TSF_COMPOSITION`）与 selection 帧
/// （`TSF_SELECTION`），而 reported compStart 始终没动。
///
/// 长拼音的组合跨度超过 3 个行高后，若逃生阀拿 caret 偏移判「起点锁错」，锚点会反复重锁、
/// 候选窗左右闪烁。组合跨度不是起点错误；reported compStart 没动就必须继续钉住。
#[test]
fn long_qq_composition_does_not_oscillate_between_start_and_selection() {
    const START_X: i32 = 598;
    const Y: i32 = 585;
    const LINE_H: i32 = 18;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    c.active_compat.lock().unwrap().composition_start_pair_guard = true;
    *c.last_valid_caret.lock().unwrap() = (START_X, Y, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "daduo".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = START_X;
        st.caret_y = Y;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((START_X, Y)), "首显于组合起点");
    *c.composition_start.lock().unwrap() = (START_X, Y, true);
    // 真机跨过阈值前的上一帧 selection caret 是 644；第一对帧随后是 start=598、selection=653。
    *c.caret_baseline.lock().unwrap() = (644, Y, true);

    for selection_x in [653, 662, 670, 682] {
        c.handle_caret_update(&CaretData {
            x: START_X,
            y: Y,
            height: LINE_H,
            composition_start_x: START_X,
            composition_start_y: Y,
            source: wind_ipc::protocol::caret_source::TSF_COMPOSITION,
            composition_rect: None,
        });
        assert!(
            drain_positions(&rx)
                .into_iter()
                .all(|pos| pos == (START_X, Y)),
            "组合起点降级帧即使触发重绘，下发位置也只能是原起点"
        );

        c.handle_caret_update(&CaretData {
            x: selection_x,
            y: Y,
            height: LINE_H,
            composition_start_x: START_X,
            composition_start_y: Y,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: None,
        });
        assert!(
            drain_positions(&rx)
                .into_iter()
                .all(|pos| pos == (START_X, Y)),
            "selection caret 只是组合末端；允许抑制重复重绘，但不得下发其它位置"
        );
        assert_eq!(
            *c.composition_start.lock().unwrap(),
            (START_X, Y, true),
            "reported compStart 未变化时，同一组合的锚点必须保持不变"
        );
    }
}

/// `composition_start` 与「宿主报的组合起点」是两个量，判据只能问后者。
///
/// QQ 组合开始前后度量会变（基线上移约 2px），首显走空闲缓存锁下的 `composition_start`
/// 与宿主此后恒定上报的 compStart 差几个像素。判据若写成「`cs` 等于 reported compStart」，
/// 这点差就让保护失效，长拼音末尾时锚点被重锁到组合末端。
/// 正确的判据是问宿主自己前后是否改口，与我方锁了什么无关。
#[test]
fn qq_reported_start_wins_even_when_locked_anchor_differs_by_metrics() {
    // 真机数值，不要改成整数——这个 case 的全部内容就是那 2px 的分歧。
    const REPORTED_X: i32 = 2268;
    const REPORTED_Y: i32 = 1070;
    const ANCHOR_X: i32 = 2266; // 首显自空闲缓存锁下的起点，与宿主报的差 (2,2)
    const ANCHOR_Y: i32 = 1072;
    const LINE_H: i32 = 34;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    c.active_compat.lock().unwrap().composition_start_pair_guard = true;
    *c.last_valid_caret.lock().unwrap() = (ANCHOR_X, ANCHOR_Y, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "yibanqing".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "一半".into(),
            ..Default::default()
        }];
        st.caret_x = ANCHOR_X;
        st.caret_y = ANCHOR_Y;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(
        last_pos(&rx),
        Some((ANCHOR_X, ANCHOR_Y)),
        "首显于空闲缓存坐标（与宿主随后报出的 compStart 差 2px）"
    );
    *c.composition_start.lock().unwrap() = (ANCHOR_X, ANCHOR_Y, true);

    let frame = |x: i32, src: i32| CaretData {
        x,
        y: REPORTED_Y,
        height: LINE_H,
        composition_start_x: REPORTED_X,
        composition_start_y: REPORTED_Y,
        source: src,
        composition_rect: None,
    };

    // 09:51:14.463 —— 组合中段的 selection 帧，喂进「宿主上一帧报的起点」。
    *c.caret_baseline.lock().unwrap() = (2346, REPORTED_Y, true);
    c.handle_caret_update(&frame(
        2346,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    ));
    drain_positions(&rx);

    // 09:51:14.736 降级帧 + 09:51:14.739 selection 帧：后者相对前者偏移 110px，
    // 越过 3 倍行高（102）。宿主报的起点两帧都没动，不得重锁。
    c.handle_caret_update(&frame(
        REPORTED_X,
        wind_ipc::protocol::caret_source::TSF_COMPOSITION,
    ));
    c.handle_caret_update(&frame(
        2378,
        wind_ipc::protocol::caret_source::TSF_SELECTION,
    ));

    let positions = drain_positions(&rx);
    let cs = *c.composition_start.lock().unwrap();
    assert!(
        positions.iter().all(|pos| *pos == (ANCHOR_X, ANCHOR_Y)),
        "宿主报的起点没改口，候选窗不得挪到组合末尾；实际下发={positions:?} cs={cs:?}"
    );
    assert_ne!(
        (cs.0, cs.1),
        (2378, REPORTED_Y),
        "锚点被重锁到组合末尾——判据又去问 `composition_start` 而不是宿主了"
    );
}

/// 宿主改口时，重锁目标必须是它新报的起点，不是当前 caret。
///
/// 输入框变高时 QQ 如实改报 compStart（x 不变、y 随框移动），pair guard 不成立而落进逃生阀；
/// 该分支默认目标是当前 caret（组合末端），重锁后 guard 反而把错锚点钉住。
/// 对已声明如实上报的宿主，宿主手里就有正确答案，不该拿 caret 猜。
#[test]
fn a_trusted_host_changing_its_reported_start_relocks_to_that_start_not_the_caret() {
    const START_X: i32 = 1832;
    const OLD_Y: i32 = 1674;
    const NEW_Y: i32 = 1606; // 输入框变高，整体上移 68px
    const LINE_H: i32 = 32;
    const CARET_AT_TAIL: i32 = 2446; // 组合末端，距真起点 614px

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    c.active_compat.lock().unwrap().composition_start_pair_guard = true;
    *c.last_valid_caret.lock().unwrap() = (START_X, OLD_Y, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "daduoshuqingkuang".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "大多数".into(),
            ..Default::default()
        }];
        st.caret_x = START_X;
        st.caret_y = OLD_Y;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((START_X, OLD_Y)), "首显于组合起点");
    *c.composition_start.lock().unwrap() = (START_X, OLD_Y, true);

    // 上移前的一帧：QQ 的组合降级帧，caret 退化成组合起点本身（真机形态）。
    // 它同时把「上一帧宿主报的起点」喂进去，并把基准落在起点上——真机里正是这个基准
    // 让下一帧的 caret 跨度达到 614px。
    *c.caret_baseline.lock().unwrap() = (START_X, OLD_Y, true);
    c.handle_caret_update(&CaretData {
        x: START_X,
        y: OLD_Y,
        height: LINE_H,
        composition_start_x: START_X,
        composition_start_y: OLD_Y,
        source: wind_ipc::protocol::caret_source::TSF_COMPOSITION,
        composition_rect: None,
    });
    drain_positions(&rx);

    // 输入框变高：宿主改报新起点（x 不变、y 上移），caret 在组合末端。
    // 偏移 614/68 越过 3 倍行高，逃生阀介入——目标必须取宿主新报的起点。
    c.handle_caret_update(&CaretData {
        x: CARET_AT_TAIL,
        y: NEW_Y,
        height: LINE_H,
        composition_start_x: START_X,
        composition_start_y: NEW_Y,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });

    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (START_X, NEW_Y),
        "宿主改口时应重锁到它新报的起点；锁成 caret 会让锚点偏出 614px 且被 guard 钉死"
    );
    assert!(
        drain_positions(&rx)
            .into_iter()
            .all(|pos| pos == (START_X, NEW_Y)),
        "候选窗应落在宿主新报的起点上"
    );
}

/// 同一条逃生阀，没有 per-app 声明的宿主必须保持原行为：重锁到当前 caret。
///
/// 微信/WPS/EverEdit 报的 compStart 有陈旧、有坐标系混用的历史，信它等于关掉逃生阀。
#[test]
fn an_untrusted_host_still_relocks_to_the_caret() {
    const LINE_H: i32 = 20;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    // 刻意不开 composition_start_pair_guard
    *c.last_valid_caret.lock().unwrap() = (1030, 987, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 1030;
        st.caret_y = 987;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((1030, 987)), "首显于陈旧坐标");
    *c.composition_start.lock().unwrap() = (1030, 987, true);

    // 宿主报了一个「新」起点，但它不可信——仍以 caret 自愈。
    c.handle_caret_update(&CaretData {
        x: 493,
        y: 988,
        height: LINE_H,
        composition_start_x: 700,
        composition_start_y: 988,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (493, 988),
        "未声明如实上报的宿主，逃生阀仍须重锁到 caret"
    );
}

/// 有组合矩形时，锚点取它的左下角——跨行自动落到最后一行行首。
///
/// QQ、飞书、记事本跨行后 `compStart.y` 都停在上一行，只有矩形的 `bottom` 跟到当前行
/// （见 `docs/architecture/caret-position-host-compat-notes.md` 的「组合矩形的三种形态」）。
#[test]
fn composition_rect_anchors_to_the_last_line_start_across_hosts() {
    // (宿主, rect, caret, 旧 compStart)
    let cases = [
        (
            "QQ",
            (1876, 1072, 1892, 1146),
            (1892, 1146),
            (1876, 1106),
            34,
        ),
        (
            "飞书",
            (2678, 1602, 2692, 1678),
            (2692, 1678),
            (2678, 1634),
            32,
        ),
        (
            "记事本",
            (1578, 886, 1898, 1002),
            (1898, 1002),
            (1578, 960),
            42,
        ),
    ];
    for (host, rect, caret, comp_start, line_h) in cases {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
        *c.last_valid_caret.lock().unwrap() = (rect.0, rect.1, line_h);
        c.last_sane_caret_height
            .store(line_h, std::sync::atomic::Ordering::Relaxed);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "dabufengxingkuang".into();
            st.candidates = vec![wind_candidate::Candidate {
                text: "大部".into(),
                ..Default::default()
            }];
            st.caret_x = rect.0;
            st.caret_y = rect.1;
            st.caret_height = line_h;
        }
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        drain_positions(&rx);
        // 首显把锚点锁在了第一行（idle_anchor 路径的行为）
        *c.composition_start.lock().unwrap() = (comp_start.0, comp_start.1, true);

        c.handle_caret_update(&CaretData {
            x: caret.0,
            y: caret.1,
            height: line_h,
            composition_start_x: comp_start.0,
            composition_start_y: comp_start.1,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some(rect),
        });

        let cs = *c.composition_start.lock().unwrap();
        assert_eq!(
            (cs.0, cs.1),
            (rect.0, rect.3),
            "{host}：锚点必须取组合矩形的左下角（最后一行行首），而不是折叠到首行的 compStart"
        );
        assert!(
            drain_positions(&rx)
                .into_iter()
                .all(|pos| pos == (rect.0, rect.3)),
            "{host}：候选窗必须落在最后一行行首"
        );
    }
}

/// 单行组合：矩形的左下角与组合起点等价，锚点不因逐字输入右移。
/// 守的是「矩形每帧更新」不会退化成「跟着 caret 跑」——矩形的 `left` 恒定，只有 `right` 增长。
#[test]
fn composition_rect_left_stays_put_while_typing_on_one_line() {
    const LEFT: i32 = 1876;
    const TOP: i32 = 1072;
    const BOTTOM: i32 = 1106;
    const LINE_H: i32 = 34;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (LEFT, BOTTOM, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "da".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "大".into(),
            ..Default::default()
        }];
        st.caret_x = LEFT;
        st.caret_y = BOTTOM;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (LEFT, BOTTOM, true);

    // 逐字输入：right 增长、left 不动
    for right in [1892, 1910, 1932, 1954, 1972, 1988] {
        c.handle_caret_update(&CaretData {
            x: right,
            y: BOTTOM,
            height: LINE_H,
            composition_start_x: LEFT,
            composition_start_y: BOTTOM,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((LEFT, TOP, right, BOTTOM)),
        });
        let cs = *c.composition_start.lock().unwrap();
        assert_eq!(
            (cs.0, cs.1),
            (LEFT, BOTTOM),
            "单行输入时锚点必须钉在行首，right={right}"
        );
    }
    assert!(
        drain_positions(&rx)
            .into_iter()
            .all(|pos| pos == (LEFT, BOTTOM)),
        "候选窗不得随组合增长右移"
    );
}

/// 上一条的反面：宿主把 `left` 报错时，同组合同行内不得跟着它走。
///
/// Tabby（xterm.js）组合内容只增不减，`left` 却逐码右移，跟随就会让候选窗抖动（见
/// `docs/architecture/caret-position-host-compat-notes.md` 的 H 类）。与上一条是一对：
/// 只留任一条，把判据写成恒真或恒假都能绿。
#[test]
fn composition_rect_left_that_drifts_within_one_composition_is_pinned() {
    const TOP: i32 = 1174;
    const BOTTOM: i32 = 1205;
    const LINE_H: i32 = 31;
    // 首帧的 left 才是真起点，后三帧都是宿主报错的漂移值
    const TRUE_START: i32 = 379;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (TRUE_START, BOTTOM, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "w".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "我".into(),
            ..Default::default()
        }];
        st.caret_x = TRUE_START;
        st.caret_y = BOTTOM;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);

    // 靶机原始帧，顺序即用户的击键顺序
    let frames = [
        (379, 393), // 首帧：矩形第一次给锚点，必须采信
        (381, 405),
        (384, 417),
        (390, 429),
    ];
    for (left, right) in frames {
        c.handle_caret_update(&CaretData {
            x: right,
            y: BOTTOM,
            height: LINE_H,
            composition_start_x: left,
            composition_start_y: BOTTOM,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((left, TOP, right, BOTTOM)),
        });
        let cs = *c.composition_start.lock().unwrap();
        assert_eq!(
            (cs.0, cs.1),
            (TRUE_START, BOTTOM),
            "同组合同行内起点按定义不动，宿主报 left={left} 也不得跟"
        );
    }
    assert!(
        drain_positions(&rx)
            .into_iter()
            .all(|pos| pos == (TRUE_START, BOTTOM)),
        "候选窗不得每打一码挪一下"
    );

    // ── 跨行必须放行 ──
    // 「矩形自动落到最后一行行首」是这条路径存在的理由，钉列不能把它一起钉掉。
    const NEXT_LINE: i32 = BOTTOM + LINE_H;
    c.handle_caret_update(&CaretData {
        x: 120,
        y: NEXT_LINE,
        height: LINE_H,
        composition_start_x: TRUE_START,
        composition_start_y: BOTTOM,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some((96, BOTTOM, 120, NEXT_LINE)),
    });
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (96, NEXT_LINE),
        "换行后锚点必须跟到最后一行行首——本条挡的是同行漂移，不是跨行更新"
    );
}

/// 锁定之后矩形仍是「每帧如实」：别的写入者改过 `cs`，下一帧矩形必须把它纠回来。
///
/// `composition_start` 在本组合里还有两个写入者（`shown_anchor` 回灌与大偏移逃生阀），
/// 都以「矩形每帧如实到达」为前提。锁定分支若什么都不做，一帧全零矩形让逃生阀把 `cs` 改成
/// 组合末端后，本行余下整段再也纠不回来；所以锁存的是值，锁定分支把 `cs` 恢复成本轮锚点。
#[test]
fn the_locked_rect_anchor_still_corrects_cs_after_another_writer_touched_it() {
    const TOP: i32 = 1174;
    const BOTTOM: i32 = 1205;
    const LINE_H: i32 = 31;
    const TRUE_START: i32 = 379;

    let (c, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (TRUE_START, BOTTOM, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "w".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "我".into(),
            ..Default::default()
        }];
        st.caret_x = TRUE_START;
        st.caret_y = BOTTOM;
        st.caret_height = LINE_H;
    }
    let frame = |left: i32, right: i32| CaretData {
        x: right,
        y: BOTTOM,
        height: LINE_H,
        composition_start_x: left,
        composition_start_y: BOTTOM,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some((left, TOP, right, BOTTOM)),
    };

    // 首帧：矩形锁定真起点
    c.handle_caret_update(&frame(TRUE_START, 393));
    assert_eq!(
        c.composition_start.lock().unwrap().0,
        TRUE_START,
        "前提：本轮锚点已锁定"
    );

    // 模拟另一个写入者（逃生阀 / shown_anchor 回灌）在无矩形的那一帧把 cs 改成组合末端
    *c.composition_start.lock().unwrap() = (9999, BOTTOM, true);

    // 下一帧矩形照常到达（宿主报的 left 仍在漂移）
    c.handle_caret_update(&frame(384, 417));
    // 先取值再断言：`Mutex` 非重入，同一表达式里锁两次会挂住（见另一条测试的同款注释）
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (TRUE_START, BOTTOM),
        "矩形必须把 cs 纠回本轮锚点——只挡不写会让那次改写永久生效，本行再也回不来"
    );
}

/// 组合结束后，矩形必须能重新给锚点——两把锁各清各的，漏清就钉死在上一组合。
///
/// `locked_rect_anchor` 与 `composition_start.2` 是两个状态：后者由 DLL 上报的 compStart
/// 持有，新组合首帧常是陈旧值，要靠矩形修正。把两把锁合并，这次修正就没了。
#[test]
fn a_new_composition_lets_the_rect_anchor_again() {
    const TOP: i32 = 1174;
    const BOTTOM: i32 = 1205;
    const LINE_H: i32 = 31;

    let (c, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (379, BOTTOM, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "w".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "我".into(),
            ..Default::default()
        }];
        st.caret_x = 379;
        st.caret_y = BOTTOM;
        st.caret_height = LINE_H;
    }
    let frame = |c: &Coordinator, left: i32, right: i32| {
        c.handle_caret_update(&CaretData {
            x: right,
            y: BOTTOM,
            height: LINE_H,
            composition_start_x: left,
            composition_start_y: BOTTOM,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((left, TOP, right, BOTTOM)),
        });
    };

    frame(&c, 379, 393);
    frame(&c, 381, 405);
    assert_eq!(
        c.composition_start.lock().unwrap().0,
        379,
        "前提：本组合内已钉住（这条断言不成立的话下面证明不了任何事）"
    );

    // 上屏 ⇒ 组合结束
    c.reset_first_show();
    assert!(
        !c.locked_rect_anchor.lock().unwrap().2,
        "组合结束必须解锁矩形锚点"
    );

    // 新组合落在别处：必须锚到新位置，而不是留在 379
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "x".into();
    }
    frame(&c, 442, 453);
    // 先取值再断言：`Mutex` 非重入，把两个 `lock()` 写进同一个表达式会死锁
    // （guard 在表达式求值结束前不释放）——本测试初版就是这么挂住的。
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (442, BOTTOM),
        "新组合的矩形必须被采信——这正是路径 A 锁进陈旧 compStart 后唯一的修正机会"
    );
}

/// Word 按键期同步取坐标被拒后，DLL 回退发出的 `gui_caret` 帧是固定的陈旧位置。本组合已
/// 收到过 TSF 域坐标后，这类帧只能是噪声：不得改写缓存、重锁组合起点、重显候选窗，
/// 否则候选窗在两者之间上下来回跳。
#[test]
fn a_gui_caret_frame_cannot_relocate_a_composition_that_has_tsf_coordinates() {
    const TOP: i32 = 518;
    const BOTTOM: i32 = 541;
    const LINE_H: i32 = 23;

    let (c, _rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (1472, BOTTOM, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "xg".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "线".into(),
            ..Default::default()
        }];
    }
    c.handle_caret_update(&CaretData {
        x: 1488,
        y: BOTTOM,
        height: LINE_H,
        composition_start_x: 1472,
        composition_start_y: BOTTOM,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some((1472, TOP, 1488, BOTTOM)),
    });
    let before = *c.composition_start.lock().unwrap();
    assert_eq!(
        (before.0, before.1),
        (1472, BOTTOM),
        "前提：TSF 帧已锁定组合起点"
    );

    // 回退链交出的 Win32 光标：无组合起点、无组合矩形
    c.handle_caret_update(&CaretData {
        x: 1434,
        y: 272,
        height: 20,
        composition_start_x: 0,
        composition_start_y: 0,
        source: wind_ipc::protocol::caret_source::GUI_CARET,
        composition_rect: None,
    });

    let cs = *c.composition_start.lock().unwrap();
    assert_eq!((cs.0, cs.1), (1472, BOTTOM), "GUI 帧不得重锁组合起点");
    let (cy, src) = {
        let st = c.state.lock().unwrap();
        (st.caret_y, st.caret_source)
    };
    assert_eq!(cy, BOTTOM, "GUI 帧不得覆盖坐标缓存");
    assert_eq!(src, wind_ipc::protocol::caret_source::TSF_SELECTION);

    // 组合结束后，GUI 帧恢复原有语义（组合前空闲上报等），不能被永久屏蔽
    c.reset_first_show();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer.clear();
        st.candidates.clear();
    }
    c.handle_caret_update(&CaretData {
        x: 1434,
        y: 272,
        height: 20,
        composition_start_x: 0,
        composition_start_y: 0,
        source: wind_ipc::protocol::caret_source::GUI_CARET,
        composition_rect: None,
    });
    let cy = c.state.lock().unwrap().caret_y;
    assert_eq!(cy, 272, "下一轮开始前 GUI 帧仍应照常写缓存");
}

/// 只返回首行的宿主：`caret.y` 会大于 `rect.bottom` ⇒ 判定不可信 ⇒ 回退既有逻辑。
/// 已知宿主都返回真包围盒，但规范不保证；这条守「遇到那种宿主不会更糟」。
#[test]
fn a_first_line_only_rect_is_distrusted_and_falls_back() {
    const LINE_H: i32 = 34;
    // 矩形只覆盖第一行，caret 已经在第二行
    let rect = (1876, 1072, 2400, 1106);
    let caret = (1892, 1146);

    // 起始锚点刻意不等于矩形左下角：否则「采信矩形」与「回退既有逻辑」
    // 会产生同一个结果，测试区分不出两者，等于什么都没测。
    const START: (i32, i32) = (1500, 1106);

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (START.0, START.1, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "dabu".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "大部".into(),
            ..Default::default()
        }];
        st.caret_x = START.0;
        st.caret_y = START.1;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (START.0, START.1, true);

    c.handle_caret_update(&CaretData {
        x: caret.0,
        y: caret.1,
        height: LINE_H,
        composition_start_x: rect.0,
        composition_start_y: rect.3,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some(rect),
    });

    let cs = *c.composition_start.lock().unwrap();
    assert_ne!(
        (cs.0, cs.1),
        (rect.0, rect.3),
        "caret 落在矩形之下时不得采信该矩形——那是「只返回首行」的形态，采信它会把锚点钉死在上一行"
    );
}

/// 「插入点形态」的宿主必须整条回退——它们的矩形不是范围，是一个点（宽 0~2px，`left`
/// 恒等于 caret.x 并跟着插入点走：WPS 文字、WPS 表格、Word cached 帧）。
///
/// 回退之后走既有锚点逻辑，那条路对这些宿主本就是调好的：WPS 文字靠「组合起点本组合内
/// 不再更新」钉住，表格类每字换一次 docMgr、候选窗随之跟到新单元格。矩形若每帧更新反而
/// 拆了前者的保护。
#[test]
fn an_insertion_point_shaped_rect_is_distrusted_for_every_host() {
    // (宿主, 矩形宽度, 起始 x, 行顶, 行底, 行高)
    let cases = [
        ("WPS 文字", 2, 1603, 617, 651, 34),
        ("WPS 表格 et", 1, 1809, 961, 997, 36),
        ("Word cached", 0, 1758, 653, 697, 44),
    ];
    for (host, w, x0, top, bottom, line_h) in cases {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
        *c.last_valid_caret.lock().unwrap() = (x0, bottom, line_h);
        c.last_sane_caret_height
            .store(line_h, std::sync::atomic::Ordering::Relaxed);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "wo".into();
            st.candidates = vec![wind_candidate::Candidate {
                text: "我".into(),
                ..Default::default()
            }];
            st.caret_x = x0;
            st.caret_y = bottom;
            st.caret_height = line_h;
        }
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        drain_positions(&rx);
        // 既有逻辑把锚点锁在起始位置
        *c.composition_start.lock().unwrap() = (x0, bottom, true);

        // 矩形整体右移（宽度不增长）——插入点形态的真实表现
        for step in [0, 14, 28] {
            let x = x0 + step;
            c.handle_caret_update(&CaretData {
                x,
                y: bottom,
                height: line_h,
                composition_start_x: x,
                composition_start_y: bottom,
                source: wind_ipc::protocol::caret_source::TSF_SELECTION,
                composition_rect: Some((x, top, x + w, bottom)),
            });
            let cs = *c.composition_start.lock().unwrap();
            assert_eq!(
                (cs.0, cs.1),
                (x0, bottom),
                "{host}：矩形是插入点不是范围，不得据此移动锚点（step={step}）"
            );
        }
        assert!(
            drain_positions(&rx)
                .into_iter()
                .all(|pos| pos == (x0, bottom)),
            "{host}：候选窗不得随插入点右移"
        );
    }
}

/// Word 的真包围盒仍照常采信——上一条挡掉的只是它那些退化的 cached 帧。跨两行时
/// compStart 停在第一行，只有矩形的左下角是第二行行首。
#[test]
fn word_real_bounding_box_is_still_trusted() {
    const LINE_H: i32 = 43;
    let rect = (1699, 701, 2710, 792);

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (rect.0, 745, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "dabufengxing".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "大部".into(),
            ..Default::default()
        }];
        st.caret_x = rect.0;
        st.caret_y = 745;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (rect.0, 745, true);

    c.handle_caret_update(&CaretData {
        x: 2710,
        y: rect.3,
        height: LINE_H,
        composition_start_x: 1699,
        composition_start_y: 745,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some(rect),
    });

    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (rect.0, rect.3),
        "真包围盒的左下角应成为锚点（第二行行首），而不是停在首行的 compStart"
    );
}

/// 布局退化的整帧不可信——不只是它的矩形，连 caret 也不能拿去重锁。
///
/// Word 的 selection 帧与 cached 帧（三个坐标全是另一处的陈旧值）交替到达时，逃生阀若拿
/// cached 帧的 caret 重锁，锚点就在两处之间来回跳。`left == right` 表示这次 GetTextExt
/// 没算出布局，同帧的 caret / compStart 一并不可信。
#[test]
fn a_degenerate_layout_frame_cannot_relock_via_the_escape_valve() {
    const LINE_H: i32 = 43;
    let good = (1952, 701, 1985, 841); // 真包围盒，左下角 (1952,841)
    let degenerate = (1758, 653, 1758, 697); // w=0，坐标是另一处的陈旧值

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (good.0, good.3, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "dabufengxingkuang".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "大部".into(),
            ..Default::default()
        }];
        st.caret_x = good.0;
        st.caret_y = good.3;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);

    // selection 帧建立锚点
    c.handle_caret_update(&CaretData {
        x: 1985,
        y: good.3,
        height: LINE_H,
        composition_start_x: 1952,
        composition_start_y: 745,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some(good),
    });
    assert_eq!(
        {
            let cs = *c.composition_start.lock().unwrap();
            (cs.0, cs.1)
        },
        (good.0, good.3),
        "真包围盒的左下角应成为锚点"
    );

    // 退化 cached 帧反复到达：锚点必须纹丝不动
    for _ in 0..4 {
        c.handle_caret_update(&CaretData {
            x: 1758,
            y: 697,
            height: 44,
            composition_start_x: 1758,
            composition_start_y: 697,
            source: wind_ipc::protocol::caret_source::TSF_CACHED,
            composition_rect: Some(degenerate),
        });
        let cs = *c.composition_start.lock().unwrap();
        assert_eq!(
            (cs.0, cs.1),
            (good.0, good.3),
            "布局退化的帧不得重锁锚点——它的 caret 与矩形出自同一次失败的布局查询"
        );
    }
    assert!(
        drain_positions(&rx)
            .into_iter()
            .all(|pos| pos == (good.0, good.3)),
        "候选窗不得在两处之间抖动"
    );
}

/// 「起点跟着 caret 走」的宿主必须抑制逃生阀，否则锚点被一路推走且回不来。
///
/// WPS 的 compStart 每帧与 caret 同步递增，打字时逃生阀反复重锁把锚点推远；删除时逐字回退
/// 每步只有十几像素，永远够不到 3 倍行高的阈值，锚点永久卡住。逃生阀只认单帧大跳，
/// 删除却是小步累积的。
#[test]
fn an_insertion_point_host_keeps_its_first_anchor_through_typing_and_deleting() {
    const TOP: i32 = 827;
    const BOTTOM: i32 = 861;
    const LINE_H: i32 = 34;
    const REAL_START: i32 = 1830;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (REAL_START, BOTTOM, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "fafasdfdddd".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "啊方案".into(),
            ..Default::default()
        }];
        st.caret_x = REAL_START;
        st.caret_y = BOTTOM;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (REAL_START, BOTTOM, true);

    // 打字：caret 与 compStart 同步右移，一路走到 2591（远超 3 倍行高）
    let mut x = REAL_START;
    while x < 2591 {
        x += 30;
        c.handle_caret_update(&CaretData {
            x,
            y: BOTTOM,
            height: LINE_H,
            composition_start_x: x,
            composition_start_y: BOTTOM,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((x, TOP, x + 2, BOTTOM)),
        });
    }
    let after_typing = *c.composition_start.lock().unwrap();
    assert_eq!(
        (after_typing.0, after_typing.1),
        (REAL_START, BOTTOM),
        "打字期间锚点必须钉在首帧真实起点；被逃生阀推走后删除就再也回不来"
    );

    // 逐字删除回退：每步 30px，够不到阈值——锚点仍须不动
    while x > REAL_START {
        x -= 30;
        c.handle_caret_update(&CaretData {
            x,
            y: BOTTOM,
            height: LINE_H,
            composition_start_x: x,
            composition_start_y: BOTTOM,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((x, TOP, x + 2, BOTTOM)),
        });
    }
    let after_delete = *c.composition_start.lock().unwrap();
    assert_eq!(
        (after_delete.0, after_delete.1),
        (REAL_START, BOTTOM),
        "删除结束后锚点仍应在真实起点"
    );
    assert!(
        drain_positions(&rx)
            .into_iter()
            .all(|pos| pos == (REAL_START, BOTTOM)),
        "候选窗全程不得离开组合起点"
    );
}

/// 钉住的是列不是行：跨行时锚点的 y 要跟到 caret 所在行。
///
/// 这类宿主给不出组合矩形，不知道最后一行的行首在哪；但 caret.y 说明了当前在哪一行，
/// 沿用钉住的 x、只换 y，候选窗就落到正确的行上。
#[test]
fn a_pinned_anchor_follows_the_caret_row_across_a_wrap() {
    const PINNED_X: i32 = 1647;
    const ROW1: i32 = 1140;
    const ROW2: i32 = 1182; // 下一行，差一个行高
    const LINE_H: i32 = 34;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    c.active_compat.lock().unwrap().pin_anchor_when_start_drifts = true;
    *c.last_valid_caret.lock().unwrap() = (PINNED_X, ROW1, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "sfgefe".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "是否".into(),
            ..Default::default()
        }];
        st.caret_x = PINNED_X;
        st.caret_y = ROW1;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (PINNED_X, ROW1, true);

    // 同一行内继续打字：插入点形态的矩形整体右移，锚点必须纹丝不动
    for x in [2000, 2200, 2400] {
        c.handle_caret_update(&CaretData {
            x,
            y: ROW1,
            height: LINE_H,
            composition_start_x: x,
            composition_start_y: ROW1,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((x, ROW1 - LINE_H, x + 2, ROW1)),
        });
        let cs = *c.composition_start.lock().unwrap();
        assert_eq!(
            (cs.0, cs.1),
            (PINNED_X, ROW1),
            "同一行内锚点必须钉住（x 与 y 都不动），x={x}"
        );
    }

    // 换行：caret 落到下一行，锚点的 y 要跟过去，x 保持钉住
    c.handle_caret_update(&CaretData {
        x: 2472,
        y: ROW2,
        height: LINE_H,
        composition_start_x: 2472,
        composition_start_y: ROW2,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some((2472, ROW2 - LINE_H, 2474, ROW2)),
    });
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (PINNED_X, ROW2),
        "跨行后锚点应保持钉住的列、跟到 caret 所在行——否则候选窗压住新行"
    );

    // 删回上一行：y 也要跟回去
    c.handle_caret_update(&CaretData {
        x: 2400,
        y: ROW1,
        height: LINE_H,
        composition_start_x: 2400,
        composition_start_y: ROW1,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some((2400, ROW1 - LINE_H, 2402, ROW1)),
    });
    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (PINNED_X, ROW1),
        "删回上一行时 y 应跟回——它跟的是当前行，不是累计位移"
    );
}

/// 未开该开关的宿主（Excel / 表格类）不受「跟行」影响，行为一字不变。
#[test]
fn row_following_is_per_app_too() {
    const LINE_H: i32 = 36;
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    // 刻意不开 pin_anchor_when_start_drifts
    *c.last_valid_caret.lock().unwrap() = (1809, 997, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "wo".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "我".into(),
            ..Default::default()
        }];
        st.caret_x = 1809;
        st.caret_y = 997;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (1809, 997, true);

    c.handle_caret_update(&CaretData {
        x: 1839,
        y: 1033,
        height: LINE_H,
        composition_start_x: 1839,
        composition_start_y: 1033,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: Some((1839, 997, 1840, 1033)),
    });
    let cs = *c.composition_start.lock().unwrap();
    assert_ne!(
        (cs.0, cs.1),
        (1809, 1033),
        "未声明的宿主不得走「x 钉住 y 跟行」这条——它们的跟随由既有逻辑负责"
    );
}

/// 换行后 caret 落在 `rect.left` 上，不得据此把真范围误判成插入点。
///
/// 「组合起点必须在插入点左边」（`left >= caret.x` 判插入点）只在单行成立：换行后 caret
/// 回到行首恰好等于 left。所以判据看宽度——光标 0~2px，真包围盒最窄一个字符 16~17px。
#[test]
fn a_wrapped_caret_at_rect_left_is_still_a_real_bounding_box() {
    const LINE_H: i32 = 37;
    let rect = (2524, 797, 2926, 834); // w=402，真范围
    let caret = (2524, 834); // 换行后回到行首，x 恰好等于 rect.left
    let stale_start = (2915, 834); // 删除前遗留的组合起点

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (stale_start.0, stale_start.1, LINE_H);
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "dddddd".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "等等".into(),
            ..Default::default()
        }];
        st.caret_x = stale_start.0;
        st.caret_y = stale_start.1;
        st.caret_height = LINE_H;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    drain_positions(&rx);
    *c.composition_start.lock().unwrap() = (stale_start.0, stale_start.1, true);

    c.handle_caret_update(&CaretData {
        x: caret.0,
        y: caret.1,
        height: LINE_H,
        composition_start_x: stale_start.0,
        composition_start_y: stale_start.1,
        source: wind_ipc::protocol::caret_source::TSF_CACHED,
        composition_rect: Some(rect),
    });

    let cs = *c.composition_start.lock().unwrap();
    assert_eq!(
        (cs.0, cs.1),
        (rect.0, rect.3),
        "w=402 是真包围盒，锚点应取其左下角（换行后那一行的行首），而不是停在陈旧的组合起点"
    );
}

/// 宽度判据的两侧边界：光标宽度级别判为插入点，一个字符宽判为真范围。
/// 阈值两侧都留了 4 倍以上余量（光标 0~2px，真包围盒最窄 16~17px）。
#[test]
fn insertion_point_width_threshold_separates_the_two_populations() {
    const LINE_H: i32 = 34;
    const TOP: i32 = 1072;
    const BOTTOM: i32 = 1106;
    // (宽度, 是否应被采信为真范围)
    let cases = [(0, false), (1, false), (2, false), (16, true), (402, true)];
    for (w, should_trust) in cases {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
        set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
        // 首显位置也要设在 9999：它同时决定 shown_anchor 与 caret_baseline。若首显在 1876，
        // 下面那帧 dx 为 0 会走微移路径并把 shown_anchor 回灌进 cs，测的就不是宽度判据了。
        *c.last_valid_caret.lock().unwrap() = (9999, BOTTOM, LINE_H);
        c.last_sane_caret_height
            .store(LINE_H, std::sync::atomic::Ordering::Relaxed);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".into();
            st.candidates = vec![wind_candidate::Candidate {
                text: "啊".into(),
                ..Default::default()
            }];
            st.caret_x = 9999;
            st.caret_y = BOTTOM;
            st.caret_height = LINE_H;
        }
        {
            let st = c.state.lock().unwrap();
            c.notify_ui_update(&st);
        }
        drain_positions(&rx);
        // 起点刻意与矩形左下角不同，才分得出「采信了矩形」和「没采信」
        *c.composition_start.lock().unwrap() = (9999, BOTTOM, true);

        c.handle_caret_update(&CaretData {
            x: 1876 + w,
            y: BOTTOM,
            height: LINE_H,
            composition_start_x: 1876,
            composition_start_y: BOTTOM,
            source: wind_ipc::protocol::caret_source::TSF_SELECTION,
            composition_rect: Some((1876, TOP, 1876 + w, BOTTOM)),
        });

        let cs = *c.composition_start.lock().unwrap();
        let trusted = (cs.0, cs.1) == (1876, BOTTOM);
        assert_eq!(
            trusted, should_trust,
            "w={w} 的矩形判断错了（trusted={trusted}，应为 {should_trust}）"
        );
    }
}

/// `composition_start_pair_guard` 必须是按宿主开启的窄保护，默认行为一字不差：
/// 没有 compat 证据时协调器分不清稳定与陈旧的 compStart，必须保留大偏移逃生阀。
#[test]
fn composition_start_pair_guard_is_per_app_and_off_by_default() {
    const START_X: i32 = 598;
    const Y: i32 = 585;
    const LINE_H: i32 = 18;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "daduo".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 644;
        st.caret_y = Y;
        st.caret_height = LINE_H;
    }
    *c.composition_start.lock().unwrap() = (START_X, Y, true);
    *c.caret_baseline.lock().unwrap() = (644, Y, true);
    *c.candidate_shown.lock().unwrap() = true;
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);

    c.handle_caret_update(&CaretData {
        x: START_X,
        y: Y,
        height: LINE_H,
        composition_start_x: START_X,
        composition_start_y: Y,
        source: wind_ipc::protocol::caret_source::TSF_COMPOSITION,
        composition_rect: None,
    });
    let _ = drain_positions(&rx);
    c.handle_caret_update(&CaretData {
        x: 653,
        y: Y,
        height: LINE_H,
        composition_start_x: START_X,
        composition_start_y: Y,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });

    assert_eq!(
        *c.composition_start.lock().unwrap(),
        (653, Y, true),
        "未命中 compat 时须保留原 caret 重锁语义，不能全局信任 reported compStart"
    );
}

/// compat 保护只认完整、可信的帧对。异常 compStart 不得成为重锁目标；无法确认帧对时
/// 回到旧 caret 逃生阀，至少候选窗仍能落在已经通过 `now_valid` 的当前插入点。
#[test]
fn composition_start_pair_guard_never_relocks_to_invalid_reported_start() {
    const START_X: i32 = 598;
    const Y: i32 = 585;
    const LINE_H: i32 = 18;

    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    c.active_compat.lock().unwrap().composition_start_pair_guard = true;
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "daduo".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = START_X;
        st.caret_y = Y;
        st.caret_height = LINE_H;
        st.caret_source = wind_ipc::protocol::caret_source::TSF_COMPOSITION;
    }
    *c.composition_start.lock().unwrap() = (START_X, Y, true);
    *c.caret_baseline.lock().unwrap() = (START_X, Y, true);
    *c.candidate_shown.lock().unwrap() = true;
    c.last_sane_caret_height
        .store(LINE_H, std::sync::atomic::Ordering::Relaxed);

    c.handle_caret_update(&CaretData {
        x: 653,
        y: Y,
        height: LINE_H,
        composition_start_x: i32::MIN,
        composition_start_y: Y,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });

    assert_eq!(
        *c.composition_start.lock().unwrap(),
        (653, Y, true),
        "溢出级异常 reported compStart 不得成为锚点；帧对不可信时退回已校验的 caret"
    );
    assert!(
        drain_positions(&rx).into_iter().all(|pos| pos == (653, Y)),
        "任何下发都不得携带异常 reported compStart"
    );
}

/// 行高「尚未观测到」与「被单帧退化值污染」是两种失效方式，都不能让容差塌缩。
///
/// 写入闸 `> 1` 只挡住了后者。初值若是 0，服务刚启动时 settle 与重锁阈值都会落到下限 3px：
/// 前者每次自动上屏回退一个字宽，后者任何微移都重锁组合起点。
#[test]
fn line_height_has_a_conservative_floor_before_any_observation() {
    let c = coord();
    assert!(
        c.last_sane_caret_height
            .load(std::sync::atomic::Ordering::Relaxed)
            >= FALLBACK_LINE_HEIGHT,
        "尚未观测到行高时必须有保守下限，否则两个容差同时塌到 3px"
    );

    // 宿主一上来就报退化高度也不得把它拉下去
    let c2 = coord();
    set_mode(&c2, wind_config::app_compat::FirstShowMode::Fast);
    {
        let mut st = c2.state.lock().unwrap();
        st.input_buffer = "a".into();
    }
    c2.handle_caret_update(&probe_at(300, 400, 1));
    assert!(
        c2.last_sane_caret_height
            .load(std::sync::atomic::Ordering::Relaxed)
            >= FALLBACK_LINE_HEIGHT,
        "h=1 是宿主的退化值，不得写进行高估计"
    );
}

/// 方向性守门：上屏不得清 `caret_cache_verified`。
///
/// 动机看似合理（上屏改变了插入点），但清了之后下一个组合走 600ms 长兜底，而长按时每个
/// 组合只活一百多毫秒，被 `reset_first_show` 作废而永不到期，候选窗长时间停在旧位置。
/// 权威坐标比组合寿命还晚到的宿主上这个死结无解，否掉的是方向而不是超时值。
#[test]
fn commit_must_not_invalidate_the_cache() {
    for guard in [false, true] {
        let c = coord();
        set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
        c.active_compat.lock().unwrap().stale_probe_guard = guard;
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        c.reset_first_show();
        assert!(
            c.caret_cache_verified
                .load(std::sync::atomic::Ordering::Relaxed),
            "上屏不得清掉缓存可信标志（stale_probe_guard={guard}）：\
                 清了之后下一个组合走 600ms 长兜底，而长按时组合只活 ~128ms，\
                 兜底被 reset_first_show 作废而永不到期，候选窗整段停在原地"
        );
    }
}

/// 组合起点钉住位置后，同一个偏差不得反复触发校正。
///
/// 基准记的是「上一次被认可的插入点」，不是「候选窗画在哪」；跟着显示位置走的话，
/// 基准与宿主插入点之间有恒定差值，每来一帧都判「要校正」。
#[test]
fn same_coordinate_reported_twice_reshows_only_once() {
    let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), None);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    *c.last_valid_caret.lock().unwrap() = (1643, 747, 20);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".into();
        st.candidates = vec![wind_candidate::Candidate {
            text: "测".into(),
            ..Default::default()
        }];
        st.caret_x = 1643;
        st.caret_y = 747;
        st.caret_height = 20;
    }
    {
        let st = c.state.lock().unwrap();
        c.notify_ui_update(&st);
    }
    assert_eq!(last_pos(&rx), Some((1643, 747)), "首显位置");
    // 组合起点锁定 ⇒ 此后候选窗位置被钉住，不随光标走
    *c.composition_start.lock().unwrap() = (1643, 747, true);

    // 宿主报的插入点已前移 80px（超出水平 settle 容差 2×0.8×20=32），应校正一次
    c.handle_caret_update(&probe_at(1723, 747, 20));
    assert!(last_pos(&rx).is_some(), "首次超出容差应当校正");

    // 同一坐标再报：它已被认可，不该再判一次
    c.handle_caret_update(&probe_at(1723, 747, 20));
    assert_eq!(
        last_pos(&rx),
        None,
        "同一个插入点重复上报不得反复触发校正——基准该记插入点，不是显示位置"
    );
}

/// probe 可信的宿主不得走 idle_anchor 抢跑，也不得抢锁组合起点。
///
/// 「组合前空闲上报可信」只在 probe 不可信的宿主上成立。EverEdit 点击移光标后不发任何
/// 通知，空闲上报滞后一拍，而它的 probe 在按键后立即给出正确位置；抢跑会让候选窗恒慢一拍，
/// 抢锁的组合起点还会让随后正确的 reshow 也救不回来。probe 可信时等那 25ms 即可。
#[test]
fn trusted_probe_host_waits_instead_of_using_the_idle_report() {
    let c = fast_coord(true); // 未配 stale_probe_guard ⇒ probe 可信
    c.caret_cache_is_idle_report
        .store(true, std::sync::atomic::Ordering::Relaxed);
    drive_first_frame(&c);
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "probe 可信的宿主应照常等待，让 probe 在兜底到期前刷新缓存"
    );
    assert!(
        !c.composition_start.lock().unwrap().2,
        "更不得把可能滞后的空闲上报锁成组合起点——锁错了 reshow 就再也救不回来"
    );
}

/// 兜底 timer 到期这条路同样要抢锁。它先置 `show_authorized`，`is_first_frame` 已为
/// false，判据若绑在 `is_first_frame` 上就整条漏掉——微信「焦点后第一个组合」走的正是
/// 这条（idle_anchor 逃生口被焦点判据挡住），表现为第三个字好了、第二个字仍挪一格。
#[test]
fn fallback_first_show_also_claims_the_composition_start() {
    let c = fast_coord(true);
    c.active_compat.lock().unwrap().stale_probe_guard = true;
    c.caret_cache_is_idle_report
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 焦点后第一个组合：立即显示的逃生口被挡住，只能走短兜底
    c.awaiting_first_authority_after_focus
        .store(true, std::sync::atomic::Ordering::Relaxed);
    drive_first_frame(&c);
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "焦点后第一个组合应进入等待，本用例才测得到兜底那条路"
    );
    // token 先 let 绑定再传：写成 fire(*c...lock().unwrap()) 会让临时 MutexGuard
    // 活到语句结束，而 fire 内部要再锁同一个 Mutex ⇒ 自死锁。
    let token = *c.pending_first_show_token.lock().unwrap();
    c.fire_pending_first_show(token);
    assert_eq!(
        *c.composition_start.lock().unwrap(),
        (100, 200, true),
        "兜底首显同样用的是组合前空闲上报，同样该把它锁成组合起点"
    );
}

/// 反向：其余首显路径不得抢锁组合起点。兜底用的是旧缓存、instant 用的是上一轮遗留
/// 坐标，本身就可能是错的；锁进组合起点会让后续 reshow 也救不回来（本组合内不再更新）。
#[test]
fn other_first_show_paths_do_not_claim_the_composition_start() {
    let c = fast_coord(true); // is_idle_report 保持 false ⇒ 非 idle_anchor
    set_mode(&c, wind_config::app_compat::FirstShowMode::Instant);
    drive_first_frame(&c);
    assert!(
        !c.composition_start.lock().unwrap().2,
        "instant 逃生口用的坐标不够格当组合起点"
    );
}

/// 长兜底到期后不再续：用旧坐标首显仍优于候选窗一直不出现。
#[test]
fn long_fallback_shows_when_it_finally_expires() {
    let c = fast_coord(false);
    c.arm_pending_first_show();
    // token 必须先 let 绑定再传：写成 `fire(*c...lock().unwrap())` 会让临时 MutexGuard
    // 活到整个语句结束（Rust 临时值生命周期），而 fire 内部要再锁同一个 Mutex ⇒ 自死锁。
    let token = *c.pending_first_show_token.lock().unwrap();
    c.fire_pending_first_show(token);
    assert!(
        !*c.pending_first_show.lock().unwrap(),
        "长兜底到期必须放行，否则候选窗永不出现"
    );
    assert!(
        c.first_show_was_provisional
            .load(std::sync::atomic::Ordering::Relaxed),
        "用的是旧坐标，须记为非权威以享放宽容差"
    );
}

/// wait/instant 档一字不变：它们的长兜底由 caret_pending 握手负责，信任门若也插一脚，
/// 两条路径叠加会让 wait 最坏等到 1200ms。
#[test]
fn trust_gate_does_not_touch_wait_mode() {
    let c = fast_coord(false);
    set_mode(&c, wind_config::app_compat::FirstShowMode::Wait);
    c.arm_pending_first_show();
    assert!(
        !c.first_show_extended
            .load(std::sync::atomic::Ordering::Relaxed),
        "wait 档不受信任门影响"
    );
    assert_eq!(c.first_show_fallback_ms(), 150, "wait 档保持既有 150ms");
}

/// 闸门日志打印的超时必须等于实际 arm 的超时。此前闸门直接打 `first_show_fallback_ms()`，
/// 信任门命中时会「日志说 25ms、实际等 600ms」——排查首显延迟时这种分叉最坑人。
#[test]
fn logged_timeout_matches_actual_arm() {
    let c = fast_coord(false);
    assert_eq!(
        c.planned_first_show_timeout_ms(),
        FIRST_SHOW_LONG_FALLBACK_MS,
        "信任门命中时闸门日志须报长兜底"
    );
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        c.planned_first_show_timeout_ms(),
        c.rt().config.ui.candidate.fast_first_show_fallback_ms,
        "未命中时须报 fast 短兜底"
    );
}

/// 上屏 / 组合结束必须复位长等待标记——「这一轮已在长等待中」是每轮独立的事实，
/// 跨轮残留会让 `already_waiting` 的判据失去意义（当前因 `pending` 同时被复位而侥幸
/// 不出错，但那是巧合不是设计）。
#[test]
fn reset_first_show_clears_extended_flag() {
    let c = fast_coord(false);
    c.arm_pending_first_show();
    assert!(
        c.first_show_extended
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    c.reset_first_show();
    assert!(
        !c.first_show_extended
            .load(std::sync::atomic::Ordering::Relaxed),
        "组合结束必须复位，否则下一轮 arm 被永久跳过"
    );
}

/// 焦点到达 = 换了 DocMgr，此刻 state 里那份是焦点事件随包携带的坐标（宿主多半还没
/// reflow，Excel 甚至还没建好编辑上下文），不够格让 fast 跳过等待。
#[test]
fn focus_gained_invalidates_caret_cache() {
    let c = coord();
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    c.handle_focus_gained(&FocusData {
        x: 100,
        y: 300,
        height: 30,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: 0,
        input_scope_mask: 0,
        disabled: false,
        reason: 0,
        caret_source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        bundle_id: String::new(),
        window_class: String::new(),
        window_title: String::new(),
    });
    assert!(
        !c.caret_cache_verified
            .load(std::sync::atomic::Ordering::Relaxed),
        "焦点到达必须作废坐标缓存的可信标记"
    );
}

/// 用户在同一 DocMgr 内点到别处：不发 focus_gained，宿主也只在有 composition 时才回送
/// caret_update，所以缓存里仍是上次输入的位置——必须作废。
#[test]
fn user_caret_move_invalidates_cache_but_self_commit_echo_does_not() {
    let c = coord();
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    c.handle_selection_changed(0);
    assert!(
        !c.caret_cache_verified
            .load(std::sync::atomic::Ordering::Relaxed),
        "用户移动光标必须作废坐标缓存"
    );

    // 反向对照：自提交回声（上屏后宿主插入文本导致的光标移动）不得作废，否则每上屏
    // 一个字就作废一次，fast 档在连打时完全退化。
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    *c.last_self_commit.lock().unwrap() = Some(std::time::Instant::now());
    c.handle_selection_changed(0);
    assert!(
        c.caret_cache_verified
            .load(std::sync::atomic::Ordering::Relaxed),
        "自提交回声不得作废坐标缓存"
    );
}

/// 兜底首显用的是按键前的旧坐标，必须记为「非权威」，否则随后到达的权威坐标会被 3px
/// 常规容差判成要校正而跳一下——兜底路径正是抖动最容易被看见的地方。
#[test]
fn fallback_first_show_marks_provisional() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 25;
    }
    *c.pending_first_show.lock().unwrap() = true;
    let token = *c.pending_first_show_token.lock().unwrap();
    c.fire_pending_first_show(token);
    assert!(
        c.first_show_was_provisional
            .load(std::sync::atomic::Ordering::Relaxed),
        "兜底显示后应置位 provisional 以享放宽容差"
    );
}

/// 上屏后紧接着开的新组合（Shift+字母上屏进临英）：兜底首显用的旧坐标确定陈旧，
/// 不得记 provisional，否则随后一个字宽的前移会被放宽容差吞掉、候选窗留在原位。
#[test]
fn first_show_after_commit_is_not_provisional() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 25;
    }
    *c.pending_first_show.lock().unwrap() = true;
    c.first_show_after_commit
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let token = *c.pending_first_show_token.lock().unwrap();
    c.fire_pending_first_show(token);
    assert!(
        !c.first_show_was_provisional
            .load(std::sync::atomic::Ordering::Relaxed),
        "上屏后的兜底首显不应享放宽容差"
    );
    assert!(
        !c.first_show_after_commit
            .load(std::sync::atomic::Ordering::Relaxed),
        "标记应被兜底首显消费"
    );
}

/// 首显用过非权威坐标后，随后到达的权威坐标若只差不到 80% 行高，不得 reshow。
/// 抖动的观感来自校正动作本身——这条钉住「小偏差不动」的行为。
#[test]
fn provisional_first_show_tolerates_small_correction() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 25;
    }
    *c.candidate_shown.lock().unwrap() = true;
    c.first_show_was_provisional
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 偏差 15px < 25 × 0.8 = 20px ⇒ 应被吞掉
    c.handle_caret_update(&CaretData {
        x: 115,
        y: 200,
        height: 25,
        composition_start_x: 115,
        composition_start_y: 200,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    assert_eq!(
        c.last_valid_caret.lock().unwrap().0,
        0,
        "小于 80% 行高的偏差不应触发 reshow（未走到 notify_ui_update）"
    );
}

/// 换行那种大偏差必须照常校正——容差放宽不能把真正的错位也一起吞掉。
#[test]
fn provisional_first_show_still_corrects_large_jump() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 900;
        st.caret_y = 200;
        st.caret_height = 25;
    }
    *c.candidate_shown.lock().unwrap() = true;
    c.first_show_was_provisional
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 换行：x 回行首、y 下移两行（实测 EverEdit 曾出现 dx=156 dy=194）
    c.handle_caret_update(&CaretData {
        x: 726,
        y: 250,
        height: 25,
        composition_start_x: 726,
        composition_start_y: 250,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    assert_eq!(
        c.last_valid_caret.lock().unwrap().0,
        726,
        "换行级偏差必须校正"
    );
}

/// 容差只作用于「首显用过非权威坐标」的那一次：常规光标更新仍按 3px 走，
/// 否则正常输入时的小幅移动会被误吞、候选窗跟不上光标。
#[test]
fn settle_tolerance_applies_only_after_provisional_first_show() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 100;
        st.caret_y = 200;
        st.caret_height = 25;
    }
    *c.candidate_shown.lock().unwrap() = true;
    // 未置位 first_show_was_provisional
    c.handle_caret_update(&CaretData {
        x: 115,
        y: 200,
        height: 25,
        composition_start_x: 115,
        composition_start_y: 200,
        source: wind_ipc::protocol::caret_source::TSF_SELECTION,
        composition_rect: None,
    });
    assert_eq!(
        c.last_valid_caret.lock().unwrap().0,
        115,
        "常规路径下 15px 偏移仍应 reshow"
    );
}

/// `PRE_REFLOW` 探测绝不作首显的位置来源，哪怕它满足全部原判据。
///
/// 它是组合刚启动时的异步取值，多数宿主内联执行拿到的是 reflow 前的坐标；拿它首显后，
/// 真权威坐标的小偏差又会被 settle 吞掉，错位就此固定。它只刷新缓存（连续快速上屏时它是
/// 唯一的位置来源），不做决策；可以当否决基准，见
/// `a_probe_equal_to_this_rounds_pre_reflow_frame_must_not_drive_first_show`。
/// 对照组同坐标只改 `source`。
#[test]
fn pre_reflow_probe_refreshes_cache_but_never_drives_first_show() {
    let arm = |source: i32| {
        let c = coord();
        set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "a".to_string();
            st.caret_x = 100;
            st.caret_y = 200;
            st.caret_height = 20;
        }
        // 基准与 probe 不同 ⇒ 判据 1 会认定「已 reflow」并采信
        *c.last_authoritative_caret.lock().unwrap() = (500, 300, true);
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        *c.pending_first_show.lock().unwrap() = true;
        let mut probe = probe_at(800, 600, 24);
        probe.source = source;
        c.handle_caret_probe(&probe);
        c
    };

    let c = arm(wind_ipc::protocol::caret_source::PRE_REFLOW);
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "PRE_REFLOW 是 reflow 前的坐标，绝不能拿它提前首显——那会把错位用 settle 容差固定下来"
    );
    assert_eq!(
        c.state.lock().unwrap().caret_x,
        800,
        "但坐标必须收进缓存：连续快速上屏时它是唯一的位置来源"
    );

    // 对照组：同一份坐标，普通来源 ⇒ 判据 1 照常采信并首显
    let c = arm(wind_ipc::protocol::caret_source::TSF_SELECTION);
    assert!(
        !*c.pending_first_show.lock().unwrap(),
        "对照组：普通 probe 满足判据 1 时应当提前首显（否则本测试证明不了差别来自 source）"
    );
}

/// 兜底首显要画在 pre_reflow 帧报的组合起点，不能画在它的 caret。
///
/// pre_reflow 的 compStart 是按键前的插入点，重排不挪它；那时 caret 已在它右边一格。
/// 有矩形时 settle 不回灌 `shown_anchor`（矩形是真值），首显若画在 caret，第 2 个字母
/// reshow 到组合起点时就会跳一下。
#[test]
fn fallback_first_show_uses_the_pre_reflow_composition_start() {
    let run = |cs_in_probe: (i32, i32)| {
        let c = fast_coord(true);
        {
            let mut st = c.state.lock().unwrap();
            st.caret_x = 1139;
            st.caret_y = 881;
            st.caret_height = 31;
        }
        drive_first_frame(&c);
        assert!(
            *c.pending_first_show.lock().unwrap(),
            "probe 可信的宿主首帧应等坐标，本用例测的是兜底那条路"
        );
        let mut pre = probe_at(1151, 881, 31);
        pre.composition_start_x = cs_in_probe.0;
        pre.composition_start_y = cs_in_probe.1;
        pre.source = wind_ipc::protocol::caret_source::PRE_REFLOW;
        c.handle_caret_probe(&pre);
        let token = *c.pending_first_show_token.lock().unwrap();
        c.fire_pending_first_show(token);
        let shown = *c.shown_anchor.lock().unwrap();
        // 随后两帧权威坐标：先是带矩形的那帧（被 settle 吸收），再是第 2 个字母
        let mut a = probe_at(1151, 881, 31);
        a.composition_start_x = 1139;
        a.composition_rect = Some((1139, 850, 1151, 881));
        c.handle_caret_update(&a);
        let mut b = probe_at(1159, 881, 31);
        b.composition_start_x = 1139;
        b.composition_rect = Some((1139, 850, 1159, 881));
        c.handle_caret_update(&b);
        (shown, *c.shown_anchor.lock().unwrap())
    };

    let (first, after_second_key) = run((1139, 881));
    assert_eq!(
        (first.0, first.1),
        (1139, 881),
        "兜底首显应取 pre_reflow 帧的组合起点，而不是已右移一格的 caret"
    );
    assert_eq!(
        (after_second_key.0, after_second_key.1),
        (1139, 881),
        "第 2 个字母之后候选窗不得再挪（A2-42 的症状）"
    );

    // 对照组：pre_reflow 帧不带组合起点时维持原行为（用缓存 caret），证明差别来自 compStart
    let (first, _) = run((0, 0));
    assert_eq!(
        (first.0, first.1),
        (1151, 881),
        "对照组：没有组合起点可用时兜底照旧用缓存 caret"
    );
}

/// probe 与本轮 `pre_reflow` 帧逐位相同 ⇒ 宿主还没重排 ⇒ 不得提前首显。
///
/// 终端行满回绕时，陈旧的行尾值与上一轮权威天然不等，判据 1 必然放行，候选窗随后横穿屏幕。
/// 判据 2 换了基准：同一轮里 DLL 已经声明过哪个坐标是重排前的。
/// 对照组同坐标、只改 `last_pre_reflow_probe` 有没有记过。
#[test]
fn a_probe_equal_to_this_rounds_pre_reflow_frame_must_not_drive_first_show() {
    // 靶机原始值
    const STALE: (i32, i32) = (1432, 1205); // 行尾，重排前
    const LAST_AUTH: (i32, i32) = (1401, 1205); // 上一轮权威，判据 1 拿它当基准
    const TRUE_POS: (i32, i32) = (369, 1205); // 换行后的真值

    let arm = |probe_at_xy: (i32, i32), record_pre_reflow: bool| {
        let c = coord();
        set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "q".to_string();
            st.caret_x = 100;
            st.caret_y = 1205;
            st.caret_height = 31;
        }
        *c.last_authoritative_caret.lock().unwrap() = (LAST_AUTH.0, LAST_AUTH.1, true);
        // 前置门：缓存未经当前插入点验证时，`first_show_needs_long_wait` 会在本判据之前
        // 就否决掉 probe 的显示决策（那是 Excel 那条信任门）。不置位的话三条断言比的是
        // 那道门，不是本判据——恒「不首显」，第一条假绿、后两条必挂。
        c.caret_cache_verified
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if record_pre_reflow {
            // 走真实通路记录，而不是直接写字段——这样「pre_reflow 分支忘了记」
            // 这种回归也会被本测试抓住。
            let mut pre = probe_at(STALE.0, STALE.1, 31);
            pre.source = wind_ipc::protocol::caret_source::PRE_REFLOW;
            c.handle_caret_probe(&pre);
        }
        *c.pending_first_show.lock().unwrap() = true;
        c.handle_caret_probe(&probe_at(probe_at_xy.0, probe_at_xy.1, 31));
        c
    };

    assert!(
        *arm(STALE, true).pending_first_show.lock().unwrap(),
        "probe 与本轮 pre_reflow 帧相同 ⇒ 宿主还停在重排前，不得提前首显（否则候选窗画在\
             行尾，67ms 后横穿屏幕跳 1044px）"
    );
    assert!(
        !*arm(TRUE_POS, true).pending_first_show.lock().unwrap(),
        "重排后的真值与 pre_reflow 帧不同 ⇒ 照常提前首显（判据只挡「没动过」，不挡换行本身）"
    );
    assert!(
        !*arm(STALE, false).pending_first_show.lock().unwrap(),
        "★ 对照组：同一份陈旧坐标，没记过 pre_reflow 基准时判据 2 不成立、判据 1 照旧放行\
             ——差别确实来自这个基准"
    );
}

/// 否决基准必须挡在连打快路径之前——行满回绕正是连打到行尾才发生的事件，而快路径
/// 不比对任何基准就采信首条采样。同 `stale_probe_guard` 的位置（见
/// `stale_probe_rejected_even_on_fast_typing_path`）。
#[test]
fn the_pre_reflow_baseline_rejects_even_on_the_fast_typing_path() {
    const STALE: (i32, i32) = (1432, 1205);
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "q".to_string();
        st.caret_x = 100;
        st.caret_y = 1205;
        st.caret_height = 31;
    }
    *c.last_authoritative_caret.lock().unwrap() = (1401, 1205, true);
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 连打：按键间隔落进 fast_typing_window_ms（出厂 100ms）⇒ 快路径武装
    *c.last_key_interval_ms.lock().unwrap() = Some(60);
    let mut pre = probe_at(STALE.0, STALE.1, 31);
    pre.source = wind_ipc::protocol::caret_source::PRE_REFLOW;
    c.handle_caret_probe(&pre);
    *c.pending_first_show.lock().unwrap() = true;
    c.handle_caret_probe(&probe_at(STALE.0, STALE.1, 31));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "连打时同样要挡：判据排在快路径下游的话，行满回绕（只在连打到行尾发生）永远绕过它"
    );
}

/// 基准要记的是 pre_reflow 的本帧原值，与 `absorb_probe_coords` 收没收它无关：退化帧与
/// `stale_probe_guard` 宿主的 probe 都不进缓存，却恰恰最需要否决基准。
#[test]
fn the_pre_reflow_baseline_is_recorded_even_when_the_frame_is_not_absorbed() {
    // stale_probe_guard 宿主：absorb_probe_coords 直接返回 false
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    c.active_compat.lock().unwrap().stale_probe_guard = true;
    let mut pre = probe_at(1432, 1205, 31);
    pre.source = wind_ipc::protocol::caret_source::PRE_REFLOW;
    c.handle_caret_probe(&pre);
    assert_eq!(
        *c.last_pre_reflow_probe.lock().unwrap(),
        (1432, 1205, true),
        "probe 不可信的宿主同样要记基准——它们才是最需要这条否决的"
    );

    // 退化帧：h<=0 同样不进缓存
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    let mut pre = probe_at(1432, 1205, 0);
    pre.source = wind_ipc::protocol::caret_source::PRE_REFLOW;
    c.handle_caret_probe(&pre);
    assert_eq!(
        *c.last_pre_reflow_probe.lock().unwrap(),
        (1432, 1205, true),
        "退化帧的坐标不配进缓存，但「宿主还没重排」这个事实照样成立"
    );
}

/// 组合结束必须清掉 pre_reflow 基准，否则下一轮拿的是上一轮的重排前坐标。
#[test]
fn the_pre_reflow_baseline_is_cleared_when_the_composition_ends() {
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    let mut pre = probe_at(1432, 1205, 31);
    pre.source = wind_ipc::protocol::caret_source::PRE_REFLOW;
    c.handle_caret_probe(&pre);
    assert!(
        c.last_pre_reflow_probe.lock().unwrap().2,
        "前提：pre_reflow 帧已记录（不成立的话下面证明不了任何事）"
    );
    c.reset_first_show();
    assert!(
        !c.last_pre_reflow_probe.lock().unwrap().2,
        "组合结束必须清位——它描述的是「本轮重排前在哪」，跨轮沿用只会误拦"
    );
}

/// 候选窗已显示时到达的 probe 不做显示决策，但坐标要收下。
///
/// 连续快速输入时宿主的 OnLayoutChange 会被 debounce 一直压住，整段没有权威坐标，probe 是
/// 唯一的位置信息；丢掉它，每轮新组合的兜底都拿旧缓存首显，候选窗钉在原地。
#[test]
fn idle_probe_coords_are_absorbed_but_two_kinds_are_not() {
    let fresh = |caret_x: i32| {
        let c = coord();
        set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
        {
            let mut st = c.state.lock().unwrap();
            st.input_buffer = "d".to_string();
            st.caret_x = caret_x; // 几百毫秒前的旧坐标
            st.caret_y = 589;
            st.caret_height = 55;
        }
        // 候选窗已显示、不在等首显——正是被丢弃的那条路
        *c.pending_first_show.lock().unwrap() = false;
        c
    };

    let c = fresh(992);
    c.handle_caret_probe(&probe_at(1760, 589, 55));
    assert_eq!(
        c.state.lock().unwrap().caret_x,
        1760,
        "已首显期间的 probe 坐标必须收进缓存，否则下一轮组合的兜底还用几百毫秒前的旧值"
    );

    // 退化帧：宿主尚未 reflow 的空 rect，收了会污染缓存
    let c = fresh(992);
    c.handle_caret_probe(&probe_at(1760, 589, 0));
    assert_eq!(
        c.state.lock().unwrap().caret_x,
        992,
        "退化帧（h<=0）不得收入缓存"
    );

    // 配了 stale_probe_guard 的宿主：probe 本就可能停在上一次组合的位置（微信），
    // 收进缓存等于把陈旧值扩散到兜底路径
    let c = fresh(992);
    c.active_compat.lock().unwrap().stale_probe_guard = true;
    c.handle_caret_probe(&probe_at(1760, 589, 55));
    assert_eq!(
        c.state.lock().unwrap().caret_x,
        992,
        "该宿主的 probe 不可信，不得收入缓存"
    );
}

#[test]
fn probe_ignored_unless_fast_mode() {
    // `wait` 档的底线：退回该档的宿主必须拿到「等 reflow 权威坐标」的原行为，probe 一条都不许
    // 消费。显式设档，不依赖默认档恰好是某值。
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Wait);
    assert!(
        still_waiting_after_probe(&c, probe_at(800, 600, 24)),
        "非 fast 档时 probe 必须被完全忽略"
    );
}

/// 首显有多条通路，信任门必须每条都接。本条守住 `caret_probe` 这条：它绕过闸门直接首显。
///
/// 坐标缓存失效时 probe 的两条判据都失去判断力：判据 1 的基准（上一轮权威）属于另一个
/// 单元格，恒不相等；判据 2 的按键间隔跨了焦点，同样说明不了当前帧可信。
#[test]
fn probe_defers_to_long_wait_when_cache_unverified() {
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    };
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
    }
    // 有基准、坐标也「变了」——判据 1 本会采信；但缓存未验证，基准不可比。
    *c.last_authoritative_caret.lock().unwrap() = (500, 300, true);
    c.caret_cache_verified
        .store(false, std::sync::atomic::Ordering::Relaxed);
    *c.pending_first_show.lock().unwrap() = true;
    c.handle_caret_probe(&probe_at(800, 600, 24));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "坐标缓存未验证时 probe 不得提前首显，须让位给长兜底等真坐标"
    );

    // 连打快路径（判据 2）同样要被拦住，否则换个入口照样绕过去。
    *c.last_key_interval_ms.lock().unwrap() = Some(60);
    c.handle_caret_probe(&probe_at(500, 300, 24));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "连打快路径也必须过信任门——只堵判据 1 等于没堵"
    );
}

/// 「坐标仍等于上一轮权威」那一帧被拒时的缓存处理，两个用例共用的现场：缓存里装着
/// `cache_source` 来源的 (0,54)，上一轮权威坐标是 (473,217)，正等首显。
fn coord_waiting_with_cache(cache_source: i32) -> Arc<Coordinator> {
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 0;
        st.caret_y = 54;
        st.caret_height = 20;
        st.caret_source = cache_source;
    }
    *c.last_authoritative_caret.lock().unwrap() = (473, 217, true);
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    *c.pending_first_show.lock().unwrap() = true;
    c
}

fn composition_probe_at(x: i32, y: i32, height: i32) -> CaretData {
    CaretData {
        source: wind_ipc::protocol::caret_source::TSF_COMPOSITION,
        ..probe_at(x, y, height)
    }
}

/// Java 被动客户端（JTable 未进入编辑）：按键时 DLL 退到 GUI caret（窗口客户区原点），
/// 组合开始后 TSF 给的编辑框位置等于上一轮权威而被判「尚未 reflow」。TSF 域的旧值比跨窗口
/// 的 GUI caret 可信，这一帧要收进缓存给兜底用，否则候选窗先闪到屏幕左边；但仍不提前首显。
#[test]
fn probe_equal_to_last_auth_replaces_non_tsf_cache_for_fallback() {
    let c = coord_waiting_with_cache(wind_ipc::protocol::caret_source::GUI_CARET);
    c.handle_caret_probe(&composition_probe_at(473, 217, 28));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "与上一轮权威相同的 probe 仍须继续等，不得提前首显"
    );
    let st = c.state.lock().unwrap();
    assert_eq!(
        (st.caret_x, st.caret_y, st.caret_height),
        (473, 217, 28),
        "兜底该用 TSF 域的 probe，而不是 GUI caret (0,54)"
    );
}

/// 对照：缓存本就是 TSF 域（宿主组合前上报的空闲插入点）时不得被替换——等待分支存在的
/// 理由正是「probe 可能停在上一轮的位置」，此时缓存才是对的那个。
#[test]
fn probe_equal_to_last_auth_keeps_tsf_cache() {
    let c = coord_waiting_with_cache(wind_ipc::protocol::caret_source::TSF_SELECTION);
    c.handle_caret_probe(&composition_probe_at(473, 217, 28));
    assert!(*c.pending_first_show.lock().unwrap());
    let st = c.state.lock().unwrap();
    assert_eq!(
        (st.caret_x, st.caret_y),
        (0, 54),
        "TSF 域的缓存不得被与上一轮权威相同的 probe 覆盖"
    );
}

/// 对照：probe 本身不是 TSF 域（GUI caret 等）时不收——换来的只是另一份同样靠不住的坐标。
#[test]
fn probe_equal_to_last_auth_non_tsf_probe_not_absorbed() {
    let c = coord_waiting_with_cache(wind_ipc::protocol::caret_source::GUI_CARET);
    let p = CaretData {
        source: wind_ipc::protocol::caret_source::GUI_CARET,
        ..probe_at(473, 217, 28)
    };
    c.handle_caret_probe(&p);
    assert!(*c.pending_first_show.lock().unwrap());
    let st = c.state.lock().unwrap();
    assert_eq!((st.caret_x, st.caret_y), (0, 54));
}

/// 把「组合前的空闲上报」摆好：模拟用户上屏后打空格移动了光标，宿主在按键前上报了
/// 真实插入点 (745,1007)，而上一轮组合的权威坐标停在 (601,988)。
fn coord_with_idle_report() -> Arc<Coordinator> {
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
        st.caret_x = 745; // 宿主主动上报的当前插入点，缓存里就是它
        st.caret_y = 1007;
        st.caret_height = 20;
    }
    *c.last_authoritative_caret.lock().unwrap() = (601, 988, true);
    // 缓存本身是可信的（信任门不该命中）——本用例要测的正是「缓存可信、但 probe 陈旧」
    c.caret_cache_verified
        .store(true, std::sync::atomic::Ordering::Relaxed);
    c.caret_cache_is_idle_report
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 本判据逐宿主开启，测微信这类宿主时要显式打开（出厂只给 Weixin.exe 配）
    c.active_compat.lock().unwrap().stale_probe_guard = true;
    *c.pending_first_show.lock().unwrap() = true;
    c
}

/// 守住 per-app 语义：没配 `stale_probe_guard` 的宿主必须走原有判据，一字不差。
/// WindTerm 这类宿主的 probe 恰是重排后的新位置，全局开启会拦错。
#[test]
fn stale_probe_guard_is_per_app_and_off_by_default() {
    let c = coord_with_idle_report();
    c.active_compat.lock().unwrap().stale_probe_guard = false; // 未配规则的普通宿主
    // 同一份「陈旧」输入：开着 guard 时会被拦（见 stale_probe_rejected_…），关着则不该拦
    c.handle_caret_probe(&probe_at(609, 989, 1));
    assert!(
        !*c.pending_first_show.lock().unwrap(),
        "未配 stale_probe_guard 的宿主必须走原有判据 1，不受本 guard 干预"
    );
}

/// 微信在 composition 期间报的 rect 仍是上一次组合的位置，而按键前的空闲上报才是真实插入点。
/// 判据 1 对此没有判断力：正确值与陈旧值都 ≠ 基准，它问错了问题。
#[test]
fn stale_probe_rejected_regardless_of_position_relation() {
    // 四组都是真机现场的坐标，位置关系互相矛盾——正是它们逐个推翻了先前
    // 四版位置判据。现在判据不看位置，四组必须一律被拒。
    let cases: [(&str, CaretData); 4] = [
        // ① 空格移光标：陈旧值在缓存左边 136px（同行左移）
        ("空格移光标·左移", probe_at(609, 989, 1)),
        // ② 换行：陈旧值留在上一行末尾，x 反而更大、y 更小（像"前进"）
        ("换行·右上方", probe_at(915, 988, 1)),
        // ③ 退格：光标左移，陈旧值停在右边 390px（同样像"前进"）
        ("退格·右移", probe_at(2016, 1033, 1)),
        // ④ 与缓存几乎重合：先前版本会放行，但对本宿主而言它同样不可信
        ("几乎重合", probe_at(748, 1009, 20)),
    ];
    for (name, probe) in cases {
        let c = coord_with_idle_report();
        c.handle_caret_probe(&probe);
        assert!(
            *c.pending_first_show.lock().unwrap(),
            "[{name}] 该宿主的 composition rect 恒陈旧，任何位置关系都不得采信——\
                 判据一旦回到「看位置」，这四组里必有一组绕过去"
        );
    }
}

/// 组合期间的普通 caret_update 与 probe 同源：该宿主的组合坐标恒不可信，
/// 首显用了空闲坐标后，这一帧不得把缓存换成陈旧值、也不得重锁候选窗。
#[test]
fn stale_probe_guard_host_ignores_caret_update_during_composition() {
    let c = coord_with_idle_report();
    c.handle_caret_update(&probe_at(473, 224, 35));
    let st = c.state.lock().unwrap();
    assert_eq!(
        (st.caret_x, st.caret_y, st.caret_height),
        (745, 1007, 20),
        "组合期的 caret_update 不得覆盖组合前的空闲坐标"
    );
    assert!(
        c.caret_cache_is_idle_report
            .load(std::sync::atomic::Ordering::Relaxed),
        "空闲上报标记要保留，后续帧才继续被挡"
    );
}

/// 没配守卫的宿主：组合期的 caret_update 照常采信（行为一字不差）。
#[test]
fn caret_update_during_composition_is_trusted_without_the_guard() {
    let c = coord_with_idle_report();
    c.active_compat.lock().unwrap().stale_probe_guard = false;
    c.handle_caret_update(&probe_at(473, 224, 35));
    let st = c.state.lock().unwrap();
    assert_eq!((st.caret_x, st.caret_y), (473, 224));
}

/// 判据的前提是手上有宿主刚报的空闲坐标。没有它时缓存本身也是旧的，
/// 没有更好的选择，probe 应照常参与——否则该宿主会退化成「永远等权威坐标」。
#[test]
fn stale_probe_guard_inactive_without_fresh_idle_report() {
    let c = coord_with_idle_report();
    c.caret_cache_is_idle_report
        .store(false, std::sync::atomic::Ordering::Relaxed);
    c.handle_caret_probe(&probe_at(609, 989, 1));
    assert!(
        !*c.pending_first_show.lock().unwrap(),
        "没有组合前空闲上报时本判据不生效，probe 照常走判据 1"
    );
}

/// 首显有多条通路，否决类判据必须每条都接。连打快路径不比对任何基准就采信，
/// 新判据若排在它之后，只要按键间隔落进 `fast_typing_window_ms` 就被整条绕过。
#[test]
fn stale_probe_rejected_even_on_fast_typing_path() {
    let c = coord_with_idle_report();
    *c.last_key_interval_ms.lock().unwrap() = Some(60); // 落进连打快路径窗口
    c.handle_caret_probe(&probe_at(609, 989, 1));
    assert!(
        *c.pending_first_show.lock().unwrap(),
        "连打快路径也必须过本判据——只堵判据 1 等于没堵"
    );
}

/// 置位/清位的两条通路：无组合那一帧记账，组合期间的权威坐标注销。
/// 清位漏了会让标记一直挂着，把本次组合自己的权威坐标也当成「组合前的空闲上报」，
/// 后续 probe 全被误拒 ⇒ fast 档静默退化成兜底档。
#[test]
fn idle_report_flag_set_by_idle_caret_and_cleared_by_authoritative() {
    let c = coord();
    set_mode(&c, wind_config::app_compat::FirstShowMode::Fast);
    // 无组合（缓冲空、无候选）⇒ 这一帧只更新缓存，并记下「组合前空闲上报」
    c.handle_caret_update(&probe_at(745, 1007, 20));
    assert!(
        c.caret_cache_is_idle_report
            .load(std::sync::atomic::Ordering::Relaxed),
        "无组合时到达的 caret 是宿主对当前插入点的直接测量，必须记账"
    );

    // 组合期间的权威坐标到达 ⇒ 缓存换成本次组合的位置，标记的前提消失
    {
        let mut st = c.state.lock().unwrap();
        st.input_buffer = "a".to_string();
    }
    c.handle_caret_update(&probe_at(760, 1007, 20));
    assert!(
        !c.caret_cache_is_idle_report
            .load(std::sync::atomic::Ordering::Relaxed),
        "组合期间的权威坐标必须注销该标记，否则 probe 会被长期误拒"
    );
}

#[test]
fn probe_releases_first_show_when_caret_moved() {
    // 坐标已不同于上一轮权威 ⇒ 宿主已 reflow ⇒ 采信并提前首显。
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    };
    assert!(
        !still_waiting_after_probe(&c, probe_at(800, 600, 24)),
        "坐标已变应提前首显"
    );
}

/// 连打快路径必须由相邻按键间隔驱动，不能由「距上次按键多久」驱动——后者恒成立（试探坐标
/// 总在按键后 10ms 内到达）。慢速手打时即使坐标等于上一轮权威也必须继续等待。
#[test]
fn slow_typing_does_not_take_fast_path() {
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    };
    // 慢速手打：相邻按键间隔 800ms，远超默认 100ms 窗口
    *c.last_key_interval_ms.lock().unwrap() = Some(800);
    assert!(
        still_waiting_after_probe(&c, probe_at(500, 300, 24)),
        "慢速输入下不得走连打快路径，须回落到「≠上一轮权威」判据"
    );
}

/// 连打（间隔在窗口内）时直接采信首条试探坐标——即使它等于上一轮权威坐标。
/// 依据：连打时光标沿同一行顺序前移、不重排，跟手比精确更重要。
#[test]
fn fast_typing_takes_fast_path_even_when_caret_unchanged() {
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    };
    *c.last_key_interval_ms.lock().unwrap() = Some(60); // 与真机脚本同节奏
    assert!(
        !still_waiting_after_probe(&c, probe_at(500, 300, 24)),
        "连打间隔内应走快路径立即首显"
    );
}

#[test]
fn probe_keeps_waiting_when_caret_equals_previous() {
    // 与上一轮权威坐标相同 ⇒ 宿主尚未 reflow（实测 WPS 前两次采样即如此）⇒ 继续等。
    // 采信它就会把候选窗定在上一轮的位置，正是要避免的抖动。
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    };
    assert!(
        still_waiting_after_probe(&c, probe_at(500, 300, 24)),
        "坐标等于上一轮权威时必须继续等待"
    );
}

#[test]
fn probe_rejects_degenerate_rect() {
    // 退化 rect（h<=0）：实测 WPS 采到过 top==bottom 的样本，其 x 与真实位置差 1687px。
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    };
    assert!(
        still_waiting_after_probe(&c, probe_at(9999, 8888, 0)),
        "退化 rect 不得采信"
    );
}

#[test]
fn default_host_waits_for_authoritative_caret() {
    // 对照组：无 compat 规则、坐标未就绪 → 保持原行为，等 reflow 权威坐标。
    // 这条也是另外两个测试的有效性保证：若闸门被改成恒放行，此测试会挂。
    let c = coord();
    assert!(
        armed_after_first_frame(&c),
        "默认宿主首帧应 arm 等待权威坐标"
    );
}

#[test]
fn instant_mode_bypasses_first_show_wait() {
    // 逃生口②：compat.toml 标记「光标稳定」的宿主直接首显。连打场景只有这一项能生效。
    let c = coord();
    *c.active_compat.lock().unwrap() = ActiveCompat {
        pid: 1234,
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Instant),
        ..Default::default()
    };
    assert!(
        !armed_after_first_frame(&c),
        "instant 档应立即首显，不得 arm 等待"
    );
}

#[test]
fn ready_coords_bypass_first_show_wait() {
    // 逃生口③：已有过有效 caret 且本轮组合起点已锁定 ⇒ 没有漂移可等。
    // 对应 Go 的 `!caretValid || !compositionStartValid` 取反。
    let c = coord();
    *c.last_valid_caret.lock().unwrap() = (100, 200, 20);
    *c.composition_start.lock().unwrap() = (100, 200, true);
    assert!(!armed_after_first_frame(&c), "坐标已就绪时不应再等 reflow");
}

#[test]
fn ready_coords_requires_both_caret_and_composition_start() {
    // 逃生口③的两个分量必须同时成立：只有 caret 有效、组合起点未锁定时仍须等待
    // ——组合起点未锁定正说明本轮 composition 的 reflow 坐标还没到。
    let c = coord();
    *c.last_valid_caret.lock().unwrap() = (100, 200, 20);
    // composition_start 保持 (0,0,false)
    assert!(
        armed_after_first_frame(&c),
        "仅 caret 有效、组合起点未锁定时应继续等待"
    );
}

#[test]
fn no_rule_keeps_bottom_coordinates() {
    let c = coord();
    // 未命中规则（默认 (0,false)）：坐标保持原样，不做 top 变换。
    c.handle_caret_update(&caret(200, 20));
    let s = c.state.lock().unwrap();
    assert_eq!(s.caret_y, 200);
    assert_eq!(s.caret_height, 20);
}

/// 服务重启时宿主早已在前台，管道重连只续发 `caret_update`、没有新的 `FOCUS_GAINED`；
/// `handle_client_connected` 须在确认该 pid 是前台窗口时提前解析 per-app 规则字段，
/// 否则 `caret_offset_*` 等整个会话停在默认值。
#[test]
fn apply_connected_pid_compat_loads_rule_when_pid_is_foreground() {
    let c = coord();
    let pid = 8848u32;
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "alacritty.exe".to_string());
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "alacritty.exe".into(),
        caret_offset_y: 12,
        composition_start_pair_guard: Some(true),
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

    c.apply_connected_pid_compat(pid, pid);

    assert_eq!(
        c.active_compat.lock().unwrap().caret_offset_y,
        12,
        "该 pid 确认在前台时，连接建立即应解析出它的 per-app 规则字段"
    );
    assert!(
        c.active_compat.lock().unwrap().composition_start_pair_guard,
        "连接恢复路径也必须刷新 composition_start_pair_guard"
    );
}

/// `ignore_host_ime_switch` 的判据逐一钉死，开和关两个方向各走一遍。
///
/// Ctrl 按住放行是唯一的逃生口：系统热键 Ctrl+Space 与宿主写 compartment 走同一条通路、
/// 载荷相同，不放行的话开了这条规则的应用里 Ctrl+Space 会彻底失灵。
#[test]
fn ignore_host_ime_switch_gates() {
    use wind_ipc::protocol::ModeSwitchSource as Src;
    let c = coord();
    let pid = 96032u32;
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "x60_toolbox.exe".to_string());
    c.active_compat.lock().unwrap().pid = pid;
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "x60_toolbox.exe".into(),
        ignore_host_ime_switch: Some(true),
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

    // 宿主写 compartment、无伴随按键 ⇒ 拦。判据不看方向：开和关两个方向都拦
    // （关：点按钮变英文；开：SSMS 焦点切换把英文态切成中文），方向由
    // tests/ignore_host_ime_switch.rs 经 handle_system_mode_switch 各走一遍。
    assert!(c.host_ime_switch_ignored(Src::CompartmentOpenClose, false));
    assert!(c.host_ime_switch_ignored(Src::CompartmentConversion, false));
    // Ctrl 按住 ⇒ 用户在按 Ctrl+Space，必须放行。
    assert!(!c.host_ime_switch_ignored(Src::CompartmentOpenClose, true));
    assert!(!c.host_ime_switch_ignored(Src::CompartmentConversion, true));
    // 我们自己发起的两条路径永远照办，否则按键兜底与菜单会失灵。
    assert!(!c.host_ime_switch_ignored(Src::CtrlSpaceKey, false));
    assert!(!c.host_ime_switch_ignored(Src::Menu, false));

    // 换成没配规则的应用、全局出厂关 ⇒ 一律照办。
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "notepad.exe".to_string());
    assert!(!c.host_ime_switch_ignored(Src::CompartmentOpenClose, false));
}

/// 生效值 = 按应用 `Some(v)` 优先，`None`（含没有规则）取全局 `input.ignore_host_ime_switch`。
#[test]
fn ignore_host_ime_switch_per_app_overrides_global() {
    use wind_ipc::protocol::ModeSwitchSource as Src;
    let mut cfg = Config::default();
    assert!(!cfg.input.ignore_host_ime_switch, "出厂关");
    cfg.input.ignore_host_ime_switch = true;
    let c = Coordinator::new_headless(cfg, None);
    let pid = 4242u32;
    c.active_compat.lock().unwrap().pid = pid;
    let rules = vec![
        wind_config::app_compat::AppCompatRule {
            process: "adopt.exe".into(),
            ignore_host_ime_switch: Some(false),
            ..Default::default()
        },
        wind_config::app_compat::AppCompatRule {
            process: "follow.exe".into(),
            auto_pair: Some(false),
            ..Default::default()
        },
    ];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);
    let at = |name: &str| {
        c.pid_names.lock().unwrap().insert(pid, name.to_string());
    };

    at("follow.exe");
    assert!(
        c.host_ime_switch_ignored(Src::CompartmentOpenClose, false),
        "规则没写这一项 ⇒ 跟随全局（开）"
    );
    assert!(
        !c.host_ime_switch_ignored(Src::CompartmentOpenClose, true),
        "全局开着也照样放行 Ctrl+Space"
    );
    assert!(!c.host_ime_switch_ignored(Src::Menu, false));
    at("notepad.exe");
    assert!(
        c.host_ime_switch_ignored(Src::CompartmentOpenClose, false),
        "没有规则 ⇒ 跟随全局（开）"
    );
    at("adopt.exe");
    assert!(
        !c.host_ime_switch_ignored(Src::CompartmentOpenClose, false),
        "按应用显式采纳压过全局"
    );
}

/// 拒绝之后必须再异步推一次状态（`CMD_STATE_PUSH`），不能只靠同步回包——理由见
/// `handle_system_mode_switch` 那段 ：只回包时 compartment 会停在宿主写的值上，
/// Ctrl+Space 变哑。两个方向各验一次。
#[test]
fn ignore_host_ime_switch_rejection_pushes_state() {
    use wind_bridge::handler::MessageHandler;
    use wind_ipc::protocol::ModeSwitchSource as Src;
    let pushed_state = |frames: &[Vec<u8>]| {
        frames.iter().any(|f| {
            wind_ipc::codec::decode_header(f)
                .is_ok_and(|h| h.command == wind_ipc::protocol::CMD_STATE_PUSH)
        })
    };
    for start_chinese in [true, false] {
        let mut cfg = Config::default();
        cfg.input.default.chinese_mode = start_chinese;
        cfg.input.ignore_host_ime_switch = true;
        let c = Coordinator::new_headless(cfg, None);
        assert_eq!(c.is_chinese_mode(), start_chinese, "前提：起步模式");
        let cap = c.push_server.attach_capture_client(7);
        c.handle_system_mode_switch(!start_chinese, Src::CompartmentOpenClose, false);
        assert_eq!(c.is_chinese_mode(), start_chinese, "被拒，模式不变");
        let frames: Vec<Vec<u8>> = cap.try_iter().collect();
        assert!(
            pushed_state(&frames),
            "start_chinese={start_chinese}：拒绝后必须推一次状态"
        );
    }
}

/// per-app 定位规则整条压过全局，坐标必须与定位方式同层取：「模式取 per-app、坐标回落全局」
/// 会让候选窗跳到为别的应用摆的坐标上。`(0,0)` 让 UI 落默认锚点才是这一档的正确答案。
#[test]
fn per_app_candidate_position_overrides_global_wholesale() {
    let c = coord();
    let pid = 96032u32;
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "x60_toolbox.exe".to_string());
    c.active_compat.lock().unwrap().pid = pid;
    // 全局：固定在 (1000, 1000)
    c.refresh_config_in_memory(|cfg| {
        cfg.ui.candidate.position_mode = "fixed".into();
        cfg.ui.candidate.custom_x = 1000;
        cfg.ui.candidate.custom_y = 1000;
    });
    assert_eq!(
        c.candidate_fixed_pos(),
        (true, 1000, 1000),
        "未配规则时跟随全局"
    );

    // per-app：固定但还没拖过 ⇒ (0,0) 交给 UI 落默认锚点，不得借用全局坐标。
    let mut rule = wind_config::app_compat::AppCompatRule {
        process: "x60_toolbox.exe".into(),
        candidate_position_mode: Some(wind_config::app_compat::CandidatePositionMode::Fixed),
        ..Default::default()
    };
    c.test_set_app_compat(wind_config::app_compat::AppCompat::from_rules(vec![
        rule.clone(),
    ]));
    assert_eq!(c.candidate_fixed_pos(), (true, 0, 0));

    // 拖过之后用它自己那份。
    rule.candidate_x = 320;
    rule.candidate_y = 480;
    c.test_set_app_compat(wind_config::app_compat::AppCompat::from_rules(vec![
        rule.clone(),
    ]));
    assert_eq!(c.candidate_fixed_pos(), (true, 320, 480));

    // 规则显式写 follow_caret ⇒ 压过全局的 fixed（这正是「独立一档」的意义）。
    rule.candidate_position_mode =
        Some(wind_config::app_compat::CandidatePositionMode::FollowCaret);
    c.test_set_app_compat(wind_config::app_compat::AppCompat::from_rules(vec![rule]));
    assert!(!c.candidate_fixed_pos().0);
}

/// 连接建立不是真实的 `FOCUS_GAINED`：新进程的管道连接必然先于它第一条 `FOCUS_GAINED`。
/// `apply_connected_pid_compat` 若整体覆写 `active_compat.pid`，随后真正的 `FOCUS_GAINED`
/// 会被 `crossed` 判据误判成「未跨进程」，吞掉 `initial_mode`/`initial_punct` 与首键竞态
/// 消除逻辑。本测试钉住 `.pid`（此处默认值 0）必须原封不动。
#[test]
fn apply_connected_pid_compat_does_not_claim_pid_identity() {
    let c = coord();
    let pid = 8848u32;
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "alacritty.exe".to_string());
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "alacritty.exe".into(),
        caret_offset_y: 12,
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

    c.apply_connected_pid_compat(pid, pid);

    assert_eq!(
        c.active_compat.lock().unwrap().pid,
        0,
        "连接建立不得提前认领 pid 身份，否则该进程随后第一条真实 FOCUS_GAINED 的 \
             crossed 判据会被误判成「同进程」"
    );
}

/// 后台宿主的无关重连（管道抖动等）不得覆盖真正聚焦应用的规则字段——
/// 否则「哪个应用的规则生效」会被连接顺序而非焦点决定。
#[test]
fn apply_connected_pid_compat_ignores_non_foreground_pid() {
    let c = coord();
    let focused_pid = 100u32;
    let background_pid = 200u32;
    c.pid_names
        .lock()
        .unwrap()
        .insert(background_pid, "alacritty.exe".to_string());
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "alacritty.exe".into(),
        caret_offset_y: 12,
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);

    // background_pid 建立连接，但当前前台窗口仍是 focused_pid。
    c.apply_connected_pid_compat(background_pid, focused_pid);

    assert_eq!(
        c.active_compat.lock().unwrap().caret_offset_y,
        0,
        "非前台 pid 的连接不得把它的规则字段写进 active_compat"
    );
}

#[test]
fn update_active_compat_extracts_pid_and_caches() {
    let c = coord();
    // client_token = PID<<32 | instance。PID=0（无效）不更新缓存。
    c.update_active_compat(0);
    assert_eq!(*c.active_compat.lock().unwrap(), ActiveCompat::default());
    // 合法 PID：headless（非真实进程）下 process_name 取不到名字 → caret_use_top=false，
    // 但 pid 应被缓存（避免重复 OpenProcess）。
    let token = (4321u64 << 32) | 7;
    c.update_active_compat(token);
    assert_eq!(c.active_compat.lock().unwrap().pid, 4321);
}

#[test]
fn update_active_compat_prefers_cached_name_over_process_lookup() {
    // macOS 路径：宿主名由 `.app` 随焦点事件送进 pid_names，`process_name` 恒空串。
    // 缓存必须优先于反查，否则 compat 规则永远匹配不到宿主。
    let c = coord();
    let pid = 5150u32;
    let token = (pid as u64) << 32 | 3;
    c.pid_names
        .lock()
        .unwrap()
        .insert(pid, "com.apple.textedit".into());
    let rules = vec![wind_config::app_compat::AppCompatRule {
        process: "com.apple.textedit".into(),
        first_show_mode: Some(wind_config::app_compat::FirstShowMode::Fast),
        ..Default::default()
    }];
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(rules);
    c.update_active_compat(token);
    assert_eq!(
        c.active_compat.lock().unwrap().first_show_mode,
        Some(wind_config::app_compat::FirstShowMode::Fast),
        "缓存里的 bundle id 必须参与 compat 规则匹配"
    );
}

#[test]
fn update_active_compat_loads_composition_start_pair_guard() {
    let c = coord();
    let pid = 5151u32;
    let token = (pid as u64) << 32 | 4;
    c.pid_names.lock().unwrap().insert(pid, "qq.exe".into());
    *c.app_compat.lock().unwrap() = wind_config::app_compat::AppCompat::from_rules(vec![
        wind_config::app_compat::AppCompatRule {
            process: "QQ.exe".into(),
            composition_start_pair_guard: Some(true),
            ..Default::default()
        },
    ]);

    c.update_active_compat(token);

    assert!(
        c.active_compat.lock().unwrap().composition_start_pair_guard,
        "焦点路径必须把 QQ 的帧对保护从 compat 规则接入热路径"
    );
}
