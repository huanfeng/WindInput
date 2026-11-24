use super::{WorkdirFallback, resolve_workdir_with};
use std::path::{Path, PathBuf};

/// 假文件系统：只有列出的路径算存在的目录。
fn dirs(list: &'static [&'static str]) -> impl Fn(&Path) -> bool {
    move |p: &Path| list.iter().any(|d| Path::new(d) == p)
}

fn home() -> Option<PathBuf> {
    Some(PathBuf::from("C:/Users/me"))
}

fn no_home() -> Option<PathBuf> {
    None
}

/// 默认策略的主用例：目标是本地程序 → 落到它自己所在目录。
/// 词典类程序按相对路径找词库，靠的就是这个语义（等价于资源管理器双击）。
#[test]
fn defaults_to_directory_of_target_program() {
    let (dir, fb) = resolve_workdir_with("D:/Dict/dict.exe", "", dirs(&["D:/Dict"]), home);
    assert_eq!(dir, "D:/Dict");
    assert_eq!(fb, None);
}

/// 裸命令名由 ShellExecute 走 PATH 解析，反推不出目录 → 中性目录。
/// 关键是不能返回空串：空串下游即继承前台宿主应用的当前目录。
#[test]
fn bare_command_name_falls_back_to_home_not_empty() {
    let (dir, fb) = resolve_workdir_with("notepad.exe", "", dirs(&[]), home);
    assert_eq!(dir, "C:/Users/me");
    assert_eq!(fb, Some(WorkdirFallback::NoTargetDir));
}

/// URL 不能走 Path::parent：`https://a/b/c` 会切出 `https://a/b` 这种似是而非的路径。
///
/// 白名单里放的正是 `Path::parent()` 对该 URL 真会产出的那个值——这样
/// 一旦 `://` 短路被删掉，本例就会返回那个假路径而变红。若随便填一个对不上的
/// 字符串，`is_dir` 恒 false 会让它无论有没有短路都绿，成为典型的假绿用例。
#[test]
fn url_target_does_not_go_through_path_parent() {
    const URL: &str = "https://www.zdic.net/hans/x";
    let bogus_parent = Path::new(URL)
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (dir, fb) =
        resolve_workdir_with(URL, "", move |p: &Path| p == Path::new(&bogus_parent), home);
    assert_eq!(dir, "C:/Users/me", "URL 不应被当成路径拆父目录");
    assert_eq!(fb, Some(WorkdirFallback::NoTargetDir));
}

/// 显式 cwd 优先于默认策略。
#[test]
fn explicit_cwd_wins_over_target_dir() {
    let (dir, fb) = resolve_workdir_with(
        "D:/Dict/dict.exe",
        "E:/Data",
        dirs(&["D:/Dict", "E:/Data"]),
        home,
    );
    assert_eq!(dir, "E:/Data");
    assert_eq!(fb, None);
}

/// 显式 cwd 写错：降级而非让整个启动失败（ShellExecuteW 收到不存在的目录会整体失败）。
#[test]
fn missing_explicit_cwd_degrades_with_reason() {
    let (dir, fb) = resolve_workdir_with("D:/Dict/dict.exe", "E:/typo", dirs(&["D:/Dict"]), home);
    assert_eq!(dir, "C:/Users/me");
    assert_eq!(fb, Some(WorkdirFallback::ExplicitMissing));
}

/// 目标目录不存在（词条写了个错路径）时同样退到中性目录，而不是把不存在的目录传下去。
#[test]
fn nonexistent_target_dir_is_not_passed_through() {
    let (dir, _) = resolve_workdir_with("D:/gone/x.exe", "", dirs(&[]), home);
    assert_eq!(dir, "C:/Users/me");
}

/// 连主目录都拿不到才返回空串——这是最后手段，且必须报告出来。
#[test]
fn empty_only_when_nothing_resolvable() {
    let (dir, fb) = resolve_workdir_with("notepad.exe", "", dirs(&[]), no_home);
    assert_eq!(dir, "");
    assert_eq!(fb, Some(WorkdirFallback::Unresolved));
}

/// 纯空白的 cwd 视同未指定（词条里 `cwd=" "` 是笔误，不该被当成合法目录）。
#[test]
fn blank_explicit_cwd_is_treated_as_unset() {
    let (dir, fb) = resolve_workdir_with("D:/Dict/d.exe", "   ", dirs(&["D:/Dict"]), home);
    assert_eq!(dir, "D:/Dict");
    assert_eq!(fb, None);
}
