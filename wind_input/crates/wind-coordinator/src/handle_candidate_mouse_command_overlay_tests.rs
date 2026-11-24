//! 鼠标点 `$CC` 命令候选（`select_candidate_at` 的命令分支）在 overlay 模式下必须与
//! 键盘选同一候选对齐：命令输入取**该模式自己的编码缓冲**、退出走该模式的退出函数。
//! 此前命令输入一律取主路 `input_buffer`（overlay 下恒空），复位只清拼音组合态。
use super::*;
use std::sync::Arc;
use wind_config::config::Config;

fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

/// 一条命令候选（无 `group_code` ⇒ 执行输入取当前编码缓冲）。headless 下 `self_weak`
/// 未装配，`spawn_command` 只告警跳过，不会真去执行。
fn command() -> Candidate {
    Candidate {
        text: "cmd".into(),
        is_command: true,
        phrase_template: "$CC(noop)".into(),
        ..Default::default()
    }
}

/// 命令输入按模式取缓冲，与各模式键盘出口传给 `overlay_commit_command` 的 `code` 一致。
#[test]
fn command_input_follows_active_mode_buffer() {
    let c = coord();
    let mut st = c.state.lock().unwrap();
    st.input_buffer = "main".into();
    st.temp_english_buffer = "te".into();
    st.temp_pinyin_buffer = "tp".into();
    st.mix_buffer = "mx".into();
    st.special_buffer = "sp".into();
    let cases = [
        (None, "main"),
        (Some(ModeKind::TempEnglish), "te"),
        (Some(ModeKind::TempPinyin), "tp"),
        (Some(ModeKind::Mix(0)), "mx"),
        (Some(ModeKind::Special(0)), "sp"),
        (Some(ModeKind::RareChar), "sp"),
        (Some(ModeKind::Reverse), "sp"),
    ];
    for (mode, want) in cases {
        st.active = mode;
        assert_eq!(c.command_input_code(&st), want, "{mode:?}");
    }
    // 辅助码按**来源**取：主输入路来的取 `input_buffer`，临拼来的取 `temp_pinyin_buffer`。
    st.active = Some(ModeKind::AuxCode);
    for (origin, want) in [(None, "main"), (Some(ModeKind::TempPinyin), "tp")] {
        st.aux_code = Some(aux_overlay(origin));
        assert_eq!(c.command_input_code(&st), want, "AuxCode origin={origin:?}");
    }
}

fn aux_overlay(origin: Option<ModeKind>) -> crate::handle_aux_code::AuxCodeOverlay {
    crate::handle_aux_code::AuxCodeOverlay {
        session: wind_aux_code::AuxCodeSession::new(Vec::new()),
        preedit_base: String::new(),
        preedit_prefix: String::new(),
        filter_options: Default::default(),
        origin,
    }
}

/// 辅助码（从临拼进来）里鼠标点**纯文本命令**：「是否纯文本」的求值输入必须与命令
/// 执行输入同源（`command_input_code` = 来源临拼缓冲）。曾取主路 `input_buffer`（此时恒空），
/// `type(code)` 求值成空串 ⇒ 判成「非纯文本」落进 `$CC` 分支，经 `cancel_session` 退出后
/// 返回 None；而键盘选同一候选走 `aux_code_committed` → 临拼出口，带回 `ClearComposition`。
#[test]
fn aux_code_from_temp_pinyin_text_command_uses_mode_buffer() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::AuxCode);
        st.aux_code = Some(aux_overlay(Some(ModeKind::TempPinyin)));
        st.temp_pinyin_buffer = "tp".into();
        st.temp_pinyin_cursor = 2;
        st.candidates = vec![Candidate {
            text: "标签".into(),
            is_command: true,
            phrase_template: r#"$CC("标签", type(code))"#.into(),
            ..Default::default()
        }];
    }
    match c.select_candidate_at(0) {
        Some(KeyAction::ClearComposition) => {}
        other => panic!("应与键盘同走来源临拼的命令出口，实际: {other:?}"),
    }
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.aux_code.is_none());
    assert!(st.temp_pinyin_buffer.is_empty());
}

/// 生僻字（经 z 夺取进来）里鼠标点**普通候选**：走键盘同一出口
/// `commit_special_candidate`，整串上屏并带回动作；回退登记与编码缓冲不得残留
/// （`rewind` 留着 ⇒ 下次缓冲恰等于残余码时退格会被误判成「退回夺取边界」）。
#[test]
fn rare_char_plain_click_exits_cleanly() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::RareChar);
        st.special_buffer = "gg".into();
        st.special_prefix = "z".into();
        st.rewind = Some(crate::pipeline::Rewind {
            snapshot: "z".into(),
            host_text: "g".into(),
            origin: crate::pipeline::RewindOrigin::Normal,
        });
        st.candidates = vec![Candidate {
            text: "玨".into(),
            ..Default::default()
        }];
    }
    match c.select_candidate_at(0) {
        Some(KeyAction::InsertText { text, .. }) => assert_eq!(text, "玨"),
        other => panic!("生僻字点选应带回上屏动作（专用出口），实际: {other:?}"),
    }
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.rewind.is_none(), "夺取回退登记不得残留");
    assert!(st.special_buffer.is_empty(), "生僻字编码缓冲不得残留");
    assert!(st.special_prefix.is_empty());
    assert!(st.overlay_spec.is_none(), "overlay 段快照随模式丢弃");
}

