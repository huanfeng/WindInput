//! `config.degradation`：把段级降级记录暴露给设置页。
//!
//! # 这条 RPC 存在的理由
//!
//! 不变量 6（`docs/design/data-custom-layer.md` §4）是「段级降级必须 WARN **且在 UI 可见**」。
//! P0 只做了日志与内部写盘闸，于是「可见」这一半只对**读日志的人**成立——普通用户看到的
//! 只是「我的按键设置怎么变回默认了」，而配置文件明明还在。这条 RPC 是那一半的兑现。
//!
//! # 为什么必须是集成测试（独立进程）
//!
//! 要让 `Config::load()` 真读到一份会降级的配置，就必须重定向用户目录
//! （`WIND_DATADIR_CONF`）与安装根（`WIND_INSTALL_ROOT`）。两者都经 OnceLock，同一进程
//! 只认第一次：放进 `#[cfg(test)]` 单测模块的话，同二进制里先跑的别的测试会把 OnceLock
//! 定死、重定向**静默失效**，本测试转而去写用户真实的 `%APPDATA%\WindInput\config.toml`。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use wind_ipc::rpc::Request;
use wind_rpc::{CoreRpc, DispatchState, dispatch};

struct StubCore;

impl CoreRpc for StubCore {
    fn is_chinese_mode(&self) -> bool {
        true
    }
    fn active_schema_id(&self) -> String {
        "wubi86".into()
    }
}

