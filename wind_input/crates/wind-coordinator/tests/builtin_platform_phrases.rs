//! 出厂短语的**按平台取舍**与 Linux 上 `proc.any` 词条的端到端守门。
//!
//! `system.phrases.toml` 里打开记事本 / 计算器 / 主目录这几条按平台各写一份
//! （`platform = 'windows' / 'darwin' / 'linux'`）。过滤曾经写死只收 `windows`，于是
//! Linux 与 macOS 上出的全是 `notepad.exe` / `calc.exe`——选中必然失败，而 darwin 那几条
//! 从未生效过。
//!
//! ⚠️ 短语读的是**仓库** `data/system.phrases.toml`，不是 `build_dev/data/` 那份部署产物
//! （理由同 `builtin_reverse_phrase.rs`）；端到端那条（仅 `linux-host` 形态编译）另需
//! `build_dev/data` 的五笔方案，缺失时带着备齐办法失败，不静默跳过。

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn repo_phrases() -> PathBuf {
    repo_root().join("data/system.phrases.toml")
}

fn entries() -> Vec<wind_phrase::SystemPhraseEntry> {
    let p = repo_phrases();
    assert!(p.is_file(), "找不到出厂短语文件：{}", p.display());
    let v = wind_phrase::PhraseLayer::parse_system_entries(&p);
    assert!(
        !v.is_empty(),
        "出厂短语解析出 0 条——解析失败会让下面的断言全部空过"
    );
    v
}

fn texts_of<'a>(v: &'a [wind_phrase::SystemPhraseEntry], code: &str) -> Vec<&'a str> {
    v.iter()
        .filter(|e| e.code == code)
        .map(|e| e.text.as_str())
        .collect()
}

fn entries_for(platform: &str) -> Vec<wind_phrase::SystemPhraseEntry> {
    let v = wind_phrase::PhraseLayer::parse_system_entries_for(&repo_phrases(), platform);
    assert!(!v.is_empty(), "{platform}: 出厂短语解析出 0 条");
    v
}

const PLATFORMS: [&str; 3] = ["windows", "darwin", "linux"];

/// 本机编出来的加载入口与「按本机平台名显式过滤」给出同一份条目。
#[test]
fn host_loader_equals_explicit_platform_filter() {
    let host = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let a: Vec<_> = entries().into_iter().map(|e| (e.code, e.text)).collect();
    let b: Vec<_> = entries_for(host)
        .into_iter()
        .map(|e| (e.code, e.text))
        .collect();
    assert_eq!(a, b);
}

/// 只有 Windows 看得到 `.exe` / `USERPROFILE` 词条。
#[test]
fn windows_programs_only_reach_windows() {
    for plat in PLATFORMS {
        let bad: Vec<_> = entries_for(plat)
            .into_iter()
            .filter(|e| e.text.contains(".exe") || e.text.contains("USERPROFILE"))
            .map(|e| format!("{} = {}", e.code, e.text))
            .collect();
        if plat == "windows" {
            assert!(!bad.is_empty(), "Windows 上应仍有 notepad.exe 等词条");
        } else {
            assert!(bad.is_empty(), "{plat} 出现了 Windows 专属词条：{bad:?}");
        }
    }
}

/// 编辑器 / 计算器 / 主目录 / 删行这四个码在每个平台上都恰好一条，且是该平台的写法。
#[test]
fn launcher_codes_resolve_to_exactly_one_entry_per_platform() {
    for plat in PLATFORMS {
        let v = entries_for(plat);
        for code in ["cono", "coca", "cohm", "codl"] {
            let t = texts_of(&v, code);
            assert_eq!(t.len(), 1, "{code} 在 {plat} 应恰好一条，实际 {t:?}");
            let t = t[0];
            let ok = match (plat, code) {
                (_, "codl") => t.contains("key.seq("),
                ("windows", _) => t.contains(".exe") || t.contains("USERPROFILE"),
                ("darwin", _) => t.contains("\"-a\"") || t.contains("HOME"),
                _ => t.contains("proc.any(") || t.contains("HOME"),
            };
            assert!(ok, "{plat} 的 {code} 写法不对：{t}");
        }
    }
}

