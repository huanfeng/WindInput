use super::*;
use crate::context::MemoryContext;
use crate::services::{Services, UrlOpener};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordOpener(Mutex<Vec<String>>);
impl UrlOpener for RecordOpener {
    fn open(&self, target: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(target.to_string());
        Ok(())
    }
}

/// 一次 `wind.cli` 调用：argv / toast 模式 / 成功文案 / 等待上限。
type CliCall = (Vec<String>, String, String, u64);
/// 一次 `ui.toast` 调用：文案 / kind / color / 位置 / 时长。
type ToastCall = (String, String, String, String, u64);

/// 记录 `wind.cli` 收到的 argv 与反馈策略。
#[derive(Default)]
struct RecSelf(Mutex<Vec<CliCall>>);
impl crate::services::ProcessRunner for RecSelf {
    fn run(&self, _spec: &crate::services::ProcSpawn<'_>) -> anyhow::Result<()> {
        unreachable!()
    }
    fn shell(&self, _cmdline: &str, _flags: &[String], _cwd: &str) -> anyhow::Result<()> {
        unreachable!()
    }
    fn run_self(&self, spec: &crate::services::CliSpawn<'_>) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((
            spec.args.to_vec(),
            spec.toast.to_string(),
            spec.ok_text.to_string(),
            spec.wait_ms,
        ));
        Ok(())
    }
}

/// 记录 `ui.toast` 收到的完整 spec。
#[derive(Default)]
struct RecToast(Mutex<Vec<ToastCall>>);
impl crate::services::NotifyService for RecToast {
    fn toast(&self, spec: &crate::services::ToastSpec<'_>) -> anyhow::Result<()> {
        self.0.lock().unwrap().push((
            spec.text.to_string(),
            spec.kind.to_string(),
            spec.color.to_string(),
            spec.position.to_string(),
            spec.duration_ms,
        ));
        Ok(())
    }
}

#[test]
fn open_dispatches_to_service() {
    let rec = Arc::new(RecordOpener::default());
    let mut svc = Services::new();
    svc.open = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);
    fn_open(&ctx, &["https://x".into()]).unwrap();
    assert_eq!(rec.0.lock().unwrap().as_slice(), &["https://x".to_string()]);
}

#[test]
fn search_composes_url() {
    let rec = Arc::new(RecordOpener::default());
    let mut svc = Services::new();
    svc.open = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);
    fn_search(&ctx, &["baidu".into(), "a b".into()]).unwrap();
    assert_eq!(rec.0.lock().unwrap()[0], "https://www.baidu.com/s?wd=a+b");
}

/// 空查询词不开浏览器，报错。
///
/// 开一个空搜索页的话，用户分不清是取值没成、引擎名写错还是命令没执行。
#[test]
fn search_refuses_an_empty_query_instead_of_opening_a_blank_page() {
    let rec = Arc::new(RecordOpener::default());
    let mut svc = Services::new();
    svc.open = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);
    // 纯空白同样算空：取值函数返回的常常是一个换行或若干空格。
    for q in ["", "   ", "\n"] {
        assert!(
            fn_search(&ctx, &["baidu".into(), q.into()]).is_err(),
            "空查询 {q:?} 应当报错"
        );
    }
    assert!(
        rec.0.lock().unwrap().is_empty(),
        "报错的同时绝不能已经把浏览器开出去了"
    );
}

/// 记录 `ProcessRunner` 收到的全部字段，供两个测试共用。
/// 每个字段都拼进字符串，漏传任何选项都会体现在断言串上。
#[derive(Default)]
struct RecProc(Mutex<Vec<String>>);
impl crate::services::ProcessRunner for RecProc {
    fn run(&self, spec: &crate::services::ProcSpawn<'_>) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(format!(
            "run:{}:{}:cwd={}:verb={}:show={}",
            spec.cmd,
            spec.args.join(","),
            spec.cwd,
            spec.verb,
            spec.show
        ));
        Ok(())
    }
    fn shell(&self, cmdline: &str, flags: &[String], cwd: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("shell:{cmdline}:{}:cwd={cwd}", flags.join("|")));
        Ok(())
    }
}

