//! 按应用方案（compat.toml `[[apps]] schema`，C0-7 / C3-3，GH#80）端到端：焦点切入切换、
//! 手切分流、热重载交互、往返热键、菜单写盘。
//!
//! 走真实的焦点事件、菜单分派、按键与 `reload_user_config`，而手切会写 `schema.active`，
//! 故整棵目录树经**便携标记**重定向到临时目录（`WIND_INSTALL_ROOT` + `portable_mode`）：
//! 用户层 = `<root>/userdata`、本机状态 = `<root>/localdata`，一个进程只能重定向一次，
//! 故单开测试二进制；用例共用这棵树，用 `LOCK` 串行、每条开头重写 config.toml / compat.toml。
//!
//! ⚠️ 本用例**不依赖 `build_dev/data`**：三个码表方案 za / zb / zc 全部自造，不会静默跳过。
//! 焦点进程名经 `FocusData::bundle_id` 注入（Linux 下 `process_name` 恒空，这是 host 上
//! 唯一够得着 `pid_names` 的公开入口；macOS 生产路径也正是它）。
//!
//! headless 构造不预热，非活跃方案都是**冷**的 ⇒ 焦点切入走「后台加载、完成后再切」那条路，
//! 每次切入后 `debug_wait_app_schema_load` 等它落定。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use wind_bridge::handler::{FocusData, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};
use wind_ui_types::MenuCmd;

static LOCK: Mutex<()> = Mutex::new(());

/// 便携根目录（进程内只建一次）。
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        // ⚠️ 目录名带 pid：多 worktree / 多会话并行跑测试时固定名会互删夹具。
        let root = std::env::temp_dir().join(format!("wind_app_schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let schemas = root.join("data/schemas");
        for (id, name, word) in [("za", "甲方案", "甲"), ("zb", "乙方案", "乙"), ("zc", "丙方案", "丙")]
        {
            std::fs::create_dir_all(schemas.join(id)).unwrap();
            std::fs::write(
                schemas.join(format!("{id}.schema.toml")),
                format!(
                    "[schema]\nid = \"{id}\"\nname = \"{name}\"\n\
                     [engine]\ntype = \"codetable\"\n\
                     [engine.codetable]\nmax_code_length = 4\n\
                     [[dictionaries]]\nid = \"main\"\npath = \"{id}/{id}.dict.yaml\"\ndefault = true\n"
                ),
            )
            .unwrap();
            std::fs::write(
                schemas.join(format!("{id}/{id}.dict.yaml")),
                format!("---\nname: {id}\nversion: \"1\"\n...\n{word}\ta\n"),
            )
            .unwrap();
        }
        std::fs::write(root.join(wind_config::variant::PORTABLE_MARKER_NAME), "").unwrap();
        std::fs::create_dir_all(root.join("userdata")).unwrap();
        // SAFETY: 在任何 OnceLock（variant / 路径缓存）初始化之前设置；ROOT 的 get_or_init
        // 保证只跑一次，且用例都先经 `LOCK` 串行。
        unsafe {
            std::env::set_var("WIND_INSTALL_ROOT", &root);
        }
        assert!(wind_config::variant::is_portable(), "前置条件：便携标记须生效");
        assert_eq!(
            Config::user_config_dir(),
            Some(root.join("userdata")),
            "前置条件：用户目录须已重定向，否则本测试会读写真实用户配置"
        );
        root
    })
}

fn user() -> PathBuf {
    root().join("userdata")
}

