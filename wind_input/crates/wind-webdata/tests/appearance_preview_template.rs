//! `appearance.previewTemplate`：设置页预览行的回包契约（设计 text-span-colors.md §11）。
//!
//! 钉住三件设置端依赖的事：`runs` 是输出偏移、按当前主题求色且等于正文色的不下发；
//! `selected` 是高亮候选里的样子（规则 3 回落）；`problems` 是**模板**偏移、覆盖未知变量与
//! 内联色的各类问题。
//!
//! 集成测试（独立进程）：`Config::user_config_dir()` 走 `WIND_DATADIR_CONF` 且内部 OnceLock
//! 缓存，env 必须在任何初始化之前就位。⚠️ 全文件仅此一个 `#[test]`。
//! ⚠️ 不依赖 `build_dev/data`：主题夹具全自造。

use serde_json::{Value, json};
use std::path::Path;
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_webdata::WebDataRpc;

const THEME: &str = r##"
[meta]
name = "预览测试"

[colors]
bg = "#FFFFFF"
selection = "#E6F0FF"
text_hint = "#969696"
accent = "#4285F4"
error = "#D93025"
tooltip_error = "#F28B82"
tooltip_bg = "#3C3C3CF0"
tooltip_text = "#FFFFFF"
ghost = "#FF000000"

[comment]
color = "${text_hint}"

[comment.roles]
code_hint = "#C00000"

[comment.selected]
color = "#FFFFFF"

[tooltip]
background = "${tooltip_bg}"
color = "${tooltip_text}"

[tooltip.roles]
title = "${accent}"
"##;

fn write_at(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
}

