use super::FALLBACK_THEME;
use crate::coordinator::Coordinator;
use std::cell::RefCell;

/// 记录每次请求的主题 id，并按 `available` 决定成败——替身返回 `&str` 而非
/// `Resolved`，降级判定与主题求值无关，不该为了测它去造一份主题数据。
fn loader<'a>(
    available: &'a [&'a str],
    seen: &'a RefCell<Vec<String>>,
) -> impl FnMut(&str) -> anyhow::Result<&'static str> + 'a {
    move |n: &str| {
        seen.borrow_mut().push(n.to_string());
        if available.contains(&n) {
            Ok("theme")
        } else {
            anyhow::bail!("theme '{}' not found", n)
        }
    }
}

#[test]
fn requested_theme_wins_without_touching_fallback() {
    let seen = RefCell::new(Vec::new());
    let got = Coordinator::load_theme_with_fallback(loader(&["violet"], &seen), "violet");
    assert_eq!(got.map(|(id, _)| id), Some("violet".to_string()));
    // 请求主题就绪时不该顺带去读 default——多一次读盘就是多一次半截 data 的暴露面。
    assert_eq!(*seen.borrow(), vec!["violet"]);
}

/// 部署期 data\ 半截（请求主题缺、default 在）时必须降级到 default，而不是停在
/// `Resolved::default()` 的编译期零值外观。
#[test]
fn missing_theme_falls_back_to_default() {
    let seen = RefCell::new(Vec::new());
    let got = Coordinator::load_theme_with_fallback(loader(&[FALLBACK_THEME], &seen), "violet");
    assert_eq!(
        got.map(|(id, _)| id),
        Some(FALLBACK_THEME.to_string()),
        "请求主题缺失应降级到 {FALLBACK_THEME}"
    );
    assert_eq!(*seen.borrow(), vec!["violet", FALLBACK_THEME]);
}

#[test]
fn missing_fallback_itself_is_not_retried() {
    let seen = RefCell::new(Vec::new());
    let got = Coordinator::load_theme_with_fallback(loader(&[], &seen), FALLBACK_THEME);
    assert!(got.is_none(), "default 自身缺失时无处可降级");
    assert_eq!(
        *seen.borrow(),
        vec![FALLBACK_THEME],
        "请求的就是 default 时不该重复试第二次"
    );
}

/// 两级皆失败必须返回 None 让调用方**保留当前**：reload 路径也走 push_theme，
/// 此时硬发一份零值主题会把运行中已经好用的外观清掉——那是退化不是兜底。
#[test]
fn both_missing_yields_none_so_caller_keeps_current() {
    let seen = RefCell::new(Vec::new());
    let got = Coordinator::load_theme_with_fallback(loader(&["jade"], &seen), "violet");
    assert!(got.is_none());
    assert_eq!(*seen.borrow(), vec!["violet", FALLBACK_THEME]);
}