#[test]
fn proc_run_dispatches_cmd_and_args() {
    let rec = Arc::new(RecProc::default());
    let mut svc = Services::new();
    svc.proc = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);
    fn_run(&ctx, &["notepad.exe".into(), "a.txt".into()]).unwrap();
    fn_shell(&ctx, &["echo hi".into()]).unwrap();
    fn_shell(&ctx, &["echo hi".into(), "term,pwsh".into()]).unwrap();
    // 具名参数直达服务层（不写则为空串 = 交宿主定默认）。
    fn_run_named(
        &ctx,
        &["dict.exe".into()],
        &[
            ("cwd".into(), "D:/Dict".into()),
            ("verb".into(), "runas".into()),
            ("show".into(), "min".into()),
        ],
    )
    .unwrap();
    fn_shell_named(
        &ctx,
        &["dict -q x".into()],
        &[("cwd".into(), "D:/Dict".into())],
    )
    .unwrap();
    let log = rec.0.lock().unwrap();
    assert_eq!(log[0], "run:notepad.exe:a.txt:cwd=:verb=:show=");
    assert_eq!(log[1], "shell:echo hi::cwd=");
    assert_eq!(log[2], "shell:echo hi:term|pwsh:cwd=");
    assert_eq!(log[3], "run:dict.exe::cwd=D:/Dict:verb=runas:show=min");
    assert_eq!(log[4], "shell:dict -q x::cwd=D:/Dict");
}

/// 枚举型具名参数的值要校验白名单，且报错要列出合法取值。
///
/// 不带 cfg(windows)：verb/show 只在 Windows 生效，但短语文件跨机器走，校验必须跨平台一致。
#[test]
fn proc_run_validates_enum_named_values_on_all_platforms() {
    let rec = Arc::new(RecProc::default());
    let mut svc = Services::new();
    svc.proc = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);

    let err = fn_run_named(&ctx, &["x.exe".into()], &[("verb".into(), "RUNAS".into())])
        .expect_err("大小写不同的动词不应被接受");
    assert!(err.to_string().contains("runas"), "{err}");
    let err = fn_run_named(&ctx, &["x.exe".into()], &[("show".into(), "tiny".into())])
        .expect_err("未知窗口状态应报错");
    assert!(err.to_string().contains("normal"), "{err}");

    // 合法值全都放行
    for v in RUN_VERBS {
        fn_run_named(&ctx, &["x.exe".into()], &[("verb".into(), (*v).into())]).unwrap();
    }
    for s in RUN_SHOWS {
        fn_run_named(&ctx, &["x.exe".into()], &[("show".into(), (*s).into())]).unwrap();
    }

    // 非法值必须在调用服务之前挡下：上面两次报错不该留下任何启动记录，
    // 否则就成了"报了错但程序已经用错参数启动了"。
    assert_eq!(
        rec.0.lock().unwrap().len(),
        RUN_VERBS.len() + RUN_SHOWS.len()
    );
}

/// 按程序名给出预设结果的假 runner，记录尝试顺序。未登记的名字 = 不存在。
///
/// 「不存在」给真的 `io::Error(NotFound)`，与 `run_native` 里 `Command::spawn()?` 同形，
/// 用字符串冒充就测不到 `is_not_found`。
struct ScriptedProc {
    ok: Vec<&'static str>,
    denied: Vec<&'static str>,
    tried: Mutex<Vec<String>>,
}
impl ScriptedProc {
    fn new(ok: &[&'static str], denied: &[&'static str]) -> Self {
        ScriptedProc {
            ok: ok.to_vec(),
            denied: denied.to_vec(),
            tried: Mutex::new(Vec::new()),
        }
    }
    fn tried(&self) -> Vec<String> {
        self.tried.lock().unwrap().clone()
    }
}
impl crate::services::ProcessRunner for ScriptedProc {
    fn run(&self, spec: &crate::services::ProcSpawn<'_>) -> anyhow::Result<()> {
        self.tried.lock().unwrap().push(spec.cmd.to_string());
        assert!(spec.args.is_empty(), "proc.any 不带参数");
        if self.ok.contains(&spec.cmd) {
            return Ok(());
        }
        let kind = if self.denied.contains(&spec.cmd) {
            std::io::ErrorKind::PermissionDenied
        } else {
            std::io::ErrorKind::NotFound
        };
        Err(std::io::Error::from(kind))?
    }
    fn shell(&self, _cmdline: &str, _flags: &[String], _cwd: &str) -> anyhow::Result<()> {
        unreachable!()
    }
}