/// 每条用例的起点：全局 za，三个方案全启用；compat 规则按参数重写。
fn reset(compat: &str, extra_config: &str) {
    std::fs::write(
        user().join("config.toml"),
        format!("[schema]\nactive = \"za\"\navailable = [\"za\", \"zb\", \"zc\"]\n{extra_config}"),
    )
    .unwrap();
    std::fs::write(user().join("compat.toml"), compat).unwrap();
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

/// 磁盘上的 `schema.active`（手切是否写盘的判据）。
fn disk_active() -> String {
    let text = std::fs::read_to_string(user().join("config.toml")).unwrap();
    let v: toml::Value = toml::from_str(&text).unwrap();
    v["schema"]["active"].as_str().unwrap().to_string()
}

/// 用户层 compat.toml 里该进程的 `schema`。
fn rule_schema(proc: &str) -> Option<String> {
    let text = std::fs::read_to_string(user().join("compat.toml")).ok()?;
    let v: toml::Value = toml::from_str(&text).unwrap();
    v.get("apps")?
        .as_array()?
        .iter()
        .find(|r| r.get("process").and_then(|p| p.as_str()) == Some(proc))?
        .get("schema")?
        .as_str()
        .map(str::to_string)
}

/// 焦点切入 `proc`（pid 不同即跨进程），并等可能的冷方案后台加载落定。
fn focus(c: &Coordinator, pid: u32, instance: u32, proc: &str) {
    c.handle_focus_gained(&FocusData {
        x: 100,
        y: 100,
        height: 20,
        composition_start_x: 0,
        composition_start_y: 0,
        client_token: (pid as u64) << 32 | instance as u64,
        input_scope_mask: 0,
        disabled: false,
        reason: 2,
        caret_source: 0,
        bundle_id: proc.to_string(),
        window_class: String::new(),
        window_title: String::new(),
    });
    c.debug_wait_app_schema_load();
}

/// 菜单「输入方案」第 i 项（available 序：za=0 zb=1 zc=2）。
fn menu_select(c: &Coordinator, i: usize) {
    c.debug_run_menu_cmd(MenuCmd::SchemaSelect(i));
}

const PLAIN: (u32, &str) = (1001, "plain.exe");

/// 固定 ↔ 无规则往返：切入固定应用换成规则方案，切回无规则应用回到全局；
/// 全程不写 `schema.active`、全局方案不动。
#[test]
fn fixed_rule_round_trip_keeps_global() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset("[[apps]]\nprocess = \"code1.exe\"\nschema = \"zb\"\n", "");
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(c.active_schema_id(), "za");

    focus(&c, 2001, 1, "code1.exe");
    assert_eq!(c.active_schema_id(), "zb", "切入固定应用应换成规则方案");
    assert_eq!(disk_active(), "za", "自动切换不得写 schema.active");

    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(c.active_schema_id(), "za", "切回无规则应用应回到全局方案");
    assert_eq!(c.debug_app_schema_state().0, "za", "全局方案不动");
    assert_eq!(disk_active(), "za");
}

/// 固定应用内手切是临时覆盖：不写 `schema.active`、不改全局；离开再回来恢复规则值。
#[test]
fn manual_switch_in_fixed_app_is_temporary() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset("[[apps]]\nprocess = \"code2.exe\"\nschema = \"zb\"\n", "");
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    focus(&c, 2002, 1, "code2.exe");
    assert_eq!(c.active_schema_id(), "zb");

    menu_select(&c, 2);
    assert_eq!(c.active_schema_id(), "zc", "手切照常生效");
    assert_eq!(disk_active(), "za", "规则应用内手切不得写 schema.active");
    assert_eq!(
        c.debug_app_schema_state().0,
        "za",
        "规则应用内手切不改全局方案"
    );

    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(c.active_schema_id(), "za");
    focus(&c, 2002, 1, "code2.exe");
    assert_eq!(
        c.active_schema_id(),
        "zb",
        "再切入恢复规则值，临时覆盖不留痕"
    );
}

