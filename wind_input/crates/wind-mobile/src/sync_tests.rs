use super::*;
use std::time::Instant;
use wind_config::Config;
use wind_coordinator::web_host::WebDataHost;
use wind_coordinator::{Coordinator, InputProfile};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::{MemBackend, MemLocal, ScopeFlags, SyncState, remote};
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

fn mobile(tag: &str, remote: &MemBackend) -> (Arc<MobileSync>, Arc<MobileCore>, PathBuf) {
    let (core, user) = core(tag);
    let state = user.join("_state");
    let sync = MobileSync::with_parts(
        &user,
        &state,
        core.clone(),
        Box::new(MemFactory(remote.clone())),
        KdfParams::FAST_TEST,
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

fn join(s: &MobileSync, pass: &str) {
    s.set_settings(&settings_json()).unwrap();
    s.command("set_secret", r#"{"password":"pw"}"#).unwrap();
    s.command("set_passphrase", &json!({ "passphrase": pass }).to_string())
        .unwrap();
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
}

impl Desktop {
    fn new(remote: &MemBackend, pass: &str) -> Desktop {
        Desktop {
            local: MemLocal::new(),
            state: SyncState::new("desk0"),
            dek: remote::init_remote(remote, pass, KdfParams::FAST_TEST).unwrap(),
        }
    }
    fn sync(&mut self, remote: &MemBackend) {
        let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0, test_ns());
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
    let mut desk = Desktop::new(&remote, "pass");
    desk.local.set(UD, &dict_text_with("ni hao", "你好"));
    desk.local.set("config.toml", "[ui]\nfont_size = 99\n");
    desk.sync(&remote);

    let (sync, core, user) = mobile("join", &remote);
    assert_eq!(phase(&sync)["phase"], "disabled");
    join(&sync, "pass");
    // 加入后自动确认身份、新建独立组（状态帧晚于回执到达）
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
    let _ = std::fs::remove_dir_all(user);
}

#[test]
fn mobile_config_is_not_applied_on_desktop() {
    let remote = MemBackend::new();
    let mut desk = Desktop::new(&remote, "pass");
    desk.sync(&remote);
    let (sync, _core, user) = mobile("cfg", &remote);
    std::fs::write(user.join("config.toml"), "[ui]\nfont_size = 7\n").unwrap();
    join(&sync, "pass");
    sync_now(&sync);
    desk.sync(&remote);
    assert!(desk.local.text("config.toml").is_none());
    assert!(
        !desk.state.files.contains_key("mobile/config.toml"),
        "别的类别的条目不进本机状态"
    );
    sync.shutdown();
    let _ = std::fs::remove_dir_all(user);
}

#[test]
fn sync_if_due_respects_interval_and_configuration() {
    let remote = MemBackend::new();
    let (sync, _core, user) = mobile("due", &remote);
    assert!(!sync.sync_if_due(0), "未启用不同步");
    join(&sync, "pass");
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
    let _ = std::fs::remove_dir_all(user);
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
    assert!(user.join("_state/sync/settings.toml").is_file());
    assert!(sync.command("nope", "").is_err());
    sync.shutdown();
    let _ = std::fs::remove_dir_all(user);
}