fn any_ctx(p: &Arc<ScriptedProc>) -> MemoryContext {
    let mut svc = Services::new();
    svc.proc = Some(p.clone());
    MemoryContext::new().with_services(svc)
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// 按顺序试，第一个成功的即停，后面的不再启动。
#[test]
fn proc_any_falls_back_in_order_and_stops_at_first_success() {
    let p = Arc::new(ScriptedProc::new(&["b", "c"], &[]));
    fn_any(&any_ctx(&p), &names(&["a", "b", "c"])).unwrap();
    assert_eq!(p.tried(), ["a", "b"]);
}

/// 第一个就能起来时只启动它。
#[test]
fn proc_any_first_candidate_wins() {
    let p = Arc::new(ScriptedProc::new(&["a", "b"], &[]));
    fn_any(&any_ctx(&p), &names(&["a", "b"])).unwrap();
    assert_eq!(p.tried(), ["a"]);
}

/// 「存在但起不来」同样回落到下一个——换一个能用的正是这个函数的目的。
#[test]
fn proc_any_falls_back_past_a_candidate_that_fails_to_spawn() {
    let p = Arc::new(ScriptedProc::new(&["c"], &["b"]));
    fn_any(&any_ctx(&p), &names(&["a", "b", "c"])).unwrap();
    assert_eq!(p.tried(), ["a", "b", "c"]);
}

/// 全部失败：报错里列出每个试过的名字，并把「没装」与「起不来」分开说。
#[test]
fn proc_any_all_fail_lists_every_candidate_by_reason() {
    let p = Arc::new(ScriptedProc::new(&[], &["b"]));
    let err = fn_any(&any_ctx(&p), &names(&["a", "b", "c"]))
        .expect_err("全部失败应报错")
        .to_string();
    assert_eq!(p.tried(), ["a", "b", "c"]);
    assert!(err.contains("未找到 a / c"), "{err}");
    assert!(err.contains("启动失败 b（"), "{err}");
    assert!(!err.contains("未找到 a / b"), "{err}");
}

/// 只有「没装」时不出现空的「启动失败」段。
#[test]
fn proc_any_all_missing_mentions_only_missing() {
    let p = Arc::new(ScriptedProc::new(&[], &[]));
    let err = fn_any(&any_ctx(&p), &names(&["gnome-calculator", "kcalc"]))
        .expect_err("全部失败应报错")
        .to_string();
    assert!(err.contains("未找到 gnome-calculator / kcalc"), "{err}");
    assert!(!err.contains("启动失败"), "{err}");
}

/// 空名在启动任何程序之前挡下，而不是先开了前面的再报错。
#[test]
fn proc_any_rejects_blank_names_before_spawning() {
    let p = Arc::new(ScriptedProc::new(&["a"], &[]));
    let ctx = any_ctx(&p);
    for bad in [&["a", ""][..], &["a", "  "], &[""]] {
        let err = fn_any(&ctx, &names(bad)).expect_err("空名应报错");
        assert!(err.to_string().contains("不能为空"), "{err}");
    }
    assert!(p.tried().is_empty(), "报错前不该启动任何程序");
}

/// 名字两端的空白去掉再交给宿主（`"kcalc "` 这种手误不该变成「未找到」）。
#[test]
fn proc_any_trims_names() {
    let p = Arc::new(ScriptedProc::new(&["kcalc"], &[]));
    fn_any(&any_ctx(&p), &names(&[" kcalc "])).unwrap();
    assert_eq!(p.tried(), ["kcalc"]);
}

#[test]
fn proc_any_without_runner_is_service_unavailable() {
    let ctx = MemoryContext::new().with_services(Services::new());
    assert!(matches!(
        fn_any(&ctx, &names(&["a"])),
        Err(CmdbarError::ServiceUnavailable { .. })
    ));
}

/// 宿主给 io 错误加了上下文也认得出「不存在」。
#[test]
fn not_found_is_detected_through_context() {
    let e = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::NotFound))
        .context("spawn kcalc");
    assert!(is_not_found(&e));
    assert!(!is_not_found(&anyhow::anyhow!("NotFound")));
    assert!(!is_not_found(&anyhow::Error::from(std::io::Error::from(
        std::io::ErrorKind::PermissionDenied
    ))));
}

