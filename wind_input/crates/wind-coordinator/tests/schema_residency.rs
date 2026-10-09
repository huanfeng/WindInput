//! 方案常驻策略（`schema.keep_all_loaded`，设计 `docs/design/memory-footprint.md` §5）端到端：
//! 启动常驻集合、启动后台校验全部方案的派生缓存、闲置淘汰与保护名单、运行期两个方向的切换。
//!
//! 整棵目录树经**便携标记**重定向到临时目录（`WIND_INSTALL_ROOT` + `portable_mode`）：
//! 词库缓存落在 `<root>/localdata/cache`（可观测、不碰真实缓存），`reload_user_config` 读
//! `<root>/userdata/config.toml`。一个进程只能重定向一次，故单开测试二进制；用例共用这棵树，
//! 用 `LOCK` 串行、每条开头重写配置。
//!
//! ⚠️ 不依赖 `build_dev/data`：方案全部自造（码表 za / zb / zc / zd + 混输 zm），不会静默跳过。
//!
//! 方案角色（各用例同一套）：
//! - `za`：当前方案；
//! - `zc`：临拼目标（`primary_pinyin = "zc"`——目标只按 id 解析，码表方案也能当靶子）；
//! - `zm`：混输，主码表成员 `zb`（成员不保护：混输自建子引擎）；
//! - `zd`：无任何保护；
//! - `english`：英文方案（只在 `schema.mix.enable_english` 的用例里被混输取走）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::web_host::WebDataHost;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_ui_types::MenuCmd;

static LOCK: Mutex<()> = Mutex::new(());

const CODETABLES: [(&str, &str); 4] = [("za", "甲"), ("zb", "乙"), ("zc", "丙"), ("zd", "丁")];

/// 便携根目录（进程内只建一次）。
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
        let root = std::env::temp_dir().join(format!("wind_residency-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let schemas = root.join("data/schemas");
        for (id, word) in CODETABLES {
            std::fs::create_dir_all(schemas.join(id)).unwrap();
            std::fs::write(
                schemas.join(format!("{id}.schema.toml")),
                format!(
                    "[schema]\nid = \"{id}\"\nname = \"{id}\"\n\
                     [engine]\ntype = \"codetable\"\n\
                     [engine.codetable]\nmax_code_length = 4\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{id}/{id}.dict.yaml\"\ndefault = true\n"
                ),
            )
            .unwrap();
            write_dict(id, word);
        }
        std::fs::write(
            schemas.join("zm.schema.toml"),
            "[schema]\nid = \"zm\"\nname = \"zm\"\n\
             [engine]\ntype = \"mixed\"\n\
             [engine.mixed]\nprimary_schema = \"zb\"\nsecondary_schema = \"\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(schemas.join("english")).unwrap();
        std::fs::write(
            schemas.join("english.schema.toml"),
            "[schema]\nid = \"english\"\nname = \"english\"\n\
             [engine]\ntype = \"english\"\n\
             [engine.codetable]\nmax_code_length = 32\n\
             [[dictionaries]]\nid = \"en_main\"\npath = \"english/en.dict.yaml\"\n\
             type = \"english\"\ndefault = true\n",
        )
        .unwrap();
        std::fs::write(
            schemas.join("english/en.dict.yaml"),
            "---\nname: en\nversion: \"1\"\n...\nhello\thello\n",
        )
        .unwrap();
        std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
        std::fs::create_dir_all(root.join("userdata")).unwrap();
        // SAFETY: 在任何 OnceLock（variant / 路径缓存 / 引擎缓存根）初始化之前设置；
        // ROOT 的 get_or_init 保证只跑一次，且用例都先经 `LOCK` 串行。
        unsafe {
            std::env::set_var("WIND_INSTALL_ROOT", &root);
        }
        assert!(wind_config::variant::is_portable(), "前置条件：便携标记须生效");
        assert_eq!(
            Config::cache_dir(),
            Some(root.join("localdata/cache")),
            "前置条件：缓存根须已重定向，否则本测试会读写真实缓存"
        );
        root
    })
}

