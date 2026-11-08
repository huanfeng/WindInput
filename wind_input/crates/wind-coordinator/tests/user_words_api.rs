//! `user_words_api`（用户词管理的存储域逻辑，桌面设置页与移动端门面共用）的行为契约。
//!
//! 桌面 RPC 形状（JSON、转义）在 wind-webdata 的测试里验；这里验下沉后的本体：显示码与
//! 扁平 key 的往返、搜索的分组、删除 / 清空，以及「加完立即打得出」。
//!
//! 前半段不依赖词库（方案未知 ⇒ 不折叠、不归一，只靠码里的空格声明切分），恒跑；
//! 后半段要 `build_dev/data`，缺失时打印跳过。

use std::path::PathBuf;
use std::sync::Arc;

use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_coordinator::user_words_api::{self as api, UserWordSort};
use wind_coordinator::web_host::WebDataHost;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
        && data_dir().join("schemas/pinyin.schema.toml").exists()
}

/// 无词库的协调器 + 临时 redb。⚠️ 文件名带 pid：多会话并行跑测试时固定名会互删夹具。
fn bare(tag: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_uwapi_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    Coordinator::new_headless_with_store(Config::default(), None, store)
}

fn codes(page: &api::UserWordListing) -> Vec<String> {
    page.items.iter().map(api::display_code).collect()
}

fn texts(page: &api::UserWordListing) -> Vec<&str> {
    page.items.iter().map(|r| r.text.as_str()).collect()
}

/// 加词 → 列表读回：存储 key 扁平，显示码带音节空格；码表码原样。
#[test]
fn add_then_list_round_trips_display_code() {
    let c = bare("roundtrip");
    api::add(&*c, "pinyin", "ni hao", "你好", 1200).unwrap();
    api::add(&*c, "pinyin", "wo men", "我们", 1100).unwrap();
    api::add(&*c, "pinyin", "aabb", "码表", 1000).unwrap();

    let store = c.user_store().unwrap();
    assert_eq!(
        store.get_user_words("pinyin", "nihao").unwrap().len(),
        1,
        "key 必须是扁平的 nihao"
    );

    let page = api::list_paged(&*c, "pinyin", "", None, 0, 50).unwrap();
    assert_eq!(page.total, 3);
    // 流式路径按 key 序
    assert_eq!(codes(&page), ["aabb", "ni hao", "wo men"]);

    // 分页：total 恒为全量，items 只给这一页
    let page = api::list_paged(&*c, "pinyin", "", None, 1, 1).unwrap();
    assert_eq!((page.total, texts(&page)), (3, vec!["你好"]));

    // 排序路径：跨页全局按权重降序
    let page =
        api::list_paged(&*c, "pinyin", "", Some((UserWordSort::Weight, true)), 0, 50).unwrap();
    assert_eq!(texts(&page), ["你好", "我们", "码表"]);
}

/// 搜索：编码前缀（可带空格）在前，词条内容 / 编码中段命中在后。
#[test]
fn search_groups_prefix_hits_before_text_and_middle_hits() {
    let c = bare("search");
    api::add(&*c, "pinyin", "ni hao", "你好", 1200).unwrap();
    api::add(&*c, "pinyin", "hao de", "好的", 1200).unwrap();
    api::add(&*c, "pinyin", "wo men", "我们", 1200).unwrap();

    let page = api::list_paged(&*c, "pinyin", "ni ha", None, 0, 50).unwrap();
    assert_eq!(texts(&page), ["你好"], "照着显示码抄的带空格前缀也要命中");

    let page = api::list_paged(&*c, "pinyin", "们", None, 0, 50).unwrap();
    assert_eq!(texts(&page), ["我们"], "词条内容包含");

    // `hao` 前缀命中「好的」排前，中段命中「你好」排后
    let page = api::list_paged(&*c, "pinyin", "hao", None, 0, 50).unwrap();
    assert_eq!((page.total, texts(&page)), (2, vec!["好的", "你好"]));
}

