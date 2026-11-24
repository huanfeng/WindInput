//! 走设置页的那条链：
//!
//! ```text
//! charset_edit / charset_import_file → store → rebuild_charsets（装配）→ verdict_of
//! ```
//!
//! store 在临时目录（`new_headless_with_ui_at`），不碰真实 `%APPDATA%`：残留记录会影响下一次测试。

use super::*;
use crate::Coordinator;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_config::Config;

/// 仓库根下的 `data/`，出厂字符类就在它的 `charsets/` 里。
fn data_dir() -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("定位不到仓库根");
    let d = repo.join("data");
    assert!(
        d.join("charsets").is_dir(),
        "找不到出厂字符类目录 {}",
        d.join("charsets").display()
    );
    d
}

fn tmp_user_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "wind_charset_settings_{}_{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 带出厂字符类 + 临时 store 的 headless coordinator。
///
/// 返回的目录兼作导出落点（`charset_export_*_to`）：测试不许调不带 `_to` 的版本，
/// 那会写进真实的 `%TEMP%/WindInput/charsets/`。
fn coord(tag: &str) -> (Arc<Coordinator>, PathBuf) {
    let user = tmp_user_dir(tag);
    let (c, _rx) =
        Coordinator::new_headless_with_ui_at(Config::default(), Some(&data_dir()), Some(&user));
    assert!(
        c.store.is_some(),
        "夹具必须开着 store，否则测的是「无处落盘」那条路"
    );
    (c, user)
}

/// 判定入口：一个字素簇现在算不算常用。
fn is_common(c: &Coordinator, s: &str) -> bool {
    let cc = c.common_chars.read().unwrap();
    cc.is_string_common(s, &c.engine_mgr.charsets())
}

fn stored(c: &Coordinator, key: &str) -> Option<String> {
    c.store.as_ref().unwrap().get_charset_doc(key).unwrap()
}