#[test]
fn preview_template_contract() {
    let tmp = std::env::temp_dir().join(format!(
        "wind_webdata_preview_template-{}",
        std::process::id()
    ));
    let root = tmp.join("install");
    let data = root.join("data");
    let user = tmp.join("UserData");
    let conf = tmp.join("datadir.conf");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(&conf, user.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: 本文件仅此一个测试，env 在任何 OnceLock 初始化之前设置，无并发读者。
    unsafe {
        std::env::set_var("WIND_DATADIR_CONF", &conf);
        std::env::set_var("WIND_INSTALL_ROOT", &root);
    }
    assert_eq!(Config::user_config_dir(), Some(user.clone()));
    write_at(&data, "themes/default/theme.toml", THEME);
    let mut cfg = Config::default();
    cfg.ui.theme.name = "default".into();
    cfg.ui.theme.style = "light".into();
    let coord = Coordinator::new_headless(cfg, Some(&data));
    let call = |p: Value| coord.web_data_rpc("appearance.previewTemplate", &p);

    // ① 注释：角色色 + 内联色；正文色部分不下发；选中态规则 3 回落（区间全丢）。
    let r = call(json!({"template": "${code_hint} $[accent]{(${pinyin})}", "scene": "comment"}))
        .unwrap();
    assert_eq!(r["text"], "hao (nǐ hǎo)");
    assert_eq!(r["fg"], json!([150, 150, 150, 255]));
    assert_eq!(r["bg"], json!([255, 255, 255, 255]));
    assert_eq!(
        r["runs"],
        json!([
            {"start": 0, "end": 3, "rgba": [192, 0, 0, 255]},
            {"start": 4, "end": 5, "rgba": [66, 133, 244, 255]},
            {"start": 5, "end": 13, "rgba": [66, 133, 244, 255]},
            {"start": 13, "end": 14, "rgba": [66, 133, 244, 255]},
        ]),
        "runs 是输出里的 UTF-8 字节区间；空格（literal，正文色）不下发"
    );
    assert_eq!(r["selected"]["fg"], json!([255, 255, 255, 255]));
    assert_eq!(r["selected"]["bg"], json!([230, 240, 255, 255]));
    assert_eq!(
        r["selected"]["runs"],
        json!([]),
        "选中态改了正文色 ⇒ 全部回落"
    );
    assert_eq!(r["problems"], json!([]));

    // ② 问题：模板偏移，未知变量 / 写法不对 / 当前主题没有 / 全透明 / 未知 key。
    let tpl = "${pinyn} $[nope]{a} $[#GG]{b} $[#FF000000]{c} $[accent,hover=x]{d}";
    let r = call(json!({"template": tpl, "scene": "comment"})).unwrap();
    let probs: Vec<(String, String)> = r["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let (s, e) = (
                p["start"].as_u64().unwrap() as usize,
                p["end"].as_u64().unwrap() as usize,
            );
            (
                tpl[s..e].to_string(),
                p["message"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let has = |span: &str, word: &str| probs.iter().any(|(s, m)| s == span && m.contains(word));
    assert!(has("${pinyn}", "pinyn"), "{probs:?}");
    assert!(has("nope", "nope"), "{probs:?}");
    assert!(has("#GG", "写法"), "{probs:?}");
    assert!(has("#FF000000", "全透明"), "{probs:?}");
    assert!(has("accent,hover=x", "hover"), "{probs:?}");
    assert_eq!(probs.len(), 5, "{probs:?}");

    // ③ 气泡：名字先查 tooltip_<名>；段名字面走 title 角色；无 selected。
    let r = call(
        json!({"template": "${char}：$[error]{${readings}}", "scene": "content", "each": "han"}),
    )
    .unwrap();
    assert_eq!(r["text"], "你：nǐ\n好：hǎo/hào");
    assert_eq!(
        r["runs"][0]["rgba"],
        json!([242, 139, 130, 255]),
        "气泡里 error 取 tooltip_error"
    );
    assert_eq!(r["selected"], Value::Null);
    assert_eq!(r["bg"], json!([60, 60, 60, 240]));
    let r = call(json!({"template": "编码", "scene": "label"})).unwrap();
    assert_eq!(
        r["runs"],
        json!([{"start": 0, "end": 6, "rgba": [66, 133, 244, 255]}])
    );

    // ④ 色块：与内联色同一求法——候选窗取同名色，气泡先查 tooltip_<名>；没请求就是空表。
    let r =
        call(json!({"template": "", "scene": "comment", "swatches": ["error", "accent"]})).unwrap();
    assert_eq!(r["swatches"]["error"], json!([217, 48, 37, 255]));
    assert_eq!(r["swatches"]["accent"], json!([66, 133, 244, 255]));
    let r = call(json!({"template": "", "scene": "content", "swatches": ["error"]})).unwrap();
    assert_eq!(r["swatches"]["error"], json!([242, 139, 130, 255]));
    let r = call(json!({"template": "x", "scene": "comment"})).unwrap();
    assert_eq!(r["swatches"], json!({}));

    // ⑤ 入参上限：模板 4KB、色块 32 个，超了直接报错（不去求值）。
    let big = "a".repeat(wind_webdata::PREVIEW_TEMPLATE_MAX + 1);
    assert!(call(json!({"template": big, "scene": "comment"})).is_err());
    let ok = "a".repeat(wind_webdata::PREVIEW_TEMPLATE_MAX);
    assert!(call(json!({"template": ok, "scene": "comment"})).is_ok());
    let many: Vec<String> = (0..=wind_webdata::PREVIEW_SWATCHES_MAX)
        .map(|i| format!("c{i}"))
        .collect();
    assert!(call(json!({"template": "", "scene": "comment", "swatches": many})).is_err());

    // ⑥ 与引擎口径对齐的诊断：气泡里的 selected=、主题里全透明的名字、SPEC 里被忽略的片段、
    //    场景外的变量。
    let msgs = |r: &Value| -> Vec<String> {
        r["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["message"].as_str().unwrap().to_string())
            .collect()
    };
    let r = call(json!({"template": "$[error,selected=nope]{x}", "scene": "content"})).unwrap();
    assert_eq!(
        msgs(&r),
        vec!["「selected=」在气泡里不生效，已忽略"],
        "气泡不去查 nope"
    );
    let r = call(json!({"template": "$[ghost]{x}", "scene": "comment"})).unwrap();
    assert!(msgs(&r)[0].contains("全透明"), "{:?}", msgs(&r));
    let r = call(json!({"template": "$[accent,selected]{x} $[accent,]{y}", "scene": "comment"}))
        .unwrap();
    let m = msgs(&r);
    assert!(
        m.iter().any(|m| m.contains("=颜色")) && m.iter().any(|m| m.contains("逗号")),
        "{m:?}"
    );
    let r = call(json!({"template": "${word_code}", "scene": "comment"})).unwrap();
    assert_eq!(r["text"], "${word_code}", "注释里没有气泡变量，原样回显");
    assert!(msgs(&r)[0].starts_with("此处不可用"));

    // ⑦ 未知 scene 报错。
    assert!(call(json!({"template": "x", "scene": "nope"})).is_err());

    // ⑧ 样例：缺省是「你好」，回包列出全部样例；`sample` 换样例词，英文样例里中文变量为空、
    //    可选段消失；未知样例报错。
    let tpl = "${dict:zh_en}{ (${pinyin})}{ ${shuangpin}}${dict:en_zh}";
    let r = call(json!({"template": tpl, "scene": "comment"})).unwrap();
    assert_eq!(r["candidate"], "你好");
    assert_eq!(r["sample"], "nihao");
    assert_eq!(r["text"], "int. hello (nǐ hǎo) nihc");
    let ids = |lang: &str| -> Vec<String> {
        r["samples"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["lang"] == lang)
            .map(|s| {
                format!(
                    "{}={}",
                    s["id"].as_str().unwrap(),
                    s["text"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(ids("zh"), ["nihao=你好", "zhongwen=中文", "shurufa=输入法"]);
    assert_eq!(ids("en"), ["apple=apple", "hello=hello", "world=world"]);
    let r = call(json!({"template": tpl, "scene": "comment", "sample": "shurufa"})).unwrap();
    assert_eq!(r["candidate"], "输入法");
    assert_eq!(r["text"], "n. input method (shū rù fǎ) uurufa");
    let r = call(json!({"template": tpl, "scene": "comment", "sample": "apple"})).unwrap();
    assert_eq!(r["candidate"], "apple");
    assert_eq!(r["text"], "n. 苹果");
    assert!(call(json!({"template": tpl, "scene": "comment", "sample": "nope"})).is_err());

    let _ = std::fs::remove_dir_all(&tmp);
}