fn write_dict(id: &str, word: &str) {
    std::fs::write(
        root_unchecked()
            .join("data/schemas")
            .join(format!("{id}/{id}.dict.yaml")),
        format!("---\nname: {id}\nversion: \"1\"\n...\n{word}\ta\n"),
    )
    .unwrap();
}

/// `root()` 初始化期间也要写词库，那时 OnceLock 还没落值。
fn root_unchecked() -> PathBuf {
    std::env::temp_dir().join(format!("wind_residency-{}", std::process::id()))
}

fn user() -> PathBuf {
    root().join("userdata")
}

fn cache_root() -> PathBuf {
    root().join("localdata/cache")
}

/// `<cache>/<id>/` 下的 `.wdat`（码表词库缓存）。
fn wdat_of(id: &str) -> Option<PathBuf> {
    std::fs::read_dir(cache_root().join(id))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "wdat"))
}

/// 每条用例的起点。`keep_all` 为 `schema.keep_all_loaded`。
fn reset(keep_all: bool) {
    reset_with(keep_all, "");
}

/// 同 [`reset`]，`extra` 原样追加到 config.toml 末尾（整张表，如 `[schema.mix]`）。
fn reset_with(keep_all: bool, extra: &str) {
    root();
    std::fs::write(
        user().join("config.toml"),
        format!(
            "[schema]\nactive = \"za\"\navailable = [\"za\", \"zb\", \"zc\", \"zd\", \"zm\"]\n\
             primary_codetable = \"zb\"\nprimary_pinyin = \"zc\"\nkeep_all_loaded = {keep_all}\n\
             [input.temp_pinyin]\nenabled = true\n\
             [input.temp_english]\nshow_candidates = false\n{extra}"
        ),
    )
    .unwrap();
}

fn set_keep_all(keep_all: bool) {
    let p = user().join("config.toml");
    let text = std::fs::read_to_string(&p).unwrap();
    let text = text.replace(
        &format!("keep_all_loaded = {}", !keep_all),
        &format!("keep_all_loaded = {keep_all}"),
    );
    std::fs::write(&p, text).unwrap();
}

/// 设置页保存了一个会把 schema 段标脏的键（引擎整体重建）。
fn dirty_schema_save() {
    let p = user().join("config.toml");
    let mut text = std::fs::read_to_string(&p).unwrap();
    // 夹具没有 data/config.toml，出厂值取 L1（false），故写 true 才是一次变更。
    text.push_str("[schema.codetable]\ntop_code_commit = true\n");
    std::fs::write(&p, text).unwrap();
}

/// 等后台线程把已加载集合推到 `want`（最多 20 秒）。
fn wait_loaded(c: &Coordinator, want: &[String]) {
    let t0 = Instant::now();
    while loaded(c) != want && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn make() -> Arc<Coordinator> {
    let data = Config::data_dir();
    Coordinator::new_headless_with_ui_at(
        Config::load(data.as_deref()).unwrap(),
        data.as_deref(),
        Some(&user()),
    )
    .0
}

fn loaded(c: &Coordinator) -> Vec<String> {
    c.engine_mgr().loaded_schemas()
}

fn sorted(v: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn key(c: &Coordinator, vk: u32) {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    });
}

/// 菜单切到 available 第 i 项后敲 `a`，返回首页候选；敲 Esc 收尾。
fn switch_and_type(c: &Coordinator, i: usize) -> Vec<String> {
    c.debug_run_menu_cmd(MenuCmd::SchemaSelect(i));
    key(c, 0x41);
    let texts = c.debug_page_texts();
    key(c, 0x1B);
    texts
}

