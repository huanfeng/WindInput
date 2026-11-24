use super::*;
use wind_config::Config;

fn coord() -> std::sync::Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.input.default.chinese_mode = true;
    cfg.input.symbol.smart_mode = false;
    Coordinator::new_headless(cfg, None)
}

/// 透传用哪份集合，取决于当下标点态。必须写在 crate 内部：`tests/` 够不着 `state`，在那边
/// 写就是永远走中文态分支的假护栏。用 `'` 判：中文态转 `‘`/`’` 必须吃，英文态该透传。
#[test]
fn passthrough_set_follows_punct_mode() {
    let c = coord();
    let quote = KeyProbe::new(keymap::VK_QUOTE);

    c.state.lock().unwrap().chinese_punct = true;
    assert!(
        c.should_handle_key(&quote),
        "中文标点态下 `'` 要转中文引号，必须吃"
    );

    c.state.lock().unwrap().chinese_punct = false;
    assert!(
        !c.should_handle_key(&quote),
        "英文标点态下 `'` 产物即原样，吃了也只能原样吐回，应透传"
    );
}

/// 全角闸门优先于集合：两个标点态下都必须吃。
#[test]
fn full_width_overrides_passthrough_in_both_punct_modes() {
    let c = coord();
    let quote = KeyProbe::new(keymap::VK_QUOTE);
    for punct in [true, false] {
        {
            let mut s = c.state.lock().unwrap();
            s.chinese_punct = punct;
            s.full_width = true;
        }
        assert!(
            c.should_handle_key(&quote),
            "全角下 `'` 要出全角形，必须吃（chinese_punct={punct}）"
        );
    }
}

/// 没有前置闸门的宿主（Linux / macOS）把每个键都送进 `handle_key_event`：空闲 Esc 必须
/// 透传，否则浏览器 Vimium 一类收不到（GH#209）；有编码时仍是取消组合。
#[test]
fn escape_passes_through_without_a_session() {
    use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
    let esc = KeyEventData {
        key_code: keymap::VK_ESCAPE,
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    };
    let c = coord();
    let act = c.handle_key_event(&esc);
    assert!(
        matches!(act, KeyAction::PassThrough),
        "空闲 Esc 应透传，实际: {act:?}"
    );

    c.state.lock().unwrap().input_buffer = "ni".into();
    let act = c.handle_key_event(&esc);
    assert!(
        matches!(act, KeyAction::ClearComposition),
        "有编码时 Esc 应取消组合，实际: {act:?}"
    );
    assert!(c.state.lock().unwrap().input_buffer.is_empty());
}
