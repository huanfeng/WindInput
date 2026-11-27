use super::*;
use std::time::Instant;
use wind_config::Config;
use wind_coordinator::web_host::WebDataHost;
use wind_coordinator::{Coordinator, InputProfile};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::{MemBackend, MemLocal, SyncState, remote};
use wind_sync_host::testing::MemFactory;

fn test_ns() -> wind_sync::scope::Namespace {
    wind_sync::scope::Namespace::new(wind_sync::DeviceClass::Desktop, "g")
}

const WAIT: Duration = Duration::from_secs(15);
const T0: u64 = 1_795_478_400;
const UD: &str = "data/pinyin/user.dict.yaml";

/// 直接拼 [`MobileCore`]（不挂环境变量与出厂目录）。⚠️ 目录名带 pid：并行会话共用 temp 时固定名会互删。
fn core(tag: &str) -> (Arc<MobileCore>, PathBuf) {
    let user = std::env::temp_dir().join(format!("wind_mobile_sy_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let (coord, _rx) =
        Coordinator::new_session(Config::default(), None, Some(&user), InputProfile::mobile());
    let core = Arc::new(MobileCore {
        coord,
        warming: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        data_dir: user.clone(),
        auto_spaced: std::sync::atomic::AtomicBool::new(false),
        auto_space: std::sync::atomic::AtomicBool::new(true),
        touch_quick_trigger: std::sync::atomic::AtomicBool::new(false),
    });
    (core, user)
}

fn settings_json() -> String {
    json!({
        "enabled": true,
        "backend": {"type": "webdav", "url": "http://127.0.0.1/dav", "user": "u"},
    })
    .to_string()
}

/// 本地目录放在用户目录之外，与 Android 的 `localRoot` / `userRoot` 布局一致（见 [`MobileSync::with_parts`]）。
fn local_dir(user: &Path) -> PathBuf {
    let mut s = user.as_os_str().to_os_string();
    s.push("_local");
    PathBuf::from(s)
}

/// 删掉测试的用户目录与它的本地目录。
fn cleanup(user: &Path) {
    let _ = std::fs::remove_dir_all(local_dir(user));
    let _ = std::fs::remove_dir_all(user);
}

fn mobile(tag: &str, remote: &MemBackend) -> (Arc<MobileSync>, Arc<MobileCore>, PathBuf) {
    let (core, user) = core(tag);
    let state = local_dir(&user);
    let _ = std::fs::remove_dir_all(&state);
    let sync = MobileSync::with_parts(
        &user,
        &state,
        core.clone(),
        Box::new(MemFactory(remote.clone())),
    );
    (sync, core, user)
}

fn phase(s: &MobileSync) -> Value {
    serde_json::from_str(&s.status()).unwrap()
}

fn wait(what: &str, s: &MobileSync, f: impl Fn(&Value) -> bool) -> Value {
    let t = Instant::now();
    loop {
        let v = phase(s);
        if f(&v) {
            return v;
        }
        assert!(t.elapsed() < WAIT, "等待「{what}」超时: {v}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `key` 为 `None` 时在空远端生成同步密钥，否则粘贴该密钥加入。
fn join(s: &MobileSync, key: Option<&str>) {
    s.set_settings(&settings_json()).unwrap();
    s.command("set_secret", r#"{"password":"pw"}"#).unwrap();
    match key {
        None => s.command("create_key", "").unwrap(),
        Some(k) => s
            .command("import_key", &json!({ "key": k }).to_string())
            .unwrap(),
    };
}

/// 跑一轮并等它结束（先见 syncing 或 last_ok 前进）。
fn sync_now(s: &MobileSync) -> Value {
    let before = phase(s)["last_ok"].as_i64();
    s.command("sync_now", "").unwrap();
    wait("同步结束", s, |v| {
        v["phase"] == "idle" && v["last_ok"].as_i64() > before || v["phase"] == "error"
    })
}

/// 模拟另一台桌面设备：用引擎直接上传文件。
struct Desktop {
    local: MemLocal,
    state: SyncState,
    dek: wind_sync::Dek,
    key: String,
}

impl Desktop {
    fn new(remote: &MemBackend) -> Desktop {
        let dek = remote::init_remote(remote).unwrap();
        Desktop {
            local: MemLocal::new(),
            state: SyncState::new("desk0"),
            key: dek.to_key_string(),
            dek,
        }
    }
    fn sync(&mut self, remote: &MemBackend) {
        let mut ctx = SyncCtx::new(T0, test_ns());
        ctx.force = true;
        sync_once(remote, &mut self.local, &mut self.state, &self.dek, &ctx).unwrap();
    }
}

/// 用真核心产出一份带词的用户词库文本（格式与落盘一致）。
fn dict_text_with(word_code: &str, word: &str) -> String {
    let (donor, dir) = core("donor");
    donor.user_word_add("pinyin", word_code, word).unwrap();
    donor.coord.user_store().unwrap().flush_user_data().unwrap();
    let text = std::fs::read_to_string(dir.join(UD)).expect("用户词库应已落盘");
    let _ = std::fs::remove_dir_all(dir);
    text
}

#[test]
fn desktop_dict_reaches_mobile_core_and_config_stays_apart() {
    let remote = MemBackend::new();
    let mut desk = Desktop::new(&remote);
    desk.local.set(UD, &dict_text_with("ni hao", "你好"));
    desk.local.set("config.toml", "[ui]\nfont_size = 99\n");
    desk.sync(&remote);

    let (sync, core, user) = mobile("join", &remote);
    assert_eq!(phase(&sync)["phase"], "disabled");
    join(&sync, Some(&desk.key));
    wait("待确认身份", &sync, |v| v["needs_identity"] == true);
    assert!(!sync.sync_if_due(0), "身份未确认不同步");
    sync.command("confirm_identity", "{}").unwrap();
    wait("确认身份", &sync, |v| {
        v["joined"] == true && v["needs_identity"] == false && v["group"] == v["device"]
    });
    let v = sync_now(&sync);
    assert_eq!(v["phase"], "idle", "{v}");

    let words = core.user_words("pinyin", "", 0, 50);
    assert!(
        words.items.iter().any(|w| w.text == "你好"),
        "桌面上传的词应已进入核心: {words:?}"
    );
    assert!(user.join(UD).is_file());
    assert!(
        !user.join("config.toml").exists(),
        "桌面的 config.toml 不得落到手机"
    );
    sync.shutdown();
    cleanup(&user);
}

#[test]
fn mobile_config_is_not_applied_on_desktop() {
    let remote = MemBackend::new();
    let mut desk = Desktop::new(&remote);
    desk.sync(&remote);
    let (sync, _core, user) = mobile("cfg", &remote);
    std::fs::write(user.join("config.toml"), "[ui]\nfont_size = 7\n").unwrap();
    join(&sync, Some(&desk.key));
    sync.command("confirm_identity", "{}").unwrap();
    sync_now(&sync);
    desk.sync(&remote);
    assert!(desk.local.text("config.toml").is_none());
    assert!(
        !desk.state.files.contains_key("mobile/config.toml"),
        "别的类别的条目不进本机状态"
    );
    sync.shutdown();
    cleanup(&user);
}

#[test]
fn sync_if_due_respects_interval_and_configuration() {
    let remote = MemBackend::new();
    let (sync, _core, user) = mobile("due", &remote);
    assert!(!sync.sync_if_due(0), "未启用不同步");
    join(&sync, None);
    let v = sync_now(&sync);
    let last = v["last_ok"].as_i64().unwrap();
    assert!(!sync.sync_if_due(600), "刚成功过，间隔未到");
    let started = sync.sync_if_due(0);
    assert!(started, "间隔为 0 应起一轮");
    wait("第二轮结束", &sync, |v| {
        v["phase"] == "idle" && v["last_ok"].as_i64().unwrap() >= last
    });
    assert!(!sync.sync_if_due(600), "刚尝试过，间隔未到");
    sync.shutdown();
    assert!(!sync.sync_if_due(0), "关闭后不再起");
    assert!(sync.command("sync_now", "").is_err());
    cleanup(&user);
}

#[test]
fn settings_round_trip_and_validation() {
    let remote = MemBackend::new();
    let (sync, _core, user) = mobile("set", &remote);
    assert!(
        sync.set_settings(r#"{"enabled": true}"#).is_err(),
        "启用须有后端"
    );
    sync.set_settings(&settings_json()).unwrap();
    let got: Value = serde_json::from_str(&sync.get_settings()).unwrap();
    assert_eq!(got["settings"]["enabled"], true);
    assert_eq!(got["has_secret"], false);
    assert!(local_dir(&user).join("sync/settings.toml").is_file());
    assert!(sync.command("nope", "").is_err());
    sync.shutdown();
    cleanup(&user);
}

/// 粘贴密钥加入后不自动确认身份，确认前不同步；不能恢复为桌面设备。
#[test]
fn joined_mobile_waits_for_identity_and_cannot_claim_desktop() {
    let remote = MemBackend::new();
    let mut desk = Desktop::new(&remote);
    desk.sync(&remote);
    let (sync, _core, user) = mobile("idn", &remote);
    sync.set_settings(&settings_json()).unwrap();
    sync.command("set_secret", r#"{"password":"pw"}"#).unwrap();
    sync.command("import_key", &json!({ "key": desk.key }).to_string())
        .unwrap();
    wait("未确认", &sync, |v| v["needs_identity"] == true);
    assert!(!sync.sync_if_due(0));
    let desk_info = wind_sync::devices::DeviceInfo {
        name: "台式机".into(),
        platform: "linux".into(),
        app_version: String::new(),
        caps: Vec::new(),
        last_seen: 0,
        group: "desk0".into(),
        group_name: String::new(),
        group_name_at: 0,
    };
    wind_sync::devices::put_device_info(&remote, &desk.dek.devinfo_key(), "desk0", &desk_info)
        .unwrap();
    let e = sync
        .command("claim_device", &json!({ "dev": "desk0" }).to_string())
        .unwrap_err();
    assert!(e.contains("不是同一类设备"), "{e}");
    sync.command("confirm_identity", "{}").unwrap();
    wait("已确认", &sync, |v| v["needs_identity"] == false);
    assert!(sync.sync_if_due(0));
    sync.shutdown();
    cleanup(&user);
}

/// 手机没有软键盘文件重载路径与扩展：这几个请求是成功的空操作（同步写入扩展 / 软键盘文件时会发）。
#[test]
fn desktop_only_reload_requests_are_noops_on_mobile() {
    let (core, user) = core("noop");
    let link = InProcLink {
        core,
        shared: Arc::new(Shared::default()),
    };
    for m in [
        "softkeyboard.reload",
        "extensions.suspend",
        "extensions.resume",
    ] {
        assert!(link.request(m, json!({ "ids": ["k"] })).is_ok(), "{m}");
    }
    cleanup(&user);
}

/// 认领另一台设备时同步进程发 `stats.replaceMonths`：本机统计换成给定月表，累计随之重算。
#[test]
fn replace_months_request_swaps_stats() {
    let (core, user) = core("stats_replace");
    let link = InProcLink {
        core: core.clone(),
        shared: Arc::new(Shared::default()),
    };
    let months =
        json!({ "months": { "2020-01": "version = 1\n[days.\"2020-01-05\"]\nchinese = 7\n" } });
    assert_eq!(
        link.request("stats.replaceMonths", months).unwrap()["written"],
        1
    );
    assert_eq!(core.stats_summary().unwrap().total_chars, 7);
    assert!(
        link.request("stats.replaceMonths", json!({ "months": { "2020-1": "" } }))
            .is_err()
    );
    cleanup(&user);
}

/// Runner 已结束的轮次数（状态帧里有，门面 JSON 不透出）。
fn rounds(s: &MobileSync) -> u64 {
    lock(&s.shared.status).round
}

/// 等 `before` 之后的一轮结束。
fn wait_round_after(s: &MobileSync, before: u64) {
    let t = Instant::now();
    while rounds(s) <= before || phase(s)["phase"] == "syncing" {
        assert!(t.elapsed() < WAIT, "等待一轮结束超时: {}", s.status());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 手动立即同步一轮并等它结束。
fn manual_round(s: &MobileSync) {
    let before = rounds(s);
    s.command("sync_now", "").unwrap();
    wait_round_after(s, before);
}

fn record(core: &MobileCore, n: u32) {
    let store = core.coord.user_store().unwrap();
    for _ in 0..n {
        store.record_freq("pinyin", "ni", "你").unwrap();
    }
    store.flush_user_data().unwrap();
}

fn freq(core: &MobileCore) -> u32 {
    let store = core.coord.user_store().unwrap();
    store
        .get_freq("pinyin", "ni", "你")
        .unwrap()
        .map_or(0, |r| r.count)
}

/// 两台手机同库：学习数据经同步到达对方并计数相加，且进入核心内存（`data.beginEdit/endEdit` 对学习文件可用）。
/// 键盘弹出触发的一轮（`sync_if_due`）不强制：30 分钟内不再交换学习数据，手动立即同步仍交换。
#[test]
fn learned_data_syncs_between_phones_and_popup_round_is_not_forced() {
    let remote = MemBackend::new();
    let (a, core_a, user_a) = mobile("lrn_a", &remote);
    join(&a, None);
    record(&core_a, 3);
    manual_round(&a);
    let key: Value = serde_json::from_str(&a.command("export_key", "").unwrap()).unwrap();
    let key = key["key"].as_str().unwrap().to_string();

    let (b, core_b, user_b) = mobile("lrn_b", &remote);
    join(&b, Some(&key));
    b.command("confirm_identity", "{}").unwrap();
    wait("b 已确认", &b, |v| v["needs_identity"] == false);
    record(&core_b, 2);
    manual_round(&b);
    assert_eq!(freq(&core_b), 5, "{}", b.status());
    manual_round(&a);
    assert_eq!(freq(&core_a), 5, "{}", a.status());

    record(&core_a, 1);
    let before = rounds(&a);
    assert!(a.sync_if_due(0));
    wait_round_after(&a, before);
    manual_round(&b);
    assert_eq!(freq(&core_b), 5, "弹出触发的一轮不该提前交换学习数据");

    manual_round(&a);
    manual_round(&b);
    assert_eq!(freq(&core_b), 6, "手动立即同步强制交换");
    // 引擎状态与同步私有状态都不该当用户文件互传：两机首次配对也不出冲突副本
    for s in [&a, &b] {
        assert_eq!(phase(s)["conflicts"], 0, "{}", s.status());
    }

    a.shutdown();
    b.shutdown();
    cleanup(&user_a);
    cleanup(&user_b);
}

/// 门面状态与桌面 `sync.status` 同形：已移除设备、空间占用与统计中/失败原因都透出，空时省略。
#[test]
fn status_carries_removed_devices_and_usage() {
    let remote = MemBackend::new();
    let (sync, _core, user) = mobile("st_usage", &remote);
    let v = phase(&sync);
    for k in ["removed_with_data", "usage", "usage_busy", "usage_error"] {
        assert!(v.get(k).is_none(), "空时省略 {k}: {v}");
    }
    // 先停 Runner，免得它的状态帧覆盖下面手填的状态
    sync.shutdown();
    {
        let mut st = lock(&sync.shared.status);
        st.removed_with_data = vec!["old".into()];
        st.usage = Some(wind_sync_ipc::Usage {
            total: 42,
            ..Default::default()
        });
        st.usage_busy = true;
        st.usage_error = Some("断网".into());
    }
    let v = phase(&sync);
    assert_eq!(v["removed_with_data"], json!(["old"]));
    assert_eq!(v["usage"]["total"], 42);
    assert_eq!(v["usage_busy"], true);
    assert_eq!(v["usage_error"], "断网");
    assert_eq!(v["phase"], "disabled");
    cleanup(&user);
}

/// 按设备看统计：设备列表即有他机统计的设备号；`all`/`self`/设备号三种口径，未知设备报错；原接口即 `all`。
#[test]
fn stats_by_device() {
    let (core, user) = core("stats_dev");
    let store = core.coord.user_store().unwrap().clone();
    assert!(core.stats_devices().is_empty());
    store
        .put_daily_stat(
            "2020-01-05",
            &wind_store::stats::DailyStats {
                chinese: 7,
                ..Default::default()
            },
        )
        .unwrap();
    let d = store.stats_dir().join("devices/phone");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("2020-01.toml"),
        "version = 1\n[days.\"2020-01-05\"]\nchinese = 3\n",
    )
    .unwrap();
    assert_eq!(core.stats_devices(), ["phone"]);
    let total = |dev: &str| {
        core.stats_summary_for(dev)
            .unwrap()
            .map(|s| s.total_chars)
            .unwrap()
    };
    assert_eq!(total("phone"), 3);
    assert_eq!(total("all"), core.stats_summary().unwrap().total_chars);
    assert!(core.stats_summary_for("tablet").is_err());
    let day = |dev: &str| {
        core.stats_daily_for("2020-01-05", "2020-01-05", dev)
            .unwrap()[0]
            .total
    };
    assert_eq!(day("phone"), 3);
    assert_eq!(
        day("all"),
        core.stats_daily("2020-01-05", "2020-01-05")[0].total
    );
    assert!(
        core.stats_daily_for("2020-01-05", "2020-01-05", "tablet")
            .is_err()
    );
    cleanup(&user);
}

/// 撞号停止时状态是 `error`，`sync_if_due` 的阶段闸门会放行；弹出触发的 auto 不得解除这个停止态、也不起轮。
#[test]
fn popup_sync_does_not_clear_device_id_conflict() {
    let remote = MemBackend::new();
    let (sync, _core, user) = mobile("halt", &remote);
    join(&sync, None);
    manual_round(&sync);
    let dev = phase(&sync)["device"].as_str().unwrap().to_string();
    let key: Value = serde_json::from_str(&sync.command("export_key", "").unwrap()).unwrap();
    let dek = wind_sync::Dek::from_key_string(key["key"].as_str().unwrap()).unwrap();
    // 另一处用同一个设备号连写两代清单，本机的清单链就接不上了
    let mut other = SyncState::new(&dev);
    let ns = wind_sync::scope::Namespace::new(wind_sync::DeviceClass::Mobile, &dev);
    for _ in 0..2 {
        let mut ctx = SyncCtx::new(T0, ns.clone());
        ctx.force = true;
        sync_once(&remote, &mut MemLocal::new(), &mut other, &dek, &ctx).unwrap();
    }
    manual_round(&sync);
    let v = phase(&sync);
    assert_eq!(v["phase"], "error", "{v}");
    let before = rounds(&sync);
    assert!(sync.sync_if_due(0), "阶段闸门放行，交给 Runner 判停止态");
    // 命令按序处理：这条回来时 auto 已处理完
    sync.command("export_key", "").unwrap();
    assert_eq!(phase(&sync)["phase"], "error", "停止态下 auto 不起轮");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rounds(&sync), before, "停止态下 auto 不起轮");
    assert_eq!(phase(&sync)["last_error"], v["last_error"]);
    sync.shutdown();
    cleanup(&user);
}