fn write_at(dir: &Path, rel: &str, body: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

fn call(state: &DispatchState, method: &str) -> Result<Value, String> {
    let r = dispatch(
        state,
        Request {
            version: 1,
            id: 1,
            method: method.to_string(),
            params: json!({}),
        },
    );
    match (r.result, r.error) {
        (Some(v), None) => Ok(v),
        (_, Some(e)) => Err(e),
        _ => panic!("既无 result 也无 error"),
    }
}

/// 毒：`ui.font.scripts` 是 `HashMap<String, String>`，值给整型。
/// 同段另有一个健康子表 `ui.candidate`，用来钉住「降级只降到 `ui.font`」。
const POISONED_USER: &str = "[ui.candidate]\nper_page = 9\n\n[ui.font]\nscripts = { latin = 42 }\n";

#[test]
fn degradation_is_readable_over_rpc_with_dotted_paths() {
    let tmp = std::env::temp_dir().join(format!(
        "wind_rpc_config_degradation_{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();
    write_at(&data, "config.toml", "[schema]\nactive = \"wubi86\"\n");

    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(
        wind_config::Config::user_config_dir(),
        Some(user.clone()),
        "前置条件：用户目录须已重定向，否则本测试会读写真实 %APPDATA%"
    );

    let file = write_at(&user, "config.toml", POISONED_USER);
    let state = DispatchState::new(Arc::new(StubCore), "dev").expect("DispatchState");

    // ── 降级态 ───────────────────────────────────────────────────────────────

    let d = call(&state, "config.degradation").expect("★ config.degradation 必须存在且成功");
    assert_eq!(d["degraded"], json!(true), "{d}");
    assert_eq!(
        d["sections"],
        json!(["ui.font"]),
        "★ 点分路径必须**原样**传出：在边界上截成顶层段名（`ui`）会让用户以为整个界面\
         设置都回了默认，而实际只有字体那一格\n{d}"
    );
    assert_eq!(
        d["totalFallback"],
        json!(false),
        "定位得到有毒段时不是整份回落\n{d}"
    );

    // 与现实对上：同段另一个子表的用户值确实还在（否则上面报的路径就是错的）。
    let cfg = call(&state, "config.get").expect("config.get");
    assert_eq!(
        cfg.pointer("/ui/candidate/per_page"),
        Some(&json!(9)),
        "前置：降级只该降到 ui.font，ui.candidate 的用户值必须保留\n{cfg}"
    );

    // ── 正常态：同一进程里改回干净配置再问一次 ────────────────────────────────
    //
    // 没有这一步，「degraded=true」与「这个方法恒返回 true」无从区分。

    std::fs::write(&file, "[ui.candidate]\nper_page = 9\n").unwrap();
    let d = call(&state, "config.degradation").expect("config.degradation");
    assert_eq!(d["degraded"], json!(false), "毒清除后须回正常态\n{d}");
    assert_eq!(d["sections"], json!([]), "{d}");
    assert_eq!(d["totalFallback"], json!(false), "{d}");
    // 四个字段恒在（不是「没降级就不给」）：设置端据此可以无条件渲染，
    // 「字段缺失」在跨仓契约里与「这版 core 还没实现」无从区分。
    for k in ["degraded", "sections", "totalFallback", "unparsable"] {
        assert!(d.get(k).is_some(), "字段 {k} 须恒在\n{d}");
    }
    assert_eq!(
        d["unparsable"],
        json!([]),
        "健康态下语法故障列表须为空\n{d}"
    );

    // ── ★★ 钉住一条**别处依赖的前提**：降级粒度对段内的标量/列表键同样成立 ──────
    //
    // `wind-webdata` 的 `keys_overview` 判断「按键总览的全局层可不可信」时，靠的是一张
    // 写死的来源路径表（`keys.page_keys` 等四个折算来源都在里面）。那张表存在的**唯一
    // 理由**就是这条事实：`narrow_bad_section` 对坏段的每个直接子键都做探针，不分子表
    // 还是标量/数组，于是 `page_keys` 出问题时记的是 `keys.page_keys` 而**不是** `keys`。
    //
    // 这条曾被想当然地写反过（「标量键定位不到子表、会整段记 keys」），据此推出「祖先
    // 判据照样成立、不必列举来源」，于是那四个来源整片失去覆盖。把前提钉在这里，是因为
    // 它住在另一个 crate 里：哪天降级粒度改成「标量键退回整段」，本用例先红，人才会想到
    // 去回看那张表还对不对——否则 webdata 那侧只会静默地多判几次不可信（无害），或在反
    // 方向的改动里静默地少判（有害且无声）。
    std::fs::write(&file, "[keys]\npage_keys = 5\n").unwrap();
    let d = call(&state, "config.degradation").expect("config.degradation");
    assert_eq!(
        d["sections"],
        json!(["keys.page_keys"]),
        "★ 段内**标量/列表**键出问题时，降级须定位到该键（`keys.page_keys`），\
         而不是退回整段（`keys`）——`wind-webdata::keys_overview` 的来源路径表依赖这条\n{d}"
    );
    assert_eq!(d["totalFallback"], json!(false), "{d}");

    // ── ★★ 语法故障：与段级降级是**两个独立维度**，同一个 RPC 里各占一格 ─────────
    //
    // 两者出错位置不同：段级降级在四层合并**之后**的类型检查，语法故障在合并**之前**的
    // 单文件解析。此前 core 只有前一个维度，于是「文件里重复写了一个键」这种事
    // `degraded` 恒为 false——设置端、写盘闸、CLI 一致判定「一切正常」，而那正是
    // 用户配置被后台整表覆盖的时刻。这条用例钉住第二个维度确实到得了边界。
    std::fs::write(&file, "[ui.candidate]\nper_page = 9\nper_page = 5\n").unwrap();
    let d = call(&state, "config.degradation").expect("config.degradation");
    assert_eq!(
        d["degraded"],
        json!(true),
        "★ 语法错误必须让 degraded 为真——它曾恒为 false，那是真机数据丢失的起点\n{d}"
    );
    assert_eq!(
        d["sections"],
        json!([]),
        "语法故障不该伪装成段级降级：两者修法不同（改那一行 vs 改那个键的类型）\n{d}"
    );
    let u = &d["unparsable"][0];
    assert_eq!(u["layer"], json!("user"), "{d}");
    assert_eq!(
        u["skippedLines"],
        json!([3]),
        "行号须是 1-based 的原始行号——用户拿它直接跳到编辑器那一行\n{d}"
    );
    assert!(
        u["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("config.toml")),
        "要说清是哪个文件\n{d}"
    );
    assert!(
        u["error"].as_str().is_some_and(|e| e.contains("per_page")),
        "原始错误要带上出问题的键名\n{d}"
    );

    // 容错不是摆设：坏行之外的内容仍要正常加载。
    let cfg = call(&state, "config.get").expect("config.get");
    assert_eq!(
        cfg.pointer("/ui/candidate/per_page"),
        Some(&json!(9)),
        "被跳过的是第二次声明，首次声明的值须留下\n{cfg}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