/// 改动前的过滤规则（写死只收空 / all / windows），原样抄在这里当对拍基准。
fn old_filter_accepts(platform: Option<&str>) -> bool {
    match platform {
        None => true,
        Some(p) => {
            let p = p.to_lowercase();
            // `desktop`：原本没写平台的电脑专用条目（打开网页、读写剪贴板……）后来补标的，
            // 只是让手机看不见它们；在 Windows 上与改标前一样都收
            p.is_empty() || p == "all" || p == "windows" || p == "desktop"
        }
    }
}

#[derive(serde::Deserialize)]
struct RawFile {
    phrases: Vec<RawPhrase>,
}

#[derive(serde::Deserialize)]
struct RawPhrase {
    code: String,
    text: String,
    #[serde(default)]
    weight: Option<i32>,
    #[serde(default)]
    position: Option<i32>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn raw_entries() -> Vec<RawPhrase> {
    let text = std::fs::read_to_string(repo_phrases()).unwrap();
    toml::from_str::<RawFile>(&text).unwrap().phrases
}

/// ★ Windows 上的条目集与改动前**逐条一致**（含顺序、权重、位置、分类）。
///
/// 基准是旧过滤规则作用在同一份文件上：新加的 linux 条目在 Windows 上必须全被滤掉，
/// 旧规则收的每一条新过滤也必须收。另在提交说明里记了一次与改动前那份文件的对拍。
#[test]
fn windows_entries_are_identical_to_the_old_filter() {
    let old: Vec<_> = raw_entries()
        .into_iter()
        .filter(|r| old_filter_accepts(r.platform.as_deref()))
        .map(|r| {
            (
                r.code,
                r.text,
                r.weight.unwrap_or(1000),
                r.position.unwrap_or(0),
                r.category.unwrap_or_default(),
            )
        })
        .collect();
    let new: Vec<_> = entries_for("windows")
        .into_iter()
        .map(|e| (e.code, e.text, e.weight, e.position, e.category))
        .collect();
    assert_eq!(new, old);
}

/// 出厂文件里**每个平台**的每条 `$CC` 都只调用已注册的函数、参数个数合法、具名参数在
/// 白名单内。
///
/// darwin 条目此前从未在任何平台被加载过，这是它们第一次被检查；Linux / macOS 的
/// 条目本机选不中（或选中也不跑），写错函数名的话用户侧只表现为「选了没反应」。
#[test]
fn every_platform_entry_calls_only_known_functions_with_valid_arity() {
    let reg = wind_cmdbar::Registry::full();
    let mut problems = Vec::new();
    let mut checked = 0;
    for r in raw_entries() {
        if !wind_cmdbar::is_cmdbar_grammar(&r.text) {
            continue;
        }
        let phrase = match wind_cmdbar::parse(&r.text) {
            Ok(p) => p,
            Err(e) => {
                problems.push(format!("{} [{:?}] 解析失败：{e}", r.code, r.platform));
                continue;
            }
        };
        checked += 1;
        let mut calls = Vec::new();
        collect_calls_phrase(&phrase, &mut calls);
        for (name, n, named) in calls {
            match reg.lookup(&name) {
                None => problems.push(format!("{} [{:?}] 未知函数 {name}", r.code, r.platform)),
                Some(spec) => {
                    if !spec.accepts(n) {
                        problems.push(format!(
                            "{} [{:?}] {name} 参数个数 {n} 不合法",
                            r.code, r.platform
                        ));
                    }
                    for k in named {
                        if !spec.accepts_named(&k) {
                            problems.push(format!(
                                "{} [{:?}] {name} 不认具名参数 {k}",
                                r.code, r.platform
                            ));
                        }
                    }
                }
            }
        }
        let hints = wind_cmdbar::lint_parsed(&phrase);
        if !hints.is_empty() {
            problems.push(format!("{} [{:?}] lint：{hints:?}", r.code, r.platform));
        }
    }
    assert!(checked > 10, "只检查了 {checked} 条 $CC——解析路径可能空转");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

type Call = (String, usize, Vec<String>);

fn collect_calls_phrase(p: &wind_cmdbar::Phrase, out: &mut Vec<Call>) {
    use wind_cmdbar::Phrase;
    match p {
        Phrase::Literal(_) => {}
        Phrase::Template(e) => collect_calls(e, out),
        Phrase::Command(c) => collect_calls_command(c, out),
        Phrase::Array(a) => a.elements.iter().for_each(|e| collect_calls(e, out)),
    }
}

fn collect_calls_command(c: &wind_cmdbar::CommandPhrase, out: &mut Vec<Call>) {
    collect_calls(&c.display, out);
    c.actions.iter().for_each(|a| collect_calls(a, out));
}

fn collect_calls(e: &wind_cmdbar::Expr, out: &mut Vec<Call>) {
    use wind_cmdbar::Expr;
    use wind_cmdbar::ast::StringPart;
    match e {
        Expr::StringLit(parts) => {
            for p in parts {
                if let StringPart::Interp(inner) = p {
                    collect_calls(inner, out);
                }
            }
        }
        Expr::Ident(name) => {
            // `type` 由 eval 拦截，不在注册表里。
            if name != "type" {
                out.push((name.clone(), 0, Vec::new()));
            }
        }
        Expr::Call { name, args, named } => {
            if name != "type" {
                out.push((
                    name.clone(),
                    args.len(),
                    named.iter().map(|(k, _)| k.clone()).collect(),
                ));
            }
            args.iter().for_each(|a| collect_calls(a, out));
            named.iter().for_each(|(_, v)| collect_calls(v, out));
        }
        Expr::Command(c) => collect_calls_command(c, out),
        _ => {}
    }
}

/// Linux 词条能被命令直通车解析，且 `proc.any` 的每个候选都是裸程序名（不带路径、参数）。
#[cfg(target_os = "linux")]
#[test]
fn linux_launcher_phrases_parse_and_list_bare_program_names() {
    let v = entries();
    for code in ["cono", "coca"] {
        let t = texts_of(&v, code)[0];
        let reg = wind_cmdbar::Registry::full();
        let ctx = wind_cmdbar::MemoryContext::new();
        let ev = wind_cmdbar::evaluate_phrase(t, &ctx, &reg)
            .unwrap_or_else(|e| panic!("{code} 解析失败：{e}"));
        assert!(!ev.primary_display().is_empty(), "{code} 应有显示文本");
        let inner = t
            .split("proc.any(")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .unwrap_or_else(|| panic!("{code} 应使用 proc.any：{t}"));
        let names: Vec<&str> = inner
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .collect();
        assert!(names.len() >= 3, "{code} 候选太少：{names:?}");
        for n in &names {
            assert!(
                !n.is_empty() && !n.contains(['/', ' ', '.']),
                "{code} 的候选应是裸程序名：{n:?}"
            );
        }
    }
}

// ───────────────── 升级迁移：库里已有的系统短语跟着新过滤走 ─────────────────

fn to_store(v: &[wind_phrase::SystemPhraseEntry]) -> Vec<wind_store::phrases::SystemPhrase> {
    v.iter()
        .map(|e| wind_store::phrases::SystemPhrase {
            code: e.code.clone(),
            text: e.text.clone(),
            weight: e.weight,
            position: e.position,
            category: e.category.clone(),
            enabled: e.enabled,
        })
        .collect()
}

// 只被 cfg(not(windows)) 的升级重同步用例使用；Windows 目标上不加门会 dead_code（clippy -D warnings）
#[cfg(not(windows))]
fn sys_rows(store: &wind_store::Store) -> Vec<(String, String)> {
    let mut v: Vec<_> = store
        .list_system_phrases()
        .unwrap()
        .into_iter()
        .map(|r| (r.code, r.text))
        .collect();
    v.sort();
    v
}

#[cfg(not(windows))]
fn keys_of(v: &[wind_phrase::SystemPhraseEntry]) -> Vec<(String, String)> {
    let mut k: Vec<_> = v.iter().map(|e| (e.code.clone(), e.text.clone())).collect();
    k.sort();
    k
}

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wind_plat_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// ★ 非 Windows 用户升级：库里是旧过滤入库的 Windows 那一份（含 `notepad.exe`），
/// 协调器启动按新过滤重同步后，库里只剩本平台的条目，错平台的行一条不留。
///
/// 走的是生产路径（构造期的哈希比对 + `sync_system_phrases`），不是直接调 store。
#[cfg(not(windows))]
#[test]
fn upgrade_resyncs_stale_windows_rows_off_windows() {
    let root = temp_root("migrate");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::copy(repo_phrases(), data.join("system.phrases.toml")).unwrap();
    let store = std::sync::Arc::new(wind_store::Store::open(root.join("user.redb")).unwrap());

    // 旧版本留下的库：Windows 那一份 + 旧版本算的哈希（与新条目集的哈希必然不同）。
    let old = entries_for("windows");
    store.sync_system_phrases(&to_store(&old)).unwrap();
    store
        .set_phrase_sys_hash("hash-written-by-old-build")
        .unwrap();
    assert!(sys_rows(&store).iter().any(|(_, t)| t.contains(".exe")));

    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec![];
    let _coord =
        wind_coordinator::Coordinator::new_headless_with_store(cfg, Some(&data), store.clone());

    let rows = sys_rows(&store);
    assert_eq!(
        rows,
        keys_of(&entries()),
        "重同步后库里应恰好是本平台的条目集"
    );
    assert!(
        !rows
            .iter()
            .any(|(_, t)| t.contains(".exe") || t.contains("USERPROFILE")),
        "不得残留 Windows 条目：{rows:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// ★ Windows 用户升级：新过滤给出的条目集与旧库一致，重同步（无论是否触发）不增不删。
#[test]
fn windows_upgrade_keeps_every_row() {
    let root = temp_root("win_keep");
    let store = wind_store::Store::open(root.join("user.redb")).unwrap();
    let old: Vec<_> = raw_entries()
        .into_iter()
        .filter(|r| old_filter_accepts(r.platform.as_deref()))
        .map(|r| wind_store::phrases::SystemPhrase {
            code: r.code,
            text: r.text,
            weight: r.weight.unwrap_or(1000),
            position: r.position.unwrap_or(0),
            category: r.category.unwrap_or_default(),
            // 出厂开关取文件里写的（时间戳出厂关闭是有意的改动，不算「条目集变了」）
            enabled: r.enabled.unwrap_or(true),
        })
        .collect();
    store.sync_system_phrases(&old).unwrap();
    let before = store.list_system_phrases().unwrap();

    let new = entries_for("windows");
    let st = store.sync_system_phrases(&to_store(&new)).unwrap();
    // `updated` 计的是「已存在、按 TOML 刷新了一遍」的行，不代表内容有变；内容是否
    // 一致由下面整行比对（权重 / 位置 / 开关 / 分类）回答。
    assert_eq!((st.added, st.removed), (0, 0), "Windows 的条目集不该增删");
    assert_eq!(
        store.list_system_phrases().unwrap(),
        before,
        "Windows 的每一行都应原样保留"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// ───────────────── 端到端：打 coca → 选中 → 真的 spawn 到排在后面的那个 ─────────────────
//
// 只在 Linux 的 `linux-host` 形态（`ext_presenter`）下成立：那里 `proc.run` 在服务进程里
// 直接 spawn，能拿到「程序不存在」；默认形态（mock）与 Windows 一样把启动转给宿主，
// 恒报成功，回落无从谈起。

#[cfg(all(target_os = "linux", ext_presenter))]
mod e2e {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use wind_bridge::handler::{KeyEventData, MessageHandler};
    use wind_config::Config;
    use wind_coordinator::Coordinator;
    use wind_ipc::protocol::EVENT_KEY_DOWN;

    const VK_SPACE: u32 = 0x20;

    fn build_data() -> PathBuf {
        repo_root().join("build_dev/data")
    }

    fn key(vk: u32) -> KeyEventData {
        KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
        }
    }

    /// 以 build_dev/data 为底、短语换成仓库那份的数据目录（逐项符号链接，不拷词库）。
    fn data_with_repo_phrases(root: &Path) -> PathBuf {
        let d = root.join("data");
        std::fs::create_dir_all(&d).unwrap();
        for e in std::fs::read_dir(build_data()).unwrap().flatten() {
            if e.file_name() == "system.phrases.toml" {
                continue;
            }
            std::os::unix::fs::symlink(e.path(), d.join(e.file_name())).unwrap();
        }
        std::os::unix::fs::symlink(repo_phrases(), d.join("system.phrases.toml")).unwrap();
        d
    }

    /// 假程序：被启动时把自己的名字追加进 `log`。
    fn fake_program(bin: &Path, name: &str, log: &Path) {
        let p = bin.join(name);
        std::fs::write(
            &p,
            format!("#!/bin/sh\necho {name} >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// 子进程分支的标记：值是父进程备好的临时根目录。
    const CHILD_ROOT_ENV: &str = "WIND_TEST_PLAT_PHR_ROOT";
    const THIS_TEST: &str = "e2e::typing_coca_launches_the_first_installed_calculator";

    /// PATH 里只有排第三、第四的两个计算器：打 `coca` 选中后，应当越过前两个（未安装）
    /// 启动第三个，且**只**启动它。
    ///
    /// 用例本体在**子进程**里跑（本测试二进制以 `--exact` 重新执行自己）：要换掉的 PATH 只
    /// 设在子进程上，不碰本进程的全局环境——同一二进制里别的用例并行起协调器，进程内
    /// `set_var` 与它们的 `getenv` 是数据竞争。
    ///
    /// 变异判据：把 wind-phrase 的平台过滤改回只收 `windows`，候选变成 `calc.exe`、日志
    /// 为空转红；把 `proc.any` 的首个成功即停去掉，日志多出 mate-calc 转红。
    #[test]
    fn typing_coca_launches_the_first_installed_calculator() {
        if let Some(root) = std::env::var_os(CHILD_ROOT_ENV) {
            coca_in_child(Path::new(&root));
            return;
        }
        assert!(
            build_data().join("schemas/wubi86.schema.toml").is_file(),
            "本用例需要 {} 下的五笔方案与词库：在仓库根跑 `scripts/dev.sh gd` 生成，或从已有的\
             工作树整份拷来 build_dev/data（只拷一部分会让别的用例假绿）",
            build_data().display()
        );
        let root = std::env::temp_dir().join(format!("wind_plat_phr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // 出厂顺序：gnome-calculator, kcalc, deepin-calculator, mate-calc, …
        fake_program(&bin, "deepin-calculator", &root.join("launched.log"));
        fake_program(&bin, "mate-calc", &root.join("launched.log"));

        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([THIS_TEST, "--exact", "--test-threads=1", "--nocapture"])
            .env(CHILD_ROOT_ENV, &root)
            .env("PATH", &bin)
            .output()
            .unwrap();
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            out.status.success(),
            "子进程失败\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        );
        // 过滤串写错时子进程跑 0 条也是「成功」：确认它真的跑了这一条。
        assert!(
            stdout.contains("1 passed"),
            "子进程没有跑到本用例：\n{stdout}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 子进程里的用例本体：PATH 已由父进程设成只含假程序的 `root/bin`。
    fn coca_in_child(root: &Path) {
        let log = root.join("launched.log");
        let data = data_with_repo_phrases(root);
        let store = Arc::new(wind_store::Store::open(root.join("user.redb")).unwrap());
        let mut cfg = Config::default();
        cfg.schema.available = vec!["wubi86".into()];
        cfg.schema.active = "wubi86".into();
        cfg.input.default.chinese_mode = true;
        let coord = Coordinator::new_headless_with_store(cfg, Some(&data), store);

        for ch in "coca".chars() {
            coord.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
        }
        let texts = coord.debug_all_candidate_texts();
        // 四码唯一时可能已自动上屏执行；没有的话首选应是这条命令，空格选中。
        if !texts.is_empty() {
            assert_eq!(
                texts.first().map(String::as_str),
                Some("打开计算器"),
                "coca 首选应为 Linux 版计算器命令，实际 {texts:?}"
            );
            coord.handle_key_event(&key(VK_SPACE));
        }

        assert!(
            wait_for(|| std::fs::read_to_string(&log).is_ok_and(|s| !s.is_empty())),
            "选中后应启动一个计算器（日志为空）"
        );
        // 给「多启动一个」留出与正例同量级的时间窗，否则只证明了「还没发生」。
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "deepin-calculator\n",
            "应越过未安装的前两个、只启动第一个装了的"
        );
    }
}