#[test]
fn wind_cli_splits_single_arg_and_passes_multi_verbatim() {
    let rec = Arc::new(RecSelf::default());
    let mut svc = Services::new();
    svc.proc = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);
    // 单参：按空白拆分
    fn_wind_cli(&ctx, &["schema dict disable wubi86 fl".into()]).unwrap();
    // 多参：原样传递（路径含空格不被拆散）
    fn_wind_cli(
        &ctx,
        &[
            "backup".into(),
            "create".into(),
            "D:/我的 备份/a.zip".into(),
        ],
    )
    .unwrap();
    // 空白单参：报错
    assert!(fn_wind_cli(&ctx, &["   ".into()]).is_err());
    let log = rec.0.lock().unwrap();
    assert_eq!(log[0].0, vec!["schema", "dict", "disable", "wubi86", "fl"]);
    assert_eq!(log[1].0, vec!["backup", "create", "D:/我的 备份/a.zip"]);
    // 不给具名参数时的默认：提示开着（空串=on）、无自定义文案、等待上限走默认。
    // 默认必须是「开」——这个功能存在的理由就是直通命令此前没有任何反馈。
    assert_eq!(log[0].1, "");
    assert_eq!(log[0].3, crate::services::DEFAULT_CLI_WAIT_MS);
}

/// `wind.cli` 的三个具名参数原样落到 spec，且非法值在调用服务前挡下。
///
/// 否则 toast 写错一个字母就静默变成不提示。
#[test]
fn wind_cli_named_feedback_params_reach_host_and_validate() {
    let rec = Arc::new(RecSelf::default());
    let mut svc = Services::new();
    svc.proc = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);

    fn_wind_cli_named(
        &ctx,
        &["dict import flypy x.yaml".into()],
        &[
            ("toast".into(), "error".into()),
            ("ok".into(), "词库已更新".into()),
            ("wait".into(), "30000".into()),
        ],
    )
    .unwrap();

    let err = fn_wind_cli_named(&ctx, &["restart".into()], &[("toast".into(), "no".into())])
        .expect_err("未知 toast 模式应报错");
    assert!(err.to_string().contains("off"), "{err}");
    let err = fn_wind_cli_named(&ctx, &["restart".into()], &[("wait".into(), "5秒".into())])
        .expect_err("非数字的 wait 应报错");
    assert!(err.to_string().contains("毫秒"), "{err}");

    let log = rec.0.lock().unwrap();
    assert_eq!(log.len(), 1, "两次非法调用都不该真的启动子进程");
    assert_eq!(log[0].1, "error");
    assert_eq!(log[0].2, "词库已更新");
    assert_eq!(log[0].3, 30_000);
}

/// `ui.toast` 的具名参数与顺序无关，且四类非法写法都当场报错。
#[test]
fn ui_toast_named_params_are_order_free_and_validated() {
    let rec = Arc::new(RecToast::default());
    let mut svc = Services::new();
    svc.notify = Some(rec.clone());
    let ctx = MemoryContext::new().with_services(svc);

    // 顺序颠倒不影响落位
    fn_ui_toast_named(
        &ctx,
        &["导入完成".into()],
        &[
            ("ms".into(), "5000".into()),
            ("pos".into(), "top_right".into()),
            ("kind".into(), "success".into()),
        ],
    )
    .unwrap();
    // 自定义色（与 kind 互斥，此处单给）
    fn_ui_toast_named(
        &ctx,
        &["完成".into()],
        &[("color".into(), "#52C41A".into())],
    )
    .unwrap();

    for bad in [
        vec![("kind".into(), "warn".into())],
        vec![("pos".into(), "right".into())],
        vec![("color".into(), "52C41A".into())],
        vec![("ms".into(), "一会儿".into())],
        // kind 与 color 都给：不猜哪个赢
        vec![
            ("kind".into(), "success".into()),
            ("color".into(), "#52C41A".into()),
        ],
    ] {
        assert!(
            fn_ui_toast_named(&ctx, &["x".into()], &bad).is_err(),
            "{bad:?} 应报错"
        );
    }

    let log = rec.0.lock().unwrap();
    assert_eq!(log.len(), 2, "非法写法一条都不该弹出去");
    assert_eq!(
        log[0],
        (
            "导入完成".into(),
            "success".into(),
            String::new(),
            "top_right".into(),
            5000
        )
    );
    assert_eq!(log[1].2, "#52C41A");
    // ms 省略 = 0 = 交给宿主取默认，而不是在这里硬写一个 2500。
    assert_eq!(log[1].4, 0);
}

#[test]
fn missing_service_errors() {
    let ctx = MemoryContext::new().with_services(Services::new());
    assert!(matches!(
        fn_open(&ctx, &["x".into()]),
        Err(CmdbarError::ServiceUnavailable { .. })
    ));
    // 完全无 services
    let bare = MemoryContext::new();
    assert!(matches!(
        fn_open(&bare, &["x".into()]),
        Err(CmdbarError::ServiceUnavailable { .. })
    ));
}