/// 快捷输入里鼠标点普通候选（整串消费）：走 `mix_select_at` 上屏，`mix_buffer` /
/// 前缀 / 已转换前缀不得残留。
#[test]
fn mix_plain_click_exits_cleanly() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::Mix(0));
        st.mix_buffer = "ab".into();
        st.mix_prefix = ";".into();
        st.committed_text = "前".into();
        st.candidates = vec![Candidate {
            text: "阿".into(),
            ..Default::default()
        }];
    }
    let _ = c.select_candidate_at(0);
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.mix_buffer.is_empty(), "快捷输入缓冲不得残留");
    assert!(st.mix_prefix.is_empty());
    assert!(st.committed_text.is_empty(), "文本透镜的已转换前缀不得残留");
}

/// 临拼里鼠标点**组折叠候选**：由临拼自己的出口展开（补全写进 `temp_pinyin_buffer`），
/// 不得先落到通用的 `complete_to_group_code`——那会把组码写进主路 `input_buffer`、
/// 查主方案，模式缓冲不动，下一键就串台。
#[test]
fn temp_pinyin_group_click_expands_in_mode() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::TempPinyin);
        st.temp_pinyin_buffer = "z".into();
        st.temp_pinyin_cursor = 1;
        st.candidates = vec![Candidate {
            text: "组".into(),
            is_group: true,
            group_code: "zzbd".into(),
            ..Default::default()
        }];
    }
    let _ = c.select_candidate_at(0);
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, Some(ModeKind::TempPinyin), "展开组不退出模式");
    assert_eq!(st.temp_pinyin_buffer, "zzbd", "组码应写进临拼缓冲");
    assert!(st.input_buffer.is_empty(), "主路缓冲不得被写入组码");
}

/// Unicode 模式（键盘侧不按序号选词）里鼠标点候选：仍走通用分支「整串上屏 + 退出」，
/// 并带回上屏动作——移动端 headless 下 push 无消费端，返回 None 会被当成 passthrough。
#[test]
fn unicode_plain_click_commits_and_exits() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::Unicode);
        st.unicode_buffer = "u+4e2d".into();
        st.unicode_cursor = st.unicode_buffer.len();
        st.candidates = vec![Candidate {
            text: "中".into(),
            ..Default::default()
        }];
    }
    match c.select_candidate_at(0) {
        Some(KeyAction::InsertText { text, .. }) => assert_eq!(text, "中"),
        other => panic!("通用分支应带回上屏动作，实际: {other:?}"),
    }
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.unicode_buffer.is_empty(), "Unicode 缓冲不得残留");
}

/// 辅助码（从临拼进来）里鼠标点命令候选：overlay 与来源临拼的缓冲一并清掉。
/// 旧写法只清拼音组合态 + `active = None`，`aux_code` 与 `temp_pinyin_buffer` 都残留。
#[test]
fn aux_code_command_click_exits_cleanly() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::AuxCode);
        st.aux_code = Some(aux_overlay(Some(ModeKind::TempPinyin)));
        st.temp_pinyin_buffer = "li".into();
        st.candidates = vec![command()];
    }
    let _ = c.select_candidate_at(0);
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.aux_code.is_none(), "辅助码 overlay 不得残留");
    assert!(st.temp_pinyin_buffer.is_empty(), "来源临拼的缓冲不得残留");
}

/// 临英里鼠标点命令候选：退出走 `exit_temp_english`，缓冲 / 前缀不残留。
#[test]
fn temp_english_command_click_exits_cleanly() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::TempEnglish);
        st.temp_english_buffer = "Hel".into();
        st.temp_english_cursor = 3;
        st.temp_english_prefix = "`".into();
        st.candidates = vec![command()];
    }
    assert!(c.select_candidate_at(0).is_none(), "命令分支不带回上屏动作");
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.temp_english_buffer.is_empty(), "临英缓冲不得残留");
    assert!(st.temp_english_prefix.is_empty(), "临英前缀不得残留");
    assert_eq!(st.temp_english_cursor, 0);
    assert!(st.candidates.is_empty());
}

/// 快捷输入里鼠标点命令候选：`exit_mix_mode` 清掉前缀与已转换段。
#[test]
fn mix_command_click_exits_cleanly() {
    let c = coord();
    {
        let mut st = c.state.lock().unwrap();
        st.active = Some(ModeKind::Mix(0));
        st.mix_buffer = "ab".into();
        st.mix_prefix = ";".into();
        st.candidates = vec![command()];
    }
    let _ = c.select_candidate_at(0);
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None);
    assert!(st.mix_buffer.is_empty());
    assert!(st.mix_prefix.is_empty(), "快捷输入前缀不得残留");
}
