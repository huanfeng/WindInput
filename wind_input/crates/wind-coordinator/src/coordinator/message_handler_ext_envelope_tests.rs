//! 扩展信封 `pos.*` / `shot.*` 的 body 解析与文案，以及滚轮的高亮移动。
use super::*;

fn coord() -> Arc<Coordinator> {
    Coordinator::new_headless(Config::default(), None)
}

#[test]
fn host_render_placed_log_levels() {
    // setup：INFO，不带 prev。
    let (warn, line) = host_render_placed_log(
        br#"{"pid":7,"trigger":"setup","probed":1,"requested":1,"actual":1,"owner":false}"#,
    )
    .unwrap();
    assert!(!warn);
    assert!(line.contains("pid=7 trigger=setup probed=1 requested=1 actual=1 owner=false"));
    assert!(!line.contains("prev_"));
    assert!(!line.contains("pass="), "旧 DLL 不报穿透窗口，不编");

    // 新 DLL 报穿透装饰窗口建没建出来。
    let (_, line) = host_render_placed_log(
        br#"{"pid":7,"trigger":"setup","probed":1,"requested":1,"actual":1,"owner":true,"pass":false}"#,
    )
    .unwrap();
    assert!(line.contains("owner=true pass=false"), "{line}");
    let (_, line) = host_render_placed_log(
        br#"{"pid":7,"trigger":"setup","probed":1,"requested":1,"actual":1,"owner":true,"pass":true}"#,
    )
    .unwrap();
    assert!(
        line.contains("pass=true") && !line.contains("合回"),
        "{line}"
    );

    // recheck 且 band 变了（开机预启动那条）：WARN。
    let (warn, line) = host_render_placed_log(
        br#"{"pid":7,"trigger":"recheck","probed":13,"requested":13,"actual":13,"owner":true,
                "prev_probed":0,"prev_requested":1,"prev_actual":1,"prev_owner":false}"#,
    )
    .unwrap();
    assert!(warn, "band 判错应升 WARN");
    assert!(line.contains("prev_requested=1") && line.contains("band 判错"));

    // recheck 但 band 本来就对、只缺 owner：INFO。
    let (warn, line) = host_render_placed_log(
        br#"{"pid":7,"trigger":"recheck","probed":13,"requested":13,"actual":13,"owner":true,
                "prev_probed":13,"prev_requested":13,"prev_actual":13,"prev_owner":false}"#,
    )
    .unwrap();
    assert!(!warn, "只缺 owner 不该报成 band 判错");
    assert!(line.contains("缺 owner"));

    // recheck、band 对、有 owner：只能是窗口失效。
    let (warn, line) = host_render_placed_log(
        br#"{"pid":7,"trigger":"recheck","probed":13,"requested":13,"actual":13,"owner":true,
                "prev_probed":13,"prev_requested":13,"prev_actual":13,"prev_owner":true}"#,
    )
    .unwrap();
    assert!(!warn);
    assert!(line.contains("已失效"));

    assert_eq!(host_render_placed_log(b"not json"), None);
    assert_eq!(host_render_placed_log(b"[1]"), None);
}