/// 改 emoji 类的 `default` 到候选判定生效，一条链走通。
///
/// 出厂 emoji 类不表态（`default` 空），`😀` 兜底判常用；设成生僻后立刻改判，无需重启或切方案。
#[test]
fn setting_a_class_default_reaches_the_verdict() {
    let (c, user) = coord("default");

    assert!(is_common(&c, "😀"), "出厂 emoji 类不表态 ⇒ 兜底判常用");

    c.charset_edit(
        "emoji",
        &CharsetEdit {
            default_common: Some("rare".into()),
            ..Default::default()
        },
    )
    .unwrap();

    assert!(!is_common(&c, "😀"), "改完 default 应立即生效");
    assert!(is_common(&c, "我"), "常用汉字不该被带累");

    // 真的落库了，而不是只改了内存镜像。
    let text = stored(&c, "emoji").expect("改动必须落库，否则重启就没了");
    assert!(text.contains("default: rare"));
    assert!(
        !text.contains("ranges:"),
        "只写用户改过的字段——写全量会把出厂值固化进用户层"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// `inherit` 是「本层不表态」，与显式设成常用不是一回事。
///
/// 前者跟随出厂更新，后者钉死；判据是库里还有没有 default 这一行。
#[test]
fn inherit_removes_the_opinion_rather_than_pinning_it() {
    let (c, user) = coord("inherit");

    c.charset_edit(
        "emoji",
        &CharsetEdit {
            default_common: Some("rare".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!is_common(&c, "😀"));

    c.charset_edit(
        "emoji",
        &CharsetEdit {
            default_common: Some("inherit".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(is_common(&c, "😀"), "改回 inherit 应恢复到不表态");
    assert_eq!(
        stored(&c, "emoji"),
        None,
        "整份调整已空 ⇒ 该删记录而不是留一份只有 key 的空壳"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// 免词频与生僻准入两个并集属性同样是「改完即生效」。
#[test]
fn union_flags_take_effect_immediately() {
    let (c, user) = coord("union");
    let reg = c.engine_mgr.charsets();
    assert!(!reg.no_freq("😀"), "出厂不免词频");
    assert!(!reg.in_rare("😀"), "出厂不进生僻模式");

    c.charset_edit(
        "emoji",
        &CharsetEdit {
            no_freq: Some(true),
            in_rare: Some(true),
            ..Default::default()
        },
    )
    .unwrap();

    let reg = c.engine_mgr.charsets();
    assert!(reg.no_freq("😀"));
    assert!(reg.in_rare("😀"));
    assert!(!reg.no_freq("我"), "汉字不该被带累");

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// `enabled: false` 把一个类整个停掉；恢复之后它回来。
#[test]
fn disabling_a_class_removes_it_and_enabling_brings_it_back() {
    let (c, user) = coord("enabled");
    assert!(c.engine_mgr.charsets().class_by_key("emoji").is_some());

    c.charset_edit(
        "emoji",
        &CharsetEdit {
            enabled: Some(false),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        c.engine_mgr.charsets().class_by_key("emoji").is_none(),
        "停用的类不该还在 registry 里"
    );

    c.charset_edit(
        "emoji",
        &CharsetEdit {
            enabled: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        c.engine_mgr.charsets().class_by_key("emoji").is_some(),
        "恢复启用后该回来"
    );
    assert_eq!(
        stored(&c, "emoji"),
        None,
        "enabled=true 是默认值，不该留一条永远等于默认的记录"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// 停用的类仍要出现在列表里，否则「停用」不可逆。
///
/// 停用的类装配时已从 registry 移除；只列 registry 的话，那一行消失后就没有入口启用回来。
#[test]
fn a_disabled_class_stays_visible_so_it_can_be_re_enabled() {
    let (c, user) = coord("visible");
    c.charset_edit(
        "emoji",
        &CharsetEdit {
            enabled: Some(false),
            ..Default::default()
        },
    )
    .unwrap();

    let rows = c.charset_rows();
    let row = rows
        .iter()
        .find(|r| r.key == "emoji")
        .expect("停用的类必须还在列表里，否则用户找不回它");
    assert!(!row.enabled, "该标成停用");
    assert!(row.overridden, "它的状态来自用户层调整");
    assert!(row.builtin, "停用了也还是出厂类");

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// `charset_reset` 撤掉这个类的全部调整。
#[test]
fn resetting_drops_every_adjustment() {
    let (c, user) = coord("reset");
    c.charset_edit(
        "emoji",
        &CharsetEdit {
            default_common: Some("rare".into()),
            no_freq: Some(true),
            order: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!is_common(&c, "😀"));

    c.charset_reset("emoji").unwrap();
    assert!(is_common(&c, "😀"), "恢复默认后判定该回到出厂");
    assert!(!c.engine_mgr.charsets().no_freq("😀"));
    assert_eq!(stored(&c, "emoji"), None);

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// 不存在的类要报错，不能静默写出一条永远不生效的记录。
#[test]
fn editing_an_unknown_class_fails_loudly() {
    let (c, user) = coord("unknown");
    let e = c
        .charset_edit(
            "没有这个类",
            &CharsetEdit {
                default_common: Some("rare".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        e.to_string().contains("没有名为"),
        "错误信息要说清是什么问题：{e}"
    );
    assert!(
        c.store
            .as_ref()
            .unwrap()
            .list_charset_docs()
            .unwrap()
            .is_empty(),
        "拒绝之后不该留下任何记录"
    );
    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// `default` 只认三个值，别的当场报错：静默忽略的表现是「点了没反应」，设置页拿不到线索。
#[test]
fn an_invalid_default_value_is_rejected() {
    let (c, user) = coord("badvalue");
    let e = c
        .charset_edit(
            "emoji",
            &CharsetEdit {
                default_common: Some("maybe".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(e.to_string().contains("common / rare / inherit"), "{e}");
    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// 列表把出厂那些类都列出来，且顺序 = 仲裁顺序；出厂类标 `builtin`。
#[test]
fn rows_list_every_class_in_arbitration_order() {
    let (c, user) = coord("rows");
    let rows = c.charset_rows();

    for key in ["emoji", "common_han", "符号", "基本汉字", "其它"] {
        let r = rows
            .iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("字符类列表里缺 {key}"));
        assert!(r.builtin, "{key} 是出厂类");
        assert!(!r.overridden, "出厂状态下没有调整");
    }
    let orders: Vec<i32> = rows.iter().map(|r| r.order).collect();
    let mut sorted = orders.clone();
    sorted.sort();
    assert_eq!(orders, sorted, "列表顺序必须与仲裁顺序一致");

    // 出厂 emoji 类不表态；common_han 表态。
    let emoji = rows.iter().find(|r| r.key == "emoji").unwrap();
    assert_eq!(emoji.default_common, None);
    assert!(emoji.member_count > 1400, "emoji 类该带着那份精确字表");
    let han = rows.iter().find(|r| r.key == "common_han").unwrap();
    assert_eq!(han.default_common, Some(true));

    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

// ── 外部编辑 ───────────────────────────────────────────────────────────

/// 新建：一个空的自建分类直接进列表（成员之后用「编辑文件」加）；重名、出厂名、
/// 只差大小写的名字、不合法的文件名都拒绝。
#[test]
fn creating_an_empty_class() {
    let (c, user) = coord("create");
    let edit = CharsetEdit {
        default_common: Some("rare".into()),
        order: Some(7),
        ..Default::default()
    };
    c.charset_create("my_symbols", "我的符号", &edit).unwrap();
    let rows = c.charset_rows();
    let row = rows
        .iter()
        .find(|r| r.key == "my_symbols")
        .expect("新分类进列表");
    assert_eq!(row.name, "我的符号");
    assert!(!row.builtin);
    assert_eq!(row.member_count, 0);
    assert_eq!(row.order, 7);

    let dup = c
        .charset_create("my_symbols", "x", &CharsetEdit::default())
        .unwrap_err();
    assert!(dup.to_string().contains("已有"), "{dup}");
    let case = c
        .charset_create("MY_SYMBOLS", "x", &CharsetEdit::default())
        .unwrap_err();
    assert!(
        case.to_string().contains("已有"),
        "Windows 文件名不分大小写：{case}"
    );
    let factory = c
        .charset_create("emoji", "x", &CharsetEdit::default())
        .unwrap_err();
    assert!(factory.to_string().contains("已有"), "{factory}");
    let bad = c
        .charset_create("a/b", "x", &CharsetEdit::default())
        .unwrap_err();
    assert!(bad.to_string().contains("文件名"), "{bad}");

    // 不给优先级：排在常用汉字表之前，否则对汉字不起作用。
    c.charset_create("plain", "", &CharsetEdit::default())
        .unwrap();
    let rows = c.charset_rows();
    let plain = rows.iter().find(|r| r.key == "plain").unwrap();
    assert_eq!(plain.order, NEW_CLASS_ORDER);
    assert_eq!(plain.name, "plain", "显示名留空同文件名");

    // 写坏的同名文件：列表里看不见，但新建同名不许覆盖它。
    let path = c.charset_store().unwrap().charset_path("broken").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "key: [不合法\n").unwrap();
    let e = c
        .charset_create("broken", "x", &CharsetEdit::default())
        .unwrap_err();
    assert!(e.to_string().contains("已有"), "{e}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "key: [不合法\n");

    let dev = c
        .charset_create("con", "x", &CharsetEdit::default())
        .unwrap_err();
    assert!(dev.to_string().contains("文件名"), "Windows 设备名：{dev}");
    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// 出厂类不能删——删了会"恢复"而不是消失，用户以为删了、列表里还在。
#[test]
fn deleting_a_factory_class_is_refused() {
    let (c, user) = coord("nodelete");
    let e = c.charset_delete("emoji").unwrap_err();
    assert!(e.to_string().contains("出厂类"), "{e}");
    assert!(c.engine_mgr.charsets().class_by_key("emoji").is_some());
    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}

/// 新建类的缺省 `order` 必须压过所有出厂类，判据读仓里真实的 `data/charsets/`。
///
/// 见 [`NEW_CLASS_ORDER`]：压不过 `common_han` 时，用户新建类把生僻字设成常用会毫无反应，
/// 且没有 warn（`shadowed_keys` 只比对 ranges）。出厂调小某类 order 时这条当场红。
#[test]
fn a_new_class_outranks_every_factory_class() {
    let (c, user) = coord("new_order");
    let lowest = c
        .engine_mgr
        .charset_factory()
        .values()
        .map(|f| f.def.order_or_default())
        .min()
        .expect("出厂层至少有 blocks.yaml 里那批类");
    assert!(
        NEW_CLASS_ORDER < lowest,
        "新建类的 order {NEW_CLASS_ORDER} 必须小于最小的出厂 order {lowest}"
    );

    // 新建 → 落盘 → 装配一整条链：真的以这个优先级排在最前。
    c.charset_create("mine", "", &CharsetEdit::default())
        .unwrap();
    let rows = c.charset_rows();
    assert_eq!(
        (rows[0].key.as_str(), rows[0].order),
        ("mine", NEW_CLASS_ORDER),
        "order 最小 ⇒ 列表第一行（列表序就是仲裁序）"
    );
    drop(c);
    let _ = std::fs::remove_dir_all(&user);
}