/// 删除收显示码（带空格）也删得掉；清空返回条数。
#[test]
fn remove_by_display_code_and_clear_counts() {
    let c = bare("remove");
    api::add(&*c, "pinyin", "ni hao", "你好", 1200).unwrap();
    api::add(&*c, "pinyin", "wo men", "我们", 1200).unwrap();

    let page = api::list_paged(&*c, "pinyin", "", None, 0, 50).unwrap();
    let shown = api::display_code(&page.items[0]);
    assert_eq!(shown, "ni hao");
    api::remove(&*c, "pinyin", &shown, "你好").unwrap();
    let page = api::list_paged(&*c, "pinyin", "", None, 0, 50).unwrap();
    assert_eq!(texts(&page), ["我们"], "拿列表给的显示码回传须删得掉");

    api::add(&*c, "pinyin", "ta men", "他们", 1200).unwrap();
    assert_eq!(api::clear(&*c, "pinyin").unwrap(), 2);
    assert_eq!(
        api::list_paged(&*c, "pinyin", "", None, 0, 50)
            .unwrap()
            .total,
        0
    );
}

/// 只读模式（meta 版本高于本版本）store 不打开：读写都报错而不是假装成功。
#[test]
fn readonly_storage_rejects_reads_and_writes() {
    let user = std::env::temp_dir().join(format!("wind_uwapi_ro_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&user);
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.join("meta.toml"),
        "format_version = 99\ndir_id = \"x\"\n",
    )
    .unwrap();
    let (c, _rx) = Coordinator::new_headless_with_ui_at(Config::default(), None, Some(&user));
    assert!(c.storage_readonly_reason().is_some(), "前提：只读模式");
    assert!(api::add(&*c, "pinyin", "ni hao", "你好", 1200).is_err());
    assert!(api::remove(&*c, "pinyin", "ni hao", "你好").is_err());
    assert!(api::clear(&*c, "pinyin").is_err());
    assert!(api::list_paged(&*c, "pinyin", "", None, 0, 50).is_err());
    let _ = std::fs::remove_dir_all(&user);
}

// ── 以下要 build_dev 词库 ─────────────────────────────────────────────

fn with_data(tag: &str, active: &str) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_uwapi_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into(), "shuangpin".into()];
    cfg.schema.active = active.into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = false;
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store)
}

/// 双拼方案 id 折叠到 `pinyin` 数据方案；`xv` 按 ü 归一落成 `xu`，搜 `xv` 也搜得到。
#[test]
fn shuangpin_folds_to_pinyin_and_normalizes_u() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = with_data("fold", "pinyin");
    api::add(&*c, "shuangpin", "xv yao", "需要", 1200).unwrap();
    let store = c.user_store().unwrap();
    assert_eq!(
        store.get_user_words("pinyin", "xuyao").unwrap().len(),
        1,
        "落进 pinyin 数据方案、key 归一成 xuyao"
    );
    let page = api::list_paged(&*c, "pinyin", "xv", None, 0, 50).unwrap();
    assert_eq!(codes(&page), ["xu yao"]);
}

/// 加词之后不重载、不重启，引擎候选立即可见（结构代次推进 ⇒ 候选缓存失效）。
#[test]
fn added_word_is_visible_to_engine_immediately() {
    if !dict_ready() {
        eprintln!("!!! 跳过：build_dev 词库不存在");
        return;
    }
    let c = with_data("visible", "wubi86");
    let type_code = |code: &str| {
        for ch in code.chars() {
            c.handle_key_event(&KeyEventData {
                key_code: (ch.to_ascii_uppercase() as u32) & 0xFF,
                scan_code: 0,
                modifiers: 0,
                event_type: EVENT_KEY_DOWN,
                toggles: 0,
                event_seq: 0,
                prev_char: 0,
                seq_char: 0,
            });
        }
        c.candidate_window(0, 50).items
    };
    let esc = || {
        c.handle_key_event(&KeyEventData {
            key_code: 0x1B,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    };

    // 先打一遍把这个码的候选算进缓存，再加词——验的正是缓存会不会挡住新词。
    let before = type_code("aaaa");
    assert!(!before.iter().any(|t| t == "风起测词"), "前提：词库里没有");
    esc();
    api::add(&*c, "wubi86", "aaaa", "风起测词", 1200).unwrap();
    let after = type_code("aaaa");
    assert!(
        after.iter().any(|t| t == "风起测词"),
        "加完立即可见，实得 {after:?}"
    );
}