/// `false`：启动只常驻当前方案 + 临拼目标；其余可用方案的缓存在启动后台建出，
/// 建完即释放引擎；之后改过词库也由启动校验重建，切换时不再同步重建。
#[test]
fn startup_without_keep_all_loads_residents_and_refreshes_every_cache() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(false);
    let _ = std::fs::remove_dir_all(cache_root());
    let c = make();
    c.debug_desktop_startup_and_prewarm();
    assert_eq!(
        loaded(&c),
        sorted(&["za", "zc"]),
        "只常驻当前方案与临拼目标"
    );
    for id in ["za", "zb", "zc", "zd"] {
        assert!(
            wdat_of(id).is_some(),
            "{id} 的词库缓存应在启动后台建出（zm 用 zb 的缓存）"
        );
    }

    // 改 zd 的词库（缓存随之过期）后重启：启动校验重建它，切过去时不再同步重建。
    drop(c);
    write_dict("zd", "戊");
    let before = std::fs::read(wdat_of("zd").unwrap()).unwrap();
    let c = make();
    c.debug_desktop_startup_and_prewarm();
    let rebuilt = wdat_of("zd").unwrap();
    assert_ne!(
        std::fs::read(&rebuilt).unwrap(),
        before,
        "词库变了，启动校验应重建 zd 的缓存"
    );
    let stamp = std::fs::metadata(&rebuilt).unwrap().modified().unwrap();
    assert_eq!(
        switch_and_type(&c, 3).first().map(String::as_str),
        Some("戊")
    );
    assert_eq!(
        std::fs::metadata(&rebuilt).unwrap().modified().unwrap(),
        stamp,
        "缓存已新鲜，切换不该再重建"
    );
    write_dict("zd", "丁");
}

/// `true`：与现状一致——启动预热全部可用方案 + 临拼目标，且清扫拍子不摘任何引擎。
#[test]
fn keep_all_loaded_prewarms_everything_and_never_evicts() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(true);
    let c = make();
    c.debug_desktop_startup_and_prewarm();
    let all = sorted(&["za", "zb", "zc", "zd", "zm"]);
    assert_eq!(loaded(&c), all);
    std::thread::sleep(Duration::from_millis(50));
    assert!(c.debug_idle_sweep(Duration::ZERO).is_empty());
    assert_eq!(loaded(&c), all, "常驻开着时一个都不摘");
}

/// 闲置淘汰：未用的摘掉；当前方案、临拼目标不摘；混输成员不保护（混输自建子引擎）；
/// 摘掉后再切回能正常出字；摘除不清掉仍在用的反查索引。
#[test]
fn idle_eviction_spares_protected_and_in_use_indexes() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(false);
    let c = make();
    c.debug_desktop_startup_and_prewarm();
    let em = c.engine_mgr();
    for id in ["zb", "zd", "zm"] {
        assert!(em.prewarm_schema(id));
    }
    // zb 是主码表：它的反查索引给编码提示用，与 zb 的引擎在不在内存无关。zd 的不在用。
    assert!(em.prewarm_reverse_index("zb"));
    assert!(em.prewarm_reverse_index("zd"));
    assert_eq!(loaded(&c), sorted(&["za", "zb", "zc", "zd", "zm"]));

    // 第一拍只推进时钟（阈值很大，谁都不摘），随后用一下 zm：它的「最后使用」= 这一拍。
    std::thread::sleep(Duration::from_secs(2));
    assert!(c.debug_idle_sweep(Duration::from_secs(3600)).is_empty());
    assert!(!em.convert_with("zm", "a", 5).candidates.is_empty());
    // 第二拍：zd、zb 闲了 ≥2s 被摘（zb 虽是 zm 的成员，混输用的是自建的子引擎）；
    // zm 刚用过；za 当前；zc 临拼目标。
    let mut evicted = c.debug_idle_sweep(Duration::from_millis(1500));
    evicted.sort();
    assert_eq!(evicted, sorted(&["zb", "zd"]));
    assert_eq!(loaded(&c), sorted(&["za", "zc", "zm"]));
    assert!(
        !em.convert_with("zm", "a", 5).candidates.is_empty(),
        "成员的独立引擎被摘，混输照常出字"
    );
    assert!(
        em.reverse_index_if_ready("zd").is_none(),
        "只属于被摘方案的反查索引随之释放"
    );
    assert!(
        em.reverse_index_if_ready("zb").is_some(),
        "主码表的反查索引仍在用，不能清"
    );

    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(c.debug_idle_sweep(Duration::from_millis(1500)), vec!["zm"]);
    assert_eq!(
        loaded(&c),
        sorted(&["za", "zc"]),
        "当前方案与临拼目标始终不摘"
    );
    assert!(
        em.reverse_index_if_ready("zb").is_some(),
        "主码表引擎被摘，它的反查索引仍留着"
    );

    // 摘掉后再切回：现建、能出字。
    assert_eq!(
        switch_and_type(&c, 3).first().map(String::as_str),
        Some("丁")
    );
    assert_eq!(
        switch_and_type(&c, 4).first().map(String::as_str),
        Some("乙")
    );
}

