const SRC: &str = include_str!("handle_mode.rs");

/// 截取 `fn <name>` 之后括号配平的函数体，并剥掉整行注释。
///
/// 本文件的注释里就在解释「为什么不能读那个开关」，纯文本扫描会把解释本身判成违规；
/// 判据要落在代码上，不能落在讲述判据的文字上。
fn body_of(name: &str) -> String {
    let at = SRC
        .find(&format!("fn {name}"))
        .unwrap_or_else(|| panic!("源码里找不到 fn {name}（改名了？守卫需同步更新）"));
    let open = SRC[at..]
        .find('{')
        .unwrap_or_else(|| panic!("fn {name} 没有函数体"))
        + at;
    let mut depth = 0usize;
    for (i, ch) in SRC[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return SRC[open..open + i + 1]
                        .lines()
                        .filter(|l| !l.trim_start().starts_with("//"))
                        .collect::<Vec<_>>()
                        .join(
                            "
",
                        );
                }
            }
            _ => {}
        }
    }
    panic!("fn {name} 的花括号不配平");
}

/// 幂等分支不得绕过收尾。
///
/// 「切到某方案」的语义是「我要用这个方案打字」。已是目标方案时，要发生的恰恰只剩收尾
/// （归位中文、取消大写）；早退就是「英文半角态/大写态下按方案热键毫无反应」。
/// 修好一个动作的收尾之后，要回头找有没有提前返回的分支绕过它。
#[test]
fn schema_switch_entries_do_not_early_return_past_the_finish() {
    for name in ["switch_schema_by_id", "select_schema"] {
        let body = body_of(name);
        assert!(
            body.contains("finish_user_schema_switch"),
            "{name} 必须走统一收尾"
        );
        // 幂等分支不能是裸 return：要么与切换尝试共用一个条件（都落到 finish），
        // 要么单独做状态归位。判据只认 `active == id ||` 这半边，右半边写什么不参与，
        // 免得守卫比被守的代码更脆。
        let idempotent_handled = body.contains("restore_state_for_same_schema")
            || body.contains("active_schema_id() == id ||");
        assert!(
            idempotent_handled,
            "{name}: 「已是该方案」时既没归位也没走收尾 ⇒ 英文态/大写态下按方案热键\
                 毫无反应。要么调 restore_state_for_same_schema，要么与切换尝试共用条件"
        );
    }
}

/// 切方案的状态归位不得受 CapsLock 那个开关（`input.capslock.cancel_on_mode_switch`）门控。
///
/// 该开关出厂 `false`，被它门控时英文半角态/大写态按方案直达热键，方案换了用户却看不到
/// 任何变化（那两种状态下按键不进引擎：英文半角在 `handle_key_event` 透传，大写被 C++ 的
/// `capsLockLetterPassthrough` 透传）。
///
/// 一个动作的语义前提不可配置，可配置的只能是副作用；那个开关的作用域只有切中英模式。
#[test]
fn finish_user_schema_switch_is_not_gated_by_capslock_option() {
    let body = body_of("finish_user_schema_switch");
    assert!(
        !body.contains("cancel_on_mode_switch"),
        "切方案的归位不得读 input.capslock.cancel_on_mode_switch——它出厂关闭，\
             一读就等于「英文态/大写态下方案切换毫无反应」"
    );
    assert!(
        !body.contains("cancel_caps_on_switch"),
        "切方案取消大写要走无条件的 force_cancel_caps_lock；cancel_caps_on_switch 带开关判定"
    );
    assert!(
        body.contains("force_cancel_caps_lock"),
        "大写开着时不取消，切完方案照样打大写英文——用户看到的仍是「切换不生效」"
    );
    assert!(
        body.contains("state.chinese_mode = true"),
        "英文半角态不归位中文，按键全程原样透传，新方案根本不参与出字"
    );
}

/// 三个方案切换入口（托盘 / 直达热键 / 循环键）必须共用同一条收尾。
///
/// 各自手写时持久化、归位中文、取消大写、清 preedit 四件事的组合会漂移，表现为
/// 「托盘切得动、热键切不动」；共用之后同一个 bug 不会只修一半。
#[test]
fn all_schema_switch_entries_share_one_finish() {
    let body = body_of("select_schema");
    assert!(
        body.contains("finish_user_schema_switch"),
        "托盘选方案必须走统一收尾，不得再手写一份"
    );
    assert!(
        !body.contains("chinese_mode = true"),
        "手写归位＝又一份会漂移的收尾；归位属于 finish_user_schema_switch"
    );
    for name in ["switch_schema_by_id", "cycle_schema"] {
        assert!(
            body_of(name).contains("finish_user_schema_switch"),
            "{name} 也必须走统一收尾"
        );
    }
}

/// 「未上屏编码」的处置必须发生在收尾之前，且每个入口都得做。
///
/// - 漏做：切了方案，编码原样残留在宿主里；
/// - 晚于 finish：`take_input_on_schema_switch` 的上屏走 `record_commit`，取的是当前活跃方案，
///   排在切换之后这串码会静默记到新方案头上。
#[test]
fn schema_switch_takes_input_before_finish() {
    for name in ["switch_schema_by_id", "cycle_schema", "select_schema"] {
        let body = body_of(name);
        let take = body
            .find("take_input_on_schema_switch")
            .unwrap_or_else(|| panic!("{name} 必须处置切换前的未上屏编码，否则宿主组合区会残留"));
        let finish = body
            .find("finish_user_schema_switch")
            .unwrap_or_else(|| panic!("{name} 必须走统一收尾"));
        assert!(
            take < finish,
            "{name}：编码处置要排在收尾之前，否则上屏记账会落到新方案头上"
        );
    }
}

/// 收尾里不得再手写清缓冲：「上屏还是丢弃」只能有一处真相。
///
/// 裸 `clear()` 看着无害（幂等），但给 finish 加新调用方而那条路没走 take 时，编码会静默丢掉。
#[test]
fn finish_does_not_clear_input_itself() {
    let body = body_of("finish_user_schema_switch");
    for pat in [
        "input_buffer.clear()",
        "candidates.clear()",
        "preedit.clear()",
    ] {
        assert!(
            !body.contains(pat),
            "finish_user_schema_switch 不得手写 {pat}：编码处置归 take_input_on_schema_switch"
        );
    }
}
