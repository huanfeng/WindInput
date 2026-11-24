use super::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Frames(Mutex<Vec<CandidateFrame>>);

impl MobileEventSink for Frames {
    fn on_candidates(&self, frame: CandidateFrame) {
        self.0.lock().unwrap().push(frame);
    }
    fn on_hide_candidates(&self) {}
    fn on_status(&self, _status: InputStatus) {}
    fn on_toast(&self, _text: String) {}
}

/// warming=true 时调 `key_down`：它会先关掉预热闸门，所以含 preedit 的帧仍要到达 sink。
#[test]
fn typing_during_warmup_keeps_frames() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists() {
        eprintln!("跳过：词库不存在");
        return;
    }
    let user = std::env::temp_dir().join(format!("wind_mobile_pump_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    let (coord, rx) =
        Coordinator::new_session(cfg, Some(&data), Some(&user), InputProfile::mobile());
    let warming = Arc::new(AtomicBool::new(true));
    let sink = Arc::new(Frames::default());
    spawn_session_pump(rx, sink.clone(), Arc::clone(&warming));
    let core = MobileCore {
        coord,
        warming,
        data_dir: user.clone(),
        auto_spaced: AtomicBool::new(false),
        auto_space: AtomicBool::new(true),
        touch_quick_trigger: AtomicBool::new(false),
    };
    core.key_down('A' as u32, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if sink
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.preedit == "a" && f.composing)
        {
            break;
        }
        assert!(Instant::now() < deadline, "预热期间按键的帧被丢了");
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_dir_all(&user);
}