/// `@remember`：手切—切走—切回恢复上次手切的方案；无记录时用全局。
#[test]
fn remember_app_restores_last_manual_choice() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(
        "[[apps]]\nprocess = \"chat3.exe\"\nschema = \"@remember\"\n",
        "",
    );
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    focus(&c, 3003, 1, "chat3.exe");
    assert_eq!(c.active_schema_id(), "za", "无记录 ⇒ 全局方案");

    menu_select(&c, 1);
    assert_eq!(c.active_schema_id(), "zb");
    assert_eq!(
        disk_active(),
        "za",
        "@remember 应用内手切不写 schema.active"
    );
    assert_eq!(
        c.debug_app_schema_state()
            .1
            .get("chat3.exe")
            .map(String::as_str),
        Some("zb"),
        "手切应记入记忆表"
    );

    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(c.active_schema_id(), "za", "切走回到全局");
    focus(&c, 3003, 1, "chat3.exe");
    assert_eq!(c.active_schema_id(), "zb", "切回恢复记忆的方案");
}

/// 回归：无规则应用里手切照旧写 `schema.active` 并成为新的全局方案——之后切进切出
/// 规则应用，回来的是这个新全局，而不是启动时的旧值（内存 config 不刷新）。
#[test]
fn manual_switch_in_plain_app_is_global() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset("[[apps]]\nprocess = \"code4.exe\"\nschema = \"zc\"\n", "");
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    menu_select(&c, 1);
    assert_eq!(c.active_schema_id(), "zb");
    assert_eq!(disk_active(), "zb", "无规则应用手切照旧写盘");
    assert_eq!(c.debug_app_schema_state().0, "zb");

    focus(&c, 2004, 1, "code4.exe");
    assert_eq!(c.active_schema_id(), "zc");
    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(c.active_schema_id(), "zb", "回来的是最近一次全局手切的结果");
}

/// 同进程内的焦点跳转（换 docMgr / 换实例）不重算：尊重用户在应用内的手切。
#[test]
fn same_process_focus_does_not_recompute() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset("[[apps]]\nprocess = \"code5.exe\"\nschema = \"zb\"\n", "");
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    focus(&c, 2005, 1, "code5.exe");
    assert_eq!(c.active_schema_id(), "zb");
    menu_select(&c, 2);
    focus(&c, 2005, 2, "code5.exe");
    assert_eq!(
        c.active_schema_id(),
        "zc",
        "同进程焦点跳转不得把手切拉回规则值"
    );
}

/// 非法值 / 不在 available 的 id 视为未配置：切入不切换，手切按全局处理（写盘）。
#[test]
fn invalid_rule_values_are_unconfigured() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(
        "[[apps]]\nprocess = \"bad6.exe\"\nschema = \"nope\"\n\n\
         [[apps]]\nprocess = \"bad6b.exe\"\nschema = \"@remeber\"\n\n\
         [[apps]]\nprocess = \"ok6.exe\"\nschema = \"zb\"\n",
        "",
    );
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    focus(&c, 6001, 1, "bad6.exe");
    assert_eq!(
        c.active_schema_id(),
        "za",
        "不在 available 的 id ⇒ 跟随全局"
    );
    focus(&c, 6002, 1, "bad6b.exe");
    assert_eq!(c.active_schema_id(), "za", "未知 @ 标记 ⇒ 跟随全局");
    menu_select(&c, 2);
    assert_eq!(disk_active(), "zc", "规则无效 ⇒ 手切按全局处理");
    focus(&c, 6003, 1, "ok6.exe");
    assert_eq!(c.active_schema_id(), "zb", "同文件的有效规则照常生效");
}

/// 热重载：设置页改了 `schema.active` ⇒ 全局方案跟随；焦点正在规则应用里时不被冲回全局。
#[test]
fn reload_updates_global_but_keeps_rule_app() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset("[[apps]]\nprocess = \"code7.exe\"\nschema = \"zb\"\n", "");
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    focus(&c, 2007, 1, "code7.exe");
    assert_eq!(c.active_schema_id(), "zb");

    // 设置页改全局方案并热重载（焦点仍在规则应用里）。
    let cfg = std::fs::read_to_string(user().join("config.toml")).unwrap();
    std::fs::write(
        user().join("config.toml"),
        cfg.replace("active = \"za\"", "active = \"zc\""),
    )
    .unwrap();
    c.reload_user_config();
    c.debug_wait_app_schema_load();
    assert_eq!(c.debug_app_schema_state().0, "zc", "全局方案跟随热重载");
    assert_eq!(c.active_schema_id(), "zb", "规则应用不被热重载冲回全局");

    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(c.active_schema_id(), "zc", "无规则应用用新的全局方案");
}