/// 运行期改键：true→false 立即按保护名单摘除；false→true 立即后台预热全部。不需要重启，
/// 也不触发整套引擎重建（当前方案的引擎不换）。
#[test]
fn runtime_toggle_applies_both_directions_without_restart() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(true);
    let c = make();
    c.debug_desktop_startup_and_prewarm();
    assert_eq!(loaded(&c), sorted(&["za", "zb", "zc", "zd", "zm"]));

    set_keep_all(false);
    c.reload_user_config();
    assert_eq!(
        loaded(&c),
        sorted(&["za", "zc"]),
        "关掉常驻：保护名单外的立即摘"
    );

    set_keep_all(true);
    c.reload_user_config();
    let all = sorted(&["za", "zb", "zc", "zd", "zm"]);
    let t0 = Instant::now();
    while loaded(&c) != all && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(loaded(&c), all, "打开常驻：后台预热全部可用方案");
}

/// 设置页保存了标脏 schema 段的键：引擎整体重建后，常驻集合要回来——`true` 是全部方案，
/// `false` 是保护名单（当前方案 + 临拼目标），而不是只剩当前方案直到重启。
#[test]
fn dirty_reload_restores_resident_set() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (keep_all, want) in [
        (true, sorted(&["za", "zb", "zc", "zd", "zm"])),
        (false, sorted(&["za", "zc"])),
    ] {
        reset(keep_all);
        let c = make();
        c.debug_desktop_startup_and_prewarm();
        assert_eq!(loaded(&c), want);
        dirty_schema_save();
        c.reload_user_config();
        wait_loaded(&c, &want);
        assert_eq!(
            loaded(&c),
            want,
            "keep_all_loaded={keep_all}：重建后常驻集合应回来"
        );
    }
}

/// 启动校验让混输取走了共享英文引擎、而眼下没有混输在内存、临英又关着：共享缓存要放手，
/// 英文不再受保护，闲置清扫能摘掉它。
#[test]
fn validation_releases_shared_english_when_no_consumer() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_with(false, "[schema.mix]\nenable_english = true\n");
    let c = make();
    c.debug_desktop_startup_and_prewarm();
    assert!(!loaded(&c).contains(&"zm".to_string()), "zm 校验完即释放");
    let evicted = c.debug_idle_sweep(Duration::ZERO);
    assert!(
        evicted.contains(&"english".to_string()),
        "没有消费者的英文引擎应能被摘：摘了 {evicted:?}，剩 {:?}",
        loaded(&c)
    );
}

/// 宿主声明按需加载（移动端 `set_eager_prewarm(false)`）时，把常驻打开也不在后台建全部方案。
#[test]
fn turning_keep_all_on_respects_eager_prewarm_off() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(false);
    let c = make();
    c.set_eager_prewarm(false);
    c.debug_desktop_startup_and_prewarm();
    set_keep_all(true);
    c.reload_user_config();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(loaded(&c), sorted(&["za", "zc"]));
}

/// 「全部常驻」的预热循环逐个复核开关：中途被关掉就停（此处开关一开始就是关的）。
#[test]
fn prewarm_all_stops_when_keep_all_is_off() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(false);
    let c = make();
    c.debug_prewarm_all_schemas();
    assert_eq!(loaded(&c), sorted(&["za", "zc"]));
}

/// 快捷输入成员的取数上限按成员**引擎类型**分级：成员没加载时也要按它的真实类型算
/// （码表单码 100），不能当成「类型未知」落到 300。
#[test]
fn mix_member_limit_uses_real_type_of_unloaded_member() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(false);
    let c = make();
    assert!(!loaded(&c).contains(&"zd".to_string()));
    assert_eq!(c.debug_mix_member_fetch_limit("zd", "a"), 100);
}
