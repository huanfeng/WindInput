use super::*;
use crate::pipeline::ModeKind;
use std::sync::Arc;
use wind_config::config::Config;

/// 三个方案的临时 data 目录：
/// - `zzp_own`：自带表（`.`→`·`；`;` 的英半列→`#`）
/// - `zzp_follow`：不写 `[punct]` ⇒ 跟随全局
/// - `english`：临英的数据归属方案，自带另一张表（`.`→`!`）
///
/// 三张表在 `.` 上取值互不相同，「取错方案」与「取对」才给出不同的断言结果。
fn data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_punct_scope_{}_{tag}", std::process::id()));
    let schemas = dir.join("schemas");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&schemas).unwrap();
    let write = |id: &str, punct: &str| {
        std::fs::write(
            schemas.join(format!("{id}.schema.toml")),
            format!("[schema]\nid = \"{id}\"\n[engine]\ntype = \"codetable\"\n{punct}"),
        )
        .unwrap();
    };
    write(
        "zzp_own",
        "[punct.custom_mappings]\n\".\" = [\"·\", \"\", \"\", \"\"]\n\
             \";\" = [\"；\", \"\", \"\", \"#\"]\n",
    );
    write("zzp_follow", "");
    write(
        "english",
        "[punct.custom_mappings]\n\".\" = [\"!\", \"\", \"\", \"\"]\n",
    );
    dir
}

/// 全局表里有一行 `,`→`G`，且与三个方案表的键不重叠——于是「全局那行还在不在」
/// 就直接回答了「整表替换有没有真的整表」。
fn coord_at(dir: &std::path::Path, active: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.active = active.to_string();
    cfg.schema.available = vec!["zzp_own".into(), "zzp_follow".into()];
    cfg.input.punct.custom_enabled = true;
    cfg.input.punct.custom_mappings.insert(
        ",".into(),
        vec!["G".into(), "".into(), "".into(), "".into()],
    );
    Coordinator::new_headless(cfg, Some(dir))
}

/// 取当前语境下生效表里某个源字符的中半列（col 0）值。
fn cn_half(c: &Coordinator, mode: Option<ModeKind>, ch: char) -> Option<String> {
    {
        let mut st = c.state.lock().unwrap();
        st.active = mode;
    }
    let bundle = c.rt();
    let state = c.state.lock().unwrap();
    let punct = c.effective_punct(&bundle, &state);
    let conv = c.punct.lock().unwrap();
    conv.peek_custom(&punct, ch, 0)
}

/// 方案声明了表 ⇒ 全局表整份不参与。
///
/// 只断言「方案的行生效」的话，逐键合并的实现照样通过；区分两种模型的是全局那行还在不在。
#[test]
fn schema_table_replaces_global_table_wholesale() {
    let dir = data_dir("own");
    let c = coord_at(&dir, "zzp_own");

    assert_eq!(
        cn_half(&c, None, '.'),
        Some("·".into()),
        "方案自己的行要生效"
    );
    assert_eq!(
        cn_half(&c, None, ','),
        None,
        "★ 全局的 `,` 那行必须整份不参与——逐键合并时它会残留"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 方案不写这一段 ⇒ 全走全局（含全局的 `custom_enabled`）。
#[test]
fn schema_without_table_follows_global() {
    let dir = data_dir("follow");
    let c = coord_at(&dir, "zzp_follow");

    assert_eq!(cn_half(&c, None, ','), Some("G".into()), "跟随全局那张表");
    assert_eq!(cn_half(&c, None, '.'), None, "别的方案的表不该漏过来");

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 归属方案是 `effective_data_schema` 而不是活跃方案。
///
/// 活跃方案是 `zzp_own`（`.`→`·`），进临英后必须换成 `english` 那张（`.`→`!`）。
#[test]
fn temp_english_takes_english_schema_table() {
    let dir = data_dir("temp");
    let c = coord_at(&dir, "zzp_own");

    assert_eq!(
        cn_half(&c, None, '.'),
        Some("·".into()),
        "主输入路取活跃方案"
    );
    assert_eq!(
        cn_half(&c, Some(ModeKind::TempEnglish), '.'),
        Some("!".into()),
        "★ 临英必须取 english 方案的表——按活跃方案取会拿到 `·`"
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 推给 DLL 的吃键集是跨方案并集，不随活跃方案变。
///
/// `;` 的英半列只有 `zzp_own` 配了，而活跃方案是 `zzp_follow`。按活跃方案裁剪的话，切到
/// `zzp_own` 后 DLL 手里还是旧表（`CONFIG_KEY_CUSTOM_EN_PUNCT` 只在握手与热重载时推）。
#[test]
fn en_punct_eat_set_is_cross_schema_union() {
    let dir = data_dir("union");
    let c = coord_at(&dir, "zzp_follow");

    assert!(
        c.rt().custom_en_punct_chars.contains(&';'),
        "★ 别的方案配的英半列源字符也必须在吃键集里（并集），实际: {:?}",
        c.rt().custom_en_punct_chars
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 守门：`english_pairs_via_pipeline` 直调 `PunctuationConverter::peek_custom`（wind-transform），
/// 绕过了 wind-punct 那层签名收窄，漏接生效表时不会编译失败，只会静默按全局表判定英文配对。
/// 再出现这种直调时，要么收编进 wind-punct，要么照此加一条。
#[test]
fn english_pairs_pipeline_uses_effective_punct() {
    const SRC: &str = include_str!("handle_punct.rs");
    let at = SRC
        .find("fn english_pairs_via_pipeline")
        .expect("源码里找不到 fn english_pairs_via_pipeline（改名了？守卫需同步更新）");
    let body: String = SRC[at..]
        .lines()
        .take_while(|l| !l.starts_with("    /// 英文模式 + 半角"))
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        body.contains("effective_punct"),
        "英文配对的左右形必须按**生效**标点表算，否则方案改了 `(` 的形态后判定按全局表"
    );
    assert!(
        !body.contains("config.input.punct"),
        "不得直取全局 punct——那正是本守卫要挡的漏接"
    );
}
