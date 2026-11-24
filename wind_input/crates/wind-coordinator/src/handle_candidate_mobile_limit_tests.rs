use crate::coordinator::Coordinator;
use wind_engine::engine::EngineType;

#[test]
fn mobile_pinyin_single_letter_is_capped_only_there() {
    let lim = Coordinator::main_path_candidate_limit;
    let py = Some(EngineType::Pinyin);
    let mobile = Some(super::MOBILE_PINYIN_SINGLE_LIMIT);
    assert_eq!(lim(py, "b", mobile), super::MOBILE_PINYIN_SINGLE_LIMIT);
    assert_eq!(
        lim(py, "b", None),
        Coordinator::initial_candidate_limit_of(py, "b"),
        "桌面不收紧"
    );
    assert_eq!(
        lim(py, "bz", mobile),
        Coordinator::initial_candidate_limit_of(py, "bz"),
        "两字母起不收紧"
    );
    for other in [
        EngineType::English,
        EngineType::CodeTable,
        EngineType::Mixed,
    ] {
        assert_eq!(
            lim(Some(other), "t", mobile),
            Coordinator::initial_candidate_limit_of(Some(other), "t"),
            "{other:?} 不受影响（英文 t 曾因上限太小打不出 then）"
        );
    }
}

#[test]
fn cut_position_only_for_mobile_pinyin_with_cursor_inside() {
    assert_eq!(
        crate::handle_candidate::cut_position(true, "fangxiang", 4, true),
        Some(4)
    );
    assert_eq!(
        crate::handle_candidate::cut_position(true, "fangxiang", 9, true),
        None,
        "光标在末尾 = 整串"
    );
    assert_eq!(
        crate::handle_candidate::cut_position(true, "fangxiang", 0, true),
        None,
        "光标在开头无前缀可查"
    );
    assert_eq!(
        crate::handle_candidate::cut_position(false, "fangxiang", 4, true),
        None,
        "电脑端不改既有行为"
    );
    assert_eq!(
        crate::handle_candidate::cut_position(true, "fangxiang", 4, false),
        None,
        "码表 / overlay 不进"
    );
    assert_eq!(
        crate::handle_candidate::cut_position(true, "你好", 1, true),
        None,
        "非字符边界"
    );
}

#[test]
fn tail_after_letters_skips_separators() {
    use crate::handle_candidate::tail_after_letters;
    assert_eq!(tail_after_letters("fang'xiang", 4), "xiang");
    assert_eq!(
        tail_after_letters("fang'xiang", 6),
        "ang",
        "光标在音节中间：余下半个音节"
    );
    assert_eq!(tail_after_letters("fang'xiang", 9), "");
    assert_eq!(tail_after_letters("ni'hao'a", 2), "hao'a");
}
