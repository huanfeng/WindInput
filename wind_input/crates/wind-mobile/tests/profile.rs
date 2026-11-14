//! 门面必须显式声明移动 Profile，且重载配置后仍是移动 Profile。
//!
//! 只放这一个测试：`MobileCore::new` 会写进程环境变量（HOME / XDG_* / WIND_INSTALL_ROOT），
//! 同进程并行跑多个会互相干扰。

use std::path::PathBuf;
use std::sync::Arc;

use wind_coordinator::InputProfile;
use wind_mobile::config_api::ConfigValue;
use wind_mobile::{CandidateFrame, InputStatus, MobileCommand, MobileCore, MobileEventSink};

struct NullSink;

impl MobileEventSink for NullSink {
    fn on_candidates(&self, _: CandidateFrame) {}
    fn on_hide_candidates(&self) {}
    fn on_status(&self, _: InputStatus) {}
    fn on_toast(&self, _: String) {}
}

#[test]
fn reload_keeps_mobile_overlay() {
    // 数据目录要是 `<安装根>/data` 的形状；用仓库自带的 build_dev/data
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data_dir.join("config.toml").exists() {
        // 主仓 CI 没有 build_dev/data：CI 环境跳过，本地缺数据必须失败
        if std::env::var_os("CI").is_some() {
            eprintln!("跳过：缺少出厂数据 {}（CI 环境）", data_dir.display());
            return;
        }
        panic!(
            "缺少出厂数据 {}，本地不允许跳过（设 CI 环境变量可跳过）",
            data_dir.display()
        );
    }
    let user_root =
        std::env::temp_dir().join(format!("wind-mobile-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user_root);
    std::fs::create_dir_all(&user_root).unwrap();

    let core = MobileCore::new(&data_dir, &user_root, "pinyin", Arc::new(NullSink));
    assert_eq!(
        core.profile(),
        InputProfile::mobile(),
        "构造后即是移动 Profile"
    );

    // 用户层改移动段：联想类型与语法权重
    core.config_set("mobile.association.kind", ConfigValue::Str("word".into()))
        .unwrap();
    core.config_set("mobile.grammar.weight", ConfigValue::Float(0.42))
        .unwrap();
    core.run_command(MobileCommand::ReloadConfig);

    assert_eq!(core.profile(), InputProfile::mobile(), "重载不改 Profile");
    let coord = core.debug_coordinator();
    assert_eq!(
        coord.debug_assoc_kind(),
        "word",
        "联想按 [mobile.association]"
    );
    assert_eq!(
        coord.debug_pinyin_grammar_weight(),
        0.42,
        "引擎语法配置取 [mobile.grammar]"
    );

    let _ = std::fs::remove_dir_all(&user_root);
}