#[test]
fn decode_ext_host_display_fields_are_independent() {
    let d = |b: &[u8]| decode_ext_host_display(b);
    assert_eq!(
        d(br#"{"caret_trusted":true}"#),
        Some(HostDisplay {
            caret_free: None,
            caret_trusted: Some(true),
            inline_preedit: None,
            scale: None
        })
    );
    assert_eq!(
        d(br#"{"caret_free":true,"scale":1.5}"#),
        Some(HostDisplay {
            caret_free: Some(true),
            caret_trusted: None,
            inline_preedit: None,
            scale: Some(1.5)
        })
    );
    // 缺省 = 不改现值
    assert_eq!(
        d(br#"{"scale":2}"#),
        Some(HostDisplay {
            caret_free: None,
            caret_trusted: None,
            inline_preedit: None,
            scale: Some(2.0)
        })
    );
    assert_eq!(
        d(b"{}"),
        Some(HostDisplay {
            caret_free: None,
            caret_trusted: None,
            inline_preedit: None,
            scale: None
        })
    );
    // 一个字段坏了不连累另一个
    assert_eq!(
        d(br#"{"caret_free":1,"scale":1.25}"#),
        Some(HostDisplay {
            caret_free: None,
            caret_trusted: None,
            inline_preedit: None,
            scale: Some(1.25)
        })
    );
    assert_eq!(
        d(br#"{"caret_free":false,"scale":"2"}"#),
        Some(HostDisplay {
            caret_free: Some(false),
            caret_trusted: None,
            inline_preedit: None,
            scale: None
        })
    );
    // 非正数的缩放当缺省
    assert_eq!(
        d(br#"{"scale":0}"#),
        Some(HostDisplay {
            caret_free: None,
            caret_trusted: None,
            inline_preedit: None,
            scale: None
        })
    );
    assert_eq!(
        d(br#"{"scale":-1}"#),
        Some(HostDisplay {
            caret_free: None,
            caret_trusted: None,
            inline_preedit: None,
            scale: None
        })
    );
    for bad in [&b""[..], b"not json", b"[1]", b"3"] {
        assert_eq!(d(bad), None, "body={:?} 应被拒", bad);
    }
    // inline_preedit（Linux：客户端有没有 Preedit 能力位）同样独立解析
    assert_eq!(
        d(br#"{"caret_free":false,"inline_preedit":false}"#),
        Some(HostDisplay {
            caret_free: Some(false),
            caret_trusted: None,
            inline_preedit: Some(false),
            scale: None
        })
    );
}

#[test]
fn decodes_well_formed_point() {
    assert_eq!(
        decode_ext_point(br#"{"x":123,"y":-456}"#),
        Some((123, -456))
    );
    // 多余字段照常忽略——JSON body 的向前兼容就靠这条。
    assert_eq!(
        decode_ext_point(br#"{"x":1,"y":2,"screen":"builtin"}"#),
        Some((1, 2))
    );
}

/// 滚轮 = 上下键调整高亮项，到页边界翻到相邻页。
///
/// 回归意义：`handle_candidate_scroll` 长期是 trait 上的空实现，Windows 的
/// host-render DLL 一直在发这个帧、服务端收下什么也不做——滚轮在两个平台都无效。
#[test]
fn scroll_moves_highlight_and_crosses_pages() {
    use wind_candidate::Candidate;
    let c = coord();
    let per_page = {
        let mut s = c.state.lock().unwrap();
        s.candidates = (0..12)
            .map(|i| Candidate {
                text: i.to_string(),
                ..Default::default()
            })
            .collect();
        s.selected_index = 0;
        s.current_page = 0;
        drop(s);
        c.per_page(None)
    };
    assert!((2..12).contains(&per_page), "本用例要求每页 2..12 项");

    // 下滚一格 → 高亮下移一项（不是翻一页）
    c.handle_candidate_scroll(-120);
    assert_eq!(c.state.lock().unwrap().selected_index, 1);

    // 一路滚到页尾再一格 → 跨到下一页首项
    for _ in 0..(per_page - 1) {
        c.handle_candidate_scroll(-120);
    }
    {
        let s = c.state.lock().unwrap();
        assert_eq!(s.current_page, 1, "页尾再下滚应翻到下一页");
        assert_eq!(s.selected_index, 0, "跨页后高亮落在首项");
    }

    // 上滚回卷到上一页末项
    c.handle_candidate_scroll(120);
    {
        let s = c.state.lock().unwrap();
        assert_eq!(s.current_page, 0);
        assert_eq!(s.selected_index, per_page - 1);
    }
}

/// 触控板一次轻扫的 delta 可能不足一格（<120）——整除会得 0，滚轮就"滚不动"。
#[test]
fn scroll_with_sub_notch_delta_still_moves_one() {
    use wind_candidate::Candidate;
    let c = coord();
    {
        let mut s = c.state.lock().unwrap();
        s.candidates = (0..5)
            .map(|i| Candidate {
                text: i.to_string(),
                ..Default::default()
            })
            .collect();
    }
    c.handle_candidate_scroll(-13);
    assert_eq!(c.state.lock().unwrap().selected_index, 1);
}

/// 惯性滚动一次可能带来极大的 delta；不设上限会一口气跳过几十项并疯狂重绘。
#[test]
fn scroll_is_capped_per_event() {
    use wind_candidate::Candidate;
    let c = coord();
    {
        let mut s = c.state.lock().unwrap();
        s.candidates = (0..200)
            .map(|i| Candidate {
                text: i.to_string(),
                ..Default::default()
            })
            .collect();
    }
    c.handle_candidate_scroll(-120 * 50);
    let s = c.state.lock().unwrap();
    let moved = s.current_page * c.per_page(None) + s.selected_index;
    assert_eq!(moved, 5, "单次事件最多移动 MAX_NOTCHES 项");
}

/// 无候选时不得有任何动作（也不该 panic）。
#[test]
fn scroll_without_candidates_is_noop() {
    let c = coord();
    c.handle_candidate_scroll(-120);
    assert_eq!(c.state.lock().unwrap().selected_index, 0);
}

/// 缺字段 / 非整数 / 越界 / 不是 JSON —— 一律 None。
///
/// 关键在于**不能取 0 兜底**：`(0, 0)` 会被当成合法坐标落盘成 custom_x/y，
/// 候选窗下次就跑到屏幕左上角，而用户只是拖了一下。
#[test]
fn rejects_malformed_bodies() {
    for bad in [
        &br#"{"x":1}"#[..],            // 缺 y
        br#"{"y":1}"#,                 // 缺 x
        br#"{"x":1.5,"y":2}"#,         // 非整数
        br#"{"x":"1","y":"2"}"#,       // 字符串
        br#"{"x":99999999999,"y":0}"#, // 越出 i32
        br#"[1,2]"#,                   // 不是对象
        b"not json",
        b"",
    ] {
        assert_eq!(decode_ext_point(bad), None, "body={:?} 应被拒", bad);
    }
}