/// 往返热键（`toggle_schema`）在「切进规则应用再切回来」之后仍能回程：自动切换会让方案
/// 代际 +1，不挂起去程记录的话这一按就没反应。
#[test]
fn toggle_schema_survives_auto_switch_round_trip() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset(
        "[[apps]]\nprocess = \"code8.exe\"\nschema = \"zb\"\n",
        "[keys.key_actions]\n\"ctrl+shift+n\" = \"toggle_schema:zc\"\n",
    );
    let c = make();
    let press = |c: &Coordinator| {
        c.handle_key_event(&KeyEventData {
            key_code: 0x4E, // N（ctrl+shift+e 是出厂 switch_engine）
            scan_code: 0,
            modifiers: MOD_CTRL | MOD_SHIFT,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    };
    focus(&c, PLAIN.0, 1, PLAIN.1);
    press(&c);
    assert_eq!(c.active_schema_id(), "zc", "去程");

    focus(&c, 2008, 1, "code8.exe");
    assert_eq!(c.active_schema_id(), "zb");
    focus(&c, PLAIN.0, 1, PLAIN.1);
    assert_eq!(
        c.active_schema_id(),
        "zc",
        "回到无规则应用：全局 = 去程落点"
    );

    press(&c);
    assert_eq!(
        c.active_schema_id(),
        "za",
        "往返键应仍能回程——自动切换不该让去程记录失效"
    );
}

/// 菜单「应用独立配置 → 方案」：写盘并当场生效；跟随全局清规则并回到全局；
/// 记住上次在无记录时把当前方案记进去（不跳走）。
#[test]
fn menu_writes_rule_and_applies_now() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset("", "");
    let c = make();
    focus(&c, PLAIN.0, 1, PLAIN.1);
    focus(&c, 9009, 1, "menu9.exe");
    assert_eq!(c.active_schema_id(), "za");

    // 固定为 zc（available 下标 2 ⇒ 菜单码 2+2）。
    c.debug_run_menu_cmd(MenuCmd::AppSchemaRule(4));
    c.debug_wait_app_schema_load();
    assert_eq!(
        rule_schema("menu9.exe").as_deref(),
        Some("zc"),
        "应写入用户层 compat.toml"
    );
    assert_eq!(c.active_schema_id(), "zc", "当场生效");
    assert_eq!(disk_active(), "za", "菜单设规则不写 schema.active");

    // 跟随全局：清规则、回到全局。
    c.debug_run_menu_cmd(MenuCmd::AppSchemaRule(0));
    assert_eq!(rule_schema("menu9.exe"), None, "跟随全局 = 清掉该字段");
    assert_eq!(c.active_schema_id(), "za");

    // 先手切到 zb（此刻无规则 ⇒ 全局手切），再设「记住上次」：记忆表播种当前方案，不跳走。
    menu_select(&c, 1);
    c.debug_run_menu_cmd(MenuCmd::AppSchemaRule(1));
    assert_eq!(rule_schema("menu9.exe").as_deref(), Some("@remember"));
    assert_eq!(c.active_schema_id(), "zb");
    assert_eq!(
        c.debug_app_schema_state()
            .1
            .get("menu9.exe")
            .map(String::as_str),
        Some("zb")
    );

    // 越界下标忽略，不改规则。
    c.debug_run_menu_cmd(MenuCmd::AppSchemaRule(99));
    assert_eq!(rule_schema("menu9.exe").as_deref(), Some("@remember"));
}
