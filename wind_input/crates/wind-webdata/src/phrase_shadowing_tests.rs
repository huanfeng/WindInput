//! 「用户短语遮蔽系统条目」的可逆性：主键只有 `(code, text)` 一把，遮蔽行归属用户，
//! 于是**任何删掉该行的操作都会连带删掉那条系统短语**。每条这样的路径都必须补回缺失的
//! 系统条目，漏一条就是「系统短语莫名少了一条」——正是本特性早期版本的原始 bug。
use super::*;
use std::sync::Arc;
use wind_config::config::Config;
use wind_coordinator::Coordinator;
use wind_store::Store;

const SYS_CODE: &str = "date";
const SYS_TEXT: &str = "$Y年$M月$D日";

/// 带真实 `system.phrases.toml` 的无头协调器（`data_dir=None` 时补齐逻辑会整体早退，
/// 那样测出来的「通过」是假的——它根本没跑到被测代码）。
fn coord_with_sys_phrase(tag: &str) -> (Arc<Coordinator>, std::path::PathBuf) {
    coord_with_sys_toml(tag, "")
}

/// `extra`：追加在 TOML 末尾的更多 `[[phrases]]` 条目。
fn coord_with_sys_toml(tag: &str, extra: &str) -> (Arc<Coordinator>, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("wind_phrase_shadow_{tag}"));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        base.join("system.phrases.toml"),
        format!(
            "[[phrases]]\ncode = '{SYS_CODE}'\ntext = '{SYS_TEXT}'\nweight = 1000\nposition = 1\n{extra}"
        ),
    )
    .unwrap();
    let db = base.join("s.redb");
    let store = Arc::new(Store::open(&db).unwrap());
    let c = Coordinator::new_headless_with_store(Config::default(), Some(&base), store);
    (c, base)
}

fn system_phrase_count(c: &Coordinator) -> usize {
    c.user_store().unwrap().list_system_phrases().unwrap().len()
}

fn user_phrase_count(c: &Coordinator) -> usize {
    c.user_store()
        .unwrap()
        .list_user_phrases_paged(None, 0, 99)
        .unwrap()
        .1
}

/// 前置校验：启动即完成系统短语入库，否则下面每个用例都在空库上跑、结论无意义。
#[test]
fn sanity_system_phrase_seeded_on_startup() {
    let (c, base) = coord_with_sys_phrase("seed");
    assert_eq!(system_phrase_count(&c), 1, "启动应把 TOML 系统短语同步入库");
    let _ = std::fs::remove_dir_all(&base);
}

/// P3 审查 I2：`dict.reload` 载入手改的短语文件后，输入期短语层随之重建。
#[test]
fn dict_reload_rebuilds_phrase_layer() {
    let (c, base) = coord_with_sys_phrase("reload_phrases");
    let store = c.user_store().cloned().unwrap();
    store.add_phrase("yx", "me@x.com", 0, 1800).unwrap();
    c.web_data_rpc("phrase.list", &json!({})).ok();
    let f = store.phrases_path();
    std::fs::write(
        &f,
        std::fs::read_to_string(&f).unwrap() + "dzdz\t地址\t1800\n",
    )
    .unwrap();
    let r = c.web_data_rpc("dict.reload", &json!({})).unwrap();
    assert!(
        r["reloaded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "phrases"),
        "{r}"
    );
    assert!(c.debug_phrase_texts("dzdz").contains(&"地址".to_string()));
    let _ = std::fs::remove_dir_all(&base);
}

/// P3 审查 I1：数据目录里没有 system.phrases.toml 时短语迁移照样做（没有 TOML 就没有系统行），
/// 用户短语不能因此看不见。
#[test]
fn phrase_migration_runs_without_system_toml() {
    let base = std::env::temp_dir().join("wind_phrase_no_toml");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let store = Arc::new(Store::open(base.join("s.redb")).unwrap());
    store
        .legacy_put_phrase("yx", "me@x.com", 1800, 0, true, false, false, "")
        .unwrap();
    let c = Coordinator::new_headless_with_store(Config::default(), Some(&base), store.clone());
    assert!(store.migrated_p3());
    assert!(store.list_phrases().unwrap().iter().any(|p| p.code == "yx"));
    assert!(c.debug_phrase_texts("yx").contains(&"me@x.com".to_string()));
    let _ = std::fs::remove_dir_all(&base);
}

/// 删除遮蔽行 → 系统条目露出来（用户预期：删掉我加的那条，就该回到系统默认）。
#[test]
fn removing_shadowing_phrase_restores_system_entry() {
    let (c, base) = coord_with_sys_phrase("remove");
    let p = json!({ "code": SYS_CODE, "text": SYS_TEXT, "weight": 5000, "position": 9 });
    c.web_data_rpc("phrase.add", &p).unwrap();
    assert_eq!(user_phrase_count(&c), 1, "遮蔽行归用户");
    assert_eq!(system_phrase_count(&c), 0, "系统条目被遮蔽");

    c.web_data_rpc(
        "phrase.remove",
        &json!({ "code": SYS_CODE, "text": SYS_TEXT }),
    )
    .unwrap();
    assert_eq!(user_phrase_count(&c), 0);
    assert_eq!(
        system_phrase_count(&c),
        1,
        "删掉遮蔽行后系统条目必须回来，而不是两条一起消失"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 「清空用户短语」同理，且**不得**顺带重置用户对系统短语的编辑。
#[test]
fn clearing_user_phrases_restores_system_entry_without_resetting_edits() {
    let (c, base) = coord_with_sys_toml(
        "clear",
        "[[phrases]]\ncode = 'other'\ntext = '别的'\nweight = 1\nposition = 0\n",
    );
    let store = c.user_store().cloned().unwrap();
    // 另一条系统短语由用户改过权重（在系统短语列表里调过）
    store
        .update_phrase("other", "别的", None, None, Some(7), Some(4321))
        .unwrap();

    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": SYS_CODE, "text": SYS_TEXT, "weight": 5000, "position": 9 }),
    )
    .unwrap();
    c.web_data_rpc("phrase.resetDefault", &json!({})).unwrap();

    assert_eq!(user_phrase_count(&c), 0, "用户短语已清空");
    assert!(
        store
            .list_system_phrases()
            .unwrap()
            .iter()
            .any(|p| p.code == SYS_CODE),
        "被遮蔽的系统条目须补回"
    );
    let other = store
        .list_system_phrases()
        .unwrap()
        .into_iter()
        .find(|p| p.code == "other")
        .expect("另一条系统短语仍在");
    assert_eq!(
        (other.weight, other.position),
        (4321, 7),
        "清空用户短语不得重置用户对系统短语的编辑（补齐必须只补缺失，不能走 sync）"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 改遮蔽行的编码 → 旧键被 remove，系统条目同样要补回。
#[test]
fn rekeying_shadowing_phrase_restores_system_entry() {
    let (c, base) = coord_with_sys_phrase("rekey");
    c.web_data_rpc(
        "phrase.add",
        &json!({ "code": SYS_CODE, "text": SYS_TEXT, "weight": 5000, "position": 0 }),
    )
    .unwrap();
    assert_eq!(system_phrase_count(&c), 0);

    c.web_data_rpc(
        "phrase.update",
        &json!({ "code": SYS_CODE, "text": SYS_TEXT, "newCode": "rq2" }),
    )
    .unwrap();
    assert_eq!(
        system_phrase_count(&c),
        1,
        "改键腾出原 (code,text) 后系统条目须补回"
    );
    assert_eq!(user_phrase_count(&c), 1, "改键后的用户短语仍在");
    let _ = std::fs::remove_dir_all(&base);
}
