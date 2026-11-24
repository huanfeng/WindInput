use super::*;

#[test]
fn dev_stem_detected() {
    assert!(is_dev_from_stem("wind_input_dev"));
    assert!(is_dev_from_stem("wind_tsf_dev"));
}

#[test]
fn non_dev_stem_rejected() {
    assert!(!is_dev_from_stem("wind_input"));
    // 旧 debug 命名不再被识别为变体
    assert!(!is_dev_from_stem("wind_input_debug"));
}

/// 测试用的绝对路径样例，按**编译目标**取——`Path::is_absolute()` 是平台语义的，
/// `D:\MyData` 只在 Windows 目标下算绝对路径，Linux/macOS 目标下一律为相对。
/// CI 在 ubuntu 上跑 `cargo test`（宿主目标），写死 Windows 路径会让所有
/// 「接受合法路径」的断言全挂，而「拒绝非法路径」的断言变成不走被测分支的假绿。
#[cfg(windows)]
const ABS_ROOT: &str = r"D:\MyData";
#[cfg(not(windows))]
const ABS_ROOT: &str = "/mnt/MyData";

/// 拼一段绝对路径，分隔符跟随平台。
fn abs(tail: &str) -> String {
    if cfg!(windows) {
        format!("{ABS_ROOT}\\{tail}")
    } else {
        format!("{ABS_ROOT}/{tail}")
    }
}

/// 安装器写的就是一行裸路径，尾随换行由 trim 吃掉。
#[test]
fn datadir_conf_plain_path_accepted() {
    let p = abs("WindInput");
    assert_eq!(parse_datadir_conf(&p), Some(PathBuf::from(&p)));
    assert_eq!(
        parse_datadir_conf(&format!("{p}\r\n")),
        Some(PathBuf::from(&p))
    );
}

/// 记事本另存为 UTF-8 会加 BOM，不能因此把整份用户配置判废。
#[test]
fn datadir_conf_bom_stripped() {
    assert_eq!(
        parse_datadir_conf(&format!("\u{feff}{ABS_ROOT}")),
        Some(PathBuf::from(ABS_ROOT))
    );
}

#[test]
fn datadir_conf_empty_rejected() {
    assert_eq!(parse_datadir_conf(""), None);
    assert_eq!(parse_datadir_conf("   \r\n"), None);
}

/// 驱动器相对路径 `X:name` 看着像绝对路径，实则解析到该盘当前目录。
/// 这是本仓踩过的坑：`Path::is_absolute` 对 `C:foo` 返回 false，正是靠它挡住。
///
/// 只在 Windows 目标下有意义：非 Windows 目标上 `C:data` 不含盘符概念，
/// 拒绝它靠的是「不以 `/` 开头」这条普通相对路径规则，验不到本测试的目标语义。
#[cfg(windows)]
#[test]
fn datadir_conf_drive_relative_rejected() {
    assert_eq!(parse_datadir_conf("C:data"), None);
}

/// 普通相对路径两平台都该拒。
#[test]
fn datadir_conf_relative_rejected() {
    assert_eq!(parse_datadir_conf(r"data\WindInput"), None);
    assert_eq!(parse_datadir_conf("data/WindInput"), None);
}

/// `..` 必须挡在 `ParentDir` 那一关，而不是被 `is_absolute` 顺带挡掉——
/// 故样例必须是**本平台的合法绝对路径**，否则测试绿了也没走到被测分支。
#[test]
fn datadir_conf_parent_traversal_rejected() {
    let p = abs("..");
    assert!(
        PathBuf::from(&p).is_absolute(),
        "样例须是本平台绝对路径，否则测不到 ParentDir 分支"
    );
    assert_eq!(parse_datadir_conf(&p), None);
}

/// 便携模式自带 userdata，绝不能再去读机器全局的 datadir.conf——
/// 否则一个装过正式版的机器上，便携包会把数据写回安装版的目录。
///
/// 断言的是纯决策函数而非 `custom_userdata_dir()`：后者的便携判定来自
/// `current_exe()`，测试进程里恒为非便携，写成条件断言就成了永不执行的假测试。
#[test]
fn portable_beats_datadir_conf() {
    let conf = Some(ABS_ROOT);
    assert_eq!(decide_custom_dir(true, conf), None, "便携必须忽略配置文件");
    assert_eq!(
        decide_custom_dir(false, conf),
        Some(PathBuf::from(ABS_ROOT)),
        "非便携才认配置文件"
    );
}

/// 无配置文件 = 用默认位置，这是绝大多数用户的路径。
#[test]
fn absent_conf_falls_back_to_default() {
    assert_eq!(decide_custom_dir(false, None), None);
}

/// 便携标记新旧两名都必须认出。
///
/// 旧名兼容不是洁癖：存量便携包里全是 `wind_portable_mode`，只认新名会让这些
/// 用户的词库配置落回 `%APPDATA%`——现象是「升级后我的词库全没了」。
#[test]
fn portable_marker_accepts_new_and_legacy_names() {
    let base = std::env::temp_dir().join(format!("wind_marker_{}", std::process::id()));

    for (i, name) in [PORTABLE_MARKER_NAME, LEGACY_PORTABLE_MARKER_NAME]
        .iter()
        .enumerate()
    {
        let dir = base.join(format!("case{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!has_portable_marker_in(&dir), "无标记时不该判为便携");
        std::fs::write(dir.join(name), "portable=1\n").unwrap();
        assert!(has_portable_marker_in(&dir), "{name} 应被认出");
    }

    // 目录存在但两个名字都没有 → 非便携（安装版的常态）。
    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(!has_portable_marker_in(&plain));

    let _ = std::fs::remove_dir_all(&base);
}

/// 新名必须与安装器清单 `[app] portable_marker` 一致——两侧不同正是原 bug。
#[test]
fn portable_marker_name_matches_installer_manifest() {
    assert_eq!(PORTABLE_MARKER_NAME, "portable_mode");
}
