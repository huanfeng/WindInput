use super::*;

const IS_PASSWORD: u64 = 1 << 31;
const IS_NUMERIC_PASSWORD: u64 = 1 << 63;

#[test]
fn reason_none_when_clean() {
    assert_eq!(reason_from(false, 0), InputDiagReason::None);
}

#[test]
fn compartment_takes_precedence_over_mask() {
    // disabled=true 一律 CompartmentDisabled，即便 mask 有密码位
    assert_eq!(
        reason_from(true, IS_PASSWORD),
        InputDiagReason::CompartmentDisabled
    );
}

#[test]
fn password_and_numeric_from_mask() {
    assert_eq!(
        reason_from(false, IS_PASSWORD),
        InputDiagReason::InputScopePassword
    );
    assert_eq!(
        reason_from(false, IS_NUMERIC_PASSWORD),
        InputDiagReason::NumericPassword
    );
}

#[test]
fn is_password_scope_covers_both_bits() {
    assert!(is_password_scope(IS_PASSWORD));
    assert!(is_password_scope(IS_NUMERIC_PASSWORD));
    assert!(!is_password_scope(0));
}

#[test]
fn report_reason_separates_context_disabled_from_host_password() {
    // DLL 折位后 mask 带 bit31，reason 字节 4：宿主并没报密码。
    let folded = 0x8000_0001;
    assert_eq!(
        reason_from_report(false, REPORT_REASON_CONTEXT_DISABLED, folded),
        InputDiagReason::ContextDisabled
    );
    // 宿主真报了 IS_PASSWORD：DLL 发 reason=2，仍是密码。
    assert_eq!(
        reason_from_report(false, 2, folded),
        InputDiagReason::InputScopePassword
    );
    // 旧 DLL 永远不发 4：退回按 mask 推导。
    assert_eq!(
        reason_from_report(false, 0, folded),
        InputDiagReason::InputScopePassword
    );
    // 线程级禁用优先级最高，不被 4 抢走。
    assert_eq!(
        reason_from_report(true, REPORT_REASON_CONTEXT_DISABLED, folded),
        InputDiagReason::CompartmentDisabled
    );
    // Chromium 密码框：context 禁用 + 宿主原始 scope 带 IS_PRIVATE ⇒ 按密码框展示。
    assert_eq!(
        reason_from_report(
            false,
            REPORT_REASON_CONTEXT_DISABLED,
            0x8000_0000 | (1 << 61)
        ),
        InputDiagReason::ContextPassword
    );
    // 谎报 4 但 mask 里没有密码位：不采信（抑制与展示不应自相矛盾）。
    assert_eq!(
        reason_from_report(false, REPORT_REASON_CONTEXT_DISABLED, 0x1),
        InputDiagReason::None
    );
}

#[test]
fn display_mask_strips_folded_password_bit_only_for_context_disabled() {
    assert_eq!(
        display_mask(InputDiagReason::ContextDisabled, 0x8000_0001),
        0x1
    );
    assert_eq!(
        display_mask(InputDiagReason::ContextPassword, 0x8000_0000 | (1 << 61)),
        1 << 61
    );
    assert_eq!(
        display_mask(InputDiagReason::InputScopePassword, 0x8000_0001),
        0x8000_0001
    );
}
