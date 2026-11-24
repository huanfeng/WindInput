use super::*;
use crate::compat_overlay::{Obj, compose, parse_raw, process_of};

/// 不经 `parse_raw`（它内部的 coalesce 正是被测对象）直接读出 `[[apps]]` 各行。
fn toml_rows(text: &str) -> Vec<Obj> {
    let toml: toml::Value = toml::from_str(text).unwrap();
    toml.get("apps")
        .and_then(|a| a.as_array())
        .unwrap()
        .iter()
        .map(|t| {
            serde_json::to_value(t)
                .unwrap()
                .as_object()
                .unwrap()
                .clone()
        })
        .collect()
}
fn legacy_coalesce(rows: Vec<Obj>) -> Vec<Obj> {
    let mut out: Vec<Obj> = Vec::new();
    for r in rows {
        let name = process_of(&r).to_string();
        let same = (!name.trim().is_empty())
            .then(|| {
                out.iter()
                    .position(|o| process_of(o).trim().eq_ignore_ascii_case(name.trim()))
            })
            .flatten();
        match same {
            Some(i) => out[i] = compose(&out[i], &r),
            None => out.push(r),
        }
    }
    out
}
fn legacy_overlay(mut base: Vec<Obj>, layer: &[Obj]) -> Vec<Obj> {
    for row in layer.iter().filter(|r| !process_of(r).trim().is_empty()) {
        match base.iter().position(|b| {
            process_of(b)
                .trim()
                .eq_ignore_ascii_case(process_of(row).trim())
        }) {
            Some(i) => base[i] = compose(&base[i], row),
            None => base.push(compose(&Obj::new(), row)),
        }
    }
    base
}
/// 旧运行时表：小写进程名 → 规则（`materialize` + `build_lookup`）。
fn legacy_table(layers: &[&str]) -> Vec<(String, String)> {
    let mut apps: Vec<Obj> = Vec::new();
    for text in layers {
        let raw = Raw {
            apps: legacy_coalesce(toml_rows(text)),
            ..Default::default()
        };
        apps = legacy_overlay(apps, &sanitize_raw(&raw).0.apps);
    }
    let rules: Vec<AppCompatRule> = materialize(&apps);
    rules
        .into_iter()
        .filter(|r| !r.process.is_empty())
        .map(|r| {
            (
                r.process.to_ascii_lowercase(),
                serde_json::to_string(&r).unwrap(),
            )
        })
        .collect()
}
fn new_table(layers: &[&str], names: &[String]) -> Vec<(String, String)> {
    let mut raw = Raw::default();
    for text in layers {
        raw = overlay_raw(raw, &sanitize_raw(&parse_raw(text).unwrap()).0);
    }
    let c = AppCompat::from_raw(&raw);
    names
        .iter()
        .filter_map(|n| {
            c.get_rule(n)
                .map(|r| (n.clone(), serde_json::to_string(r).unwrap()))
        })
        .collect()
}

const USER_FIXTURES: [&str; 3] = [
    r#"
[[apps]]
process = "weixin.EXE"
caret_use_top = false
first_show_mode = "instant"

[[apps]]
process = "EXCEL.EXE"
unset = ["first_show_mode"]
auto_pair = false

[[apps]]
process = "MapleStory.exe"
disabled = true

[[apps]]
process = " NewApp.exe "
initial_mode = "english"
schema = "@remember"
caret_offset_x = 3
unknown_future = [1, 2]

[[apps]]
process = "newapp.exe"
initial_mode = "chinese"
auto_pair = true

[[apps]]
process = 1
auto_pair = true

[[apps]]
auto_pair = true

[[apps]]
process = "typo.exe"
first_show_mode = "wiat"
smart_method = "hold_composition"
"#,
    r#"
[[apps]]
process = "QQ.exe"
disabled = true

[[apps]]
process = "SearchHost.exe"
host_render = false

[[apps]]
process = "custom.exe"
host_render = true
"#,
    r#"
[[apps]]
process = "QQ.exe"
disabled = false
first_show_mode = "wait"

[[apps]]
process = "Feishu.exe"
unset = "stale_probe_guard"
"#,
];

fn shipped() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/compat.toml");
    std::fs::read_to_string(p).expect("仓库 data/compat.toml 必须在")
}

#[test]
fn files_without_window_conditions_compose_exactly_as_before() {
    let sys = shipped();
    let stacks: Vec<Vec<&str>> = vec![
        vec![&sys],
        vec![&sys, USER_FIXTURES[0]],
        vec![&sys, USER_FIXTURES[1], USER_FIXTURES[2]],
        vec![&sys, USER_FIXTURES[1], USER_FIXTURES[0]],
        vec![USER_FIXTURES[0]],
    ];
    for (i, layers) in stacks.iter().enumerate() {
        let old = legacy_table(layers);
        assert!(!old.is_empty());
        let mut names: Vec<String> = old.iter().map(|(n, _)| n.clone()).collect();
        names.extend(["unknown.exe", "", "*"].map(String::from));
        let new = new_table(layers, &names);
        assert_eq!(new, old, "第 {i} 组：新旧合成结果不同");
    }
}

#[test]
fn coalesce_and_render_are_unchanged_for_old_files() {
    let sys = shipped();
    for text in [
        sys.as_str(),
        USER_FIXTURES[0],
        USER_FIXTURES[1],
        USER_FIXTURES[2],
    ] {
        let raw = parse_raw(text).unwrap();
        assert_eq!(raw.apps, legacy_coalesce(toml_rows(text)));
        let rendered = render_raw(USER_COMPAT_HEADER, &raw).unwrap();
        assert_eq!(parse_raw(&rendered).unwrap().apps, raw.apps);
    }
}
