use super::*;
use wind_ui_types::{TooltipDoc, TooltipLine, TooltipSection};

fn section(title: Option<&str>, lines: &[&str]) -> TooltipSection {
    TooltipSection {
        title: title.map(Into::into),
        inline: false,
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, t)| TooltipLine {
                text: (*t).into(),
                raw: i as u16,
            })
            .collect(),
    }
}

/// 完整原文（整段，显示已截断）+ 拼音（逐字）+ 无名段。
fn entry() -> TooltipPageEntry {
    TooltipPageEntry {
        text: "你好世界".into(),
        rendered: RenderedTooltip {
            doc: std::sync::Arc::new(TooltipDoc {
                sections: vec![
                    section(Some("完整原文"), &["你好…"]),
                    section(Some("拼音"), &["你：nǐ", "好：hǎo/hào"]),
                    section(None, &["x"]),
                ],
            }),
            raw: vec![
                vec!["你好世界".into()],
                vec!["你：nǐ".into(), "好：hǎo/hào".into()],
                vec!["x".into()],
            ],
            per_char: vec![false, true, false],
        },
    }
}

fn labels(items: &[MenuItemSpec]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

fn hit(section: u16, raw_line: Option<u16>) -> Option<TooltipHit> {
    Some(TooltipHit { section, raw_line })
}

#[test]
fn menu_items_follow_the_hit() {
    let e = entry();
    let all = ["复制全部", "截图此窗口"];
    assert_eq!(labels(&tooltip_menu_items(Some(&e), None)), all, "内边距");
    assert_eq!(
        labels(&tooltip_menu_items(None, hit(0, None))),
        ["截图此窗口"],
        "没有可信快照（缓存缺失 / 与 UI 所画不一致）：不给任何取值项"
    );
    assert_eq!(
        labels(&tooltip_menu_items(Some(&e), hit(1, Some(1)))),
        [
            "复制「拼音」",
            "上屏「拼音」",
            "复制此行",
            "上屏此行",
            "",
            "复制全部",
            "截图此窗口"
        ],
        "逐字段的内容行"
    );
    assert_eq!(
        labels(&tooltip_menu_items(Some(&e), hit(1, None))),
        ["复制「拼音」", "上屏「拼音」", "", "复制全部", "截图此窗口"],
        "标题行没有「此行」"
    );
    assert_eq!(
        labels(&tooltip_menu_items(Some(&e), hit(0, Some(0)))),
        [
            "复制「完整原文」",
            "上屏「完整原文」",
            "",
            "复制全部",
            "截图此窗口"
        ],
        "整段求值的段没有「此行」"
    );
    assert_eq!(
        labels(&tooltip_menu_items(Some(&e), hit(2, Some(0))))[..2],
        ["复制「第 3 段」", "上屏「第 3 段」"],
        "无名段用序号"
    );
    assert_eq!(
        labels(&tooltip_menu_items(Some(&e), hit(9, Some(0)))),
        all,
        "越界命中退化"
    );
}

#[test]
fn long_section_label_is_truncated() {
    assert_eq!(
        section_label(Some("编码(五笔86极点版)"), 0),
        "编码(五笔86极…"
    );
    assert_eq!(section_label(Some(""), 1), "第 2 段");
}

/// 段名截断按字素簇计：ZWJ 序列不被切开，恰好 8 簇不截。
#[test]
fn section_label_truncates_by_grapheme() {
    let family = "👨\u{200D}👩\u{200D}👧";
    let eight = format!("abcdefg{family}");
    assert_eq!(section_label(Some(&eight), 0), eight, "恰好 8 簇不截");
    assert_eq!(
        section_label(Some(&format!("{eight}h")), 0),
        format!("{eight}…")
    );
}

/// 取值用原始行、不含段名：完整原文段复制得到未截断原文。
#[test]
fn values_come_from_raw_lines() {
    let e = entry();
    let v = |h, c| tooltip_value(&e, h, c);
    assert_eq!(
        v(hit(0, Some(0)), MenuCmd::TooltipCopySection).as_deref(),
        Some("你好世界")
    );
    assert_eq!(
        v(hit(1, None), MenuCmd::TooltipCommitSection).as_deref(),
        Some("你：nǐ\n好：hǎo/hào")
    );
    assert_eq!(
        v(hit(1, Some(1)), MenuCmd::TooltipCopyLine).as_deref(),
        Some("好：hǎo/hào")
    );
    assert_eq!(
        v(hit(1, None), MenuCmd::TooltipCopyLine),
        None,
        "标题行没有「此行」"
    );
    assert_eq!(
        v(None, MenuCmd::TooltipCopy).as_deref(),
        Some("[完整原文]\n你好世界\n[拼音]\n你：nǐ\n好：hǎo/hào\nx")
    );
}

// ───────────────── 协调器端到端（需 build_dev/data 词库）─────────────────

use std::sync::mpsc::Receiver;
use wind_bridge::handler::{KeyEventData, MessageHandler};

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn key(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 测试独占的用户目录（store 落这里），drop 时删掉。
struct UserDir(std::path::PathBuf);

impl Drop for UserDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 打出 `nihao`（拼音方案），候选页已组装、气泡已缓存。词库缺失返回 `None`。
fn typed(tag: &str) -> Option<(std::sync::Arc<Coordinator>, Receiver<UiCommand>, UserDir)> {
    if !data_dir().join("schemas/pinyin.schema.toml").exists() {
        return None;
    }
    let mut cfg = wind_config::Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    let user =
        UserDir(std::env::temp_dir().join(format!("wind_tipmenu_{tag}_{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&user.0);
    let (c, rx) = Coordinator::new_headless_with_ui_at(cfg, Some(&data_dir()), Some(&user.0));
    c.debug_mark_coords_ready();
    for ch in "nihao".chars() {
        c.handle_key_event(&key(ch.to_ascii_uppercase() as u32));
    }
    Some((c, rx, user))
}

fn drain(rx: &Receiver<UiCommand>) -> Vec<UiCommand> {
    rx.try_iter().collect()
}

/// 页缓存与下发给 UI 的气泡是同一份：候选页组装一次，两边都在。
#[test]
fn page_cache_matches_the_docs_sent_to_ui() {
    let Some((c, rx, _u)) = typed("cache") else {
        return;
    };
    let sent: Vec<std::sync::Arc<TooltipDoc>> = drain(&rx)
        .into_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::UpdateCandidates { candidates, .. } => Some(
                candidates
                    .into_iter()
                    .map(|i| i.tooltip)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .next_back()
        .expect("应下发过候选页");
    let page = c.tooltip_page.lock().unwrap();
    assert_eq!(page.len(), sent.len());
    assert_eq!(page[0].text, "你好");
    for (e, d) in page.iter().zip(&sent) {
        assert_eq!(&e.rendered.doc, d);
    }
}

/// 弹菜单（与 UI 一致的指纹）并取出菜单项标签。
fn open_menu(
    c: &Coordinator,
    rx: &Receiver<UiCommand>,
    fp: u64,
    h: Option<TooltipHit>,
) -> Vec<String> {
    let _ = drain(rx);
    c.show_tooltip_menu(0, 0, 0, h, fp);
    drain(rx)
        .into_iter()
        .find_map(|cmd| match cmd {
            UiCommand::ShowCandidateMenu { items, .. } => {
                Some(items.into_iter().map(|i| i.label).collect())
            }
            _ => None,
        })
        .expect("应弹出菜单")
}

fn fp0(c: &Coordinator) -> u64 {
    c.tooltip_page.lock().unwrap()[0].rendered.doc.fingerprint()
}

/// 把第 0 个候选的气泡换成「前面多一个 [编码] 段」的结构，候选原文不变——审查探针的
/// 现场：反查索引后台建好，同一个候选的气泡被重算。
fn prepend_code_section(c: &Coordinator) {
    let mut page = c.tooltip_page.lock().unwrap();
    let r = &mut page[0].rendered;
    std::sync::Arc::make_mut(&mut r.doc).sections.insert(
        0,
        wind_ui_types::TooltipSection {
            title: Some("编码".into()),
            inline: false,
            lines: vec![TooltipLine {
                text: "wqvb".into(),
                raw: 0,
            }],
        },
    );
    r.raw.insert(0, vec!["wqvb".into()]);
    r.per_char.insert(0, false);
}

// 只被 changed_candidate_still_copies_but_refuses_commit 用，门与它一致
fn copied(rx: &Receiver<UiCommand>) -> Vec<String> {
    drain(rx)
        .into_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::CopyTooltipText(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn toasts(rx: &Receiver<UiCommand>) -> Vec<String> {
    drain(rx)
        .into_iter()
        .filter_map(|cmd| match cmd {
            UiCommand::ShowToast { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

/// 审查探针，闸门一：UI 画的是旧结构、缓存已换成新结构（前面插了一段）⇒ 指纹不等，
/// 只给截图，不给任何取值项；此时硬点取值命令也只会放弃并 Toast。
#[test]
fn menu_degrades_when_ui_and_cache_disagree() {
    let Some((c, rx, _u)) = typed("fp") else {
        return;
    };
    let ui_fp = fp0(&c);
    let ui_line = c.tooltip_page.lock().unwrap()[0]
        .rendered
        .raw
        .last()
        .cloned();
    prepend_code_section(&c);
    let items = open_menu(&c, &rx, ui_fp, hit(0, Some(0)));
    assert_eq!(items, ["截图此窗口"]);
    c.tooltip_menu_action(MenuCmd::TooltipCopyLine);
    assert_eq!(toasts(&rx), [ABANDONED_TOAST]);
    // 指纹一致时恢复完整菜单（对照：闸门不是一刀切）。
    let items = open_menu(&c, &rx, fp0(&c), hit(0, Some(0)));
    assert!(
        items.contains(&"复制全部".to_string()),
        "{items:?} / UI 行 {ui_line:?}"
    );
}

/// 审查探针，闸门二：弹菜单时一致，菜单开着期间同一候选的结构变了 ⇒ 取值仍来自快照，
/// 「上屏此行」上屏的是用户点的那一行，而不是新结构同位置的 `wqvb`。
#[test]
fn action_takes_value_from_snapshot_not_from_refreshed_cache() {
    let Some((c, rx, _u)) = typed("snap") else {
        return;
    };
    let (sec, line, want) = {
        let page = c.tooltip_page.lock().unwrap();
        let r = &page[0].rendered;
        let s = r
            .per_char
            .iter()
            .position(|&p| p)
            .expect("出厂有逐字的拼音段");
        (s as u16, 0u16, r.raw[s][0].clone())
    };
    let cap = c.push_server.attach_capture_client(7);
    let _ = open_menu(&c, &rx, fp0(&c), hit(sec, Some(line)));
    prepend_code_section(&c);
    c.tooltip_menu_action(MenuCmd::TooltipCommitLine);
    let pushed: Vec<Vec<u8>> = cap.try_iter().collect();
    assert_eq!(
        pushed.last(),
        Some(&wind_ipc::codec::encode_commit_text(
            &want, None, false, true, false
        )),
        "应上屏快照里的「{want}」"
    );
}

/// 候选已变：复制照常（取快照），上屏放弃并 Toast。
///
/// 用例靠「菜单开着时 Esc 被菜单消费」来关菜单。
#[test]
fn changed_candidate_still_copies_but_refuses_commit() {
    let Some((c, rx, _u)) = typed("stale") else {
        return;
    };
    let want = c.tooltip_page.lock().unwrap()[0].rendered.raw_plain_text();
    let _ = open_menu(&c, &rx, fp0(&c), hit(0, None));
    // 菜单开着时候选刷新（Esc 后重打 shi）：第 0 个候选不再是「你好」。
    c.handle_key_event(&key(wind_keys::keymap::VK_ESCAPE));
    for ch in "SHI".chars() {
        c.handle_key_event(&key(ch as u32));
    }
    assert_ne!(c.tooltip_page.lock().unwrap()[0].text, "你好");
    let _ = drain(&rx);
    c.tooltip_menu_action(MenuCmd::TooltipCopy);
    assert_eq!(copied(&rx), [want]);
    let cap = c.push_server.attach_capture_client(9);
    c.tooltip_menu_action(MenuCmd::TooltipCommitSection);
    assert_eq!(cap.try_iter().count(), 0, "候选已变不得上屏");
    assert_eq!(toasts(&rx), [ABANDONED_TOAST]);
}

/// 完整链路：菜单动作 → `commit_text_ending_session` → `push_no_key_ctx_action` → push。
/// 含换行的原文经与候选上屏同一道换行改写；会话结束、不进联想、不记词频、不喂造词。
#[test]
fn commit_goes_through_the_push_exit() {
    let Some((c, rx, _u)) = typed("push") else {
        return;
    };
    // 完整原文段（含换行）：真实的多行完整原文要长短语词条，这里直接放进缓存。
    let full = "第一行\n第二行";
    {
        let mut page = c.tooltip_page.lock().unwrap();
        let r = &mut page[0].rendered;
        std::sync::Arc::make_mut(&mut r.doc).sections.insert(
            0,
            wind_ui_types::TooltipSection {
                title: Some("完整原文".into()),
                inline: false,
                lines: vec![
                    TooltipLine {
                        text: "第一行".into(),
                        raw: 0,
                    },
                    TooltipLine {
                        text: "第二行".into(),
                        raw: 1,
                    },
                ],
            },
        );
        r.raw
            .insert(0, full.split('\n').map(str::to_string).collect());
        r.per_char.insert(0, false);
    }
    let store = c.store.clone().expect("指定了用户目录应开 store");
    let freq = || {
        store
            .get_freq("pinyin", "nihao", "你好")
            .ok()
            .flatten()
            .map(|r| r.count)
    };
    let freq_before = freq();
    let writes_before = c
        .auto_phrase_writes
        .load(std::sync::atomic::Ordering::Relaxed);
    let history_before = c.recent_commits.lock().unwrap().clone();
    let cap = c.push_server.attach_capture_client(3);

    let _ = open_menu(&c, &rx, fp0(&c), hit(0, None));
    c.tooltip_menu_action(MenuCmd::TooltipCommitSection);

    let expected = c.convert_commit_newline(full.to_string());
    let pushed: Vec<Vec<u8>> = cap.try_iter().collect();
    assert_eq!(
        pushed.last(),
        Some(&wind_ipc::codec::encode_commit_text(
            &expected, None, false, true, false
        )),
        "换行改写与候选上屏同一道（push_no_key_ctx_action）"
    );
    assert_eq!(c.debug_candidate_count(), 0, "会话结束，也没进联想");
    assert_eq!(freq(), freq_before, "上屏的不是候选，不记词频");
    assert_eq!(
        c.auto_phrase_writes
            .load(std::sync::atomic::Ordering::Relaxed),
        writes_before,
        "不喂自动造词"
    );
    assert_eq!(*c.recent_commits.lock().unwrap(), history_before);
    // 会话结束后再上屏：放弃并 Toast。
    let _ = drain(&rx);
    c.tooltip_menu_action(MenuCmd::TooltipCommitSection);
    assert_eq!(toasts(&rx), [ABANDONED_TOAST]);
}
