//! 出厂短语里**所有平台**的 `$CC(...)` 词条都必须能被命令直通车解析。
//!
//! 平台过滤让每个平台只加载自己的条目，于是别的平台的条目在本机永远不被执行、也就永远不被
//! 发现写坏了。macOS 那几条此前甚至连“过滤放行”都没有过，从未被跑过。这条测试绕过过滤，
//! 直接按 TOML 原文逐条过一遍 `lint_phrase`（解析 + 函数名/参数个数校验），让 Linux 开发机
//! 也能守住 macOS / Windows 条目的语法。
//!
//! 只能证明“写得对”，证明不了“在对应平台上跑得通”（比如 `open -a TextEdit` 真机才知道）。

use std::path::PathBuf;

fn repo_phrases() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../data/system.phrases.toml")
}

#[test]
fn every_platform_entry_parses_as_a_command_phrase() {
    let p = repo_phrases();
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {}：{e}", p.display()));
    let doc: toml::Value = toml::from_str(&raw).expect("system.phrases.toml 不是合法 TOML");
    let phrases = doc
        .get("phrases")
        .and_then(|v| v.as_array())
        .expect("缺 [[phrases]]");
    assert!(!phrases.is_empty(), "解析出 0 条，下面的断言会空过");

    let mut checked = 0usize;
    let mut platforms = std::collections::BTreeSet::new();
    let mut bad = Vec::new();
    for ph in phrases {
        let code = ph.get("code").and_then(|v| v.as_str()).unwrap_or("?");
        let text = ph.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let platform = ph.get("platform").and_then(|v| v.as_str()).unwrap_or("all");
        platforms.insert(platform.to_string());
        if !text.starts_with("$CC(") {
            continue;
        }
        checked += 1;
        if let Err(e) = wind_cmdbar::lint_phrase(text) {
            bad.push(format!("[{platform}] {code}: {e}  <- {text}"));
        }
    }
    assert!(checked > 0, "没有任何 $CC 词条被检查");
    // 三个桌面平台的条目都得在场，否则等于没守到（有人把 macos 条目整块删了也该红）。
    // 苹果统一写 macos（darwin 是旧写法，过滤照认，但出厂文件不再用它）。
    for want in ["windows", "macos", "linux"] {
        assert!(
            platforms.contains(want),
            "文件里没有 platform='{want}' 的条目"
        );
    }
    assert!(bad.is_empty(), "有词条解析失败：\n{}", bad.join("\n"));
}
