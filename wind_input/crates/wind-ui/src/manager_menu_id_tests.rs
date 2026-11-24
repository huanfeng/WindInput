use super::*;

/// `to_menu_id` 与 `from_menu_id` 是**两份手写的 match**，新增 MenuCmd 变体必须同时改两处。
/// 漏掉 `from_menu_id` 那侧的表现是「点了菜单毫无反应」且不留任何日志——极难联想到 id 映射。
/// 本测试锁住双向一致性，顺带也能抓出两个变体撞同一 id 的情况（撞号时 round-trip 必然不等）。
///
/// ⚠ 编译器抓不到下面这张列表的遗漏：新增 MenuCmd 变体时请手动补一行。
#[test]
fn menu_cmd_id_roundtrip() {
    let all = [
        MenuCmd::SchemaEnglish,
        MenuCmd::TogglePunct,
        MenuCmd::ToggleWidth,
        MenuCmd::ToggleS2t,
        MenuCmd::ToggleToolbar,
        MenuCmd::ReloadConfig,
        MenuCmd::RestartService,
        MenuCmd::OpenConfigDir,
        MenuCmd::OpenAppDir,
        MenuCmd::OpenLogDir,
        MenuCmd::OpenDictionary,
        MenuCmd::OpenSettings,
        MenuCmd::OpenAbout,
        MenuCmd::TakeScreenshot,
        MenuCmd::ScreenshotCandidateToClipboard,
        MenuCmd::ToggleInputDiagnostics,
        MenuCmd::TogglePasswordSuppress,
        MenuCmd::FirstShowMode(0),
        MenuCmd::FirstShowMode(1),
        MenuCmd::FirstShowMode(2),
        MenuCmd::FirstShowMode(3),
        MenuCmd::InitialMode(0),
        MenuCmd::InitialMode(1),
        MenuCmd::InitialMode(2),
        MenuCmd::InitialPunct(0),
        MenuCmd::InitialPunct(1),
        MenuCmd::InitialPunct(2),
        MenuCmd::StatusToggleAlways,
        MenuCmd::StatusResetPosition,
        MenuCmd::StatusScreenshot,
        MenuCmd::StatusTogglePinned,
        MenuCmd::StatusToggleShowOnFocus,
        MenuCmd::TooltipCopy,
        MenuCmd::TooltipScreenshot,
        MenuCmd::TooltipCopySection,
        MenuCmd::TooltipCommitSection,
        MenuCmd::TooltipCopyLine,
        MenuCmd::TooltipCommitLine,
        MenuCmd::InputDiagCopy,
        MenuCmd::InputDiagToggleFreeze,
        MenuCmd::InputDiagToggleTopmost,
        MenuCmd::InputDiagToggleSection(0),
        MenuCmd::InputDiagToggleSection(3),
        MenuCmd::AutoPairRule(0),
        MenuCmd::AutoPairRule(2),
        MenuCmd::CompatRuleEnabled(0),
        MenuCmd::CompatRuleEnabled(1),
        MenuCmd::PasswordForceEnglishRule(0),
        MenuCmd::PasswordForceEnglishRule(1),
        MenuCmd::PasswordForceEnglishRule(2),
        MenuCmd::AppSchemaRule(0),
        MenuCmd::AppSchemaRule(1),
        MenuCmd::AppSchemaRule(2),
        MenuCmd::AppSchemaRule(999),
        MenuCmd::StatusPositionRule(0),
        MenuCmd::StatusPositionRule(2),
        MenuCmd::StatusPositionRule(9),
        MenuCmd::StatusFallbackRule(0),
        MenuCmd::StatusFallbackRule(2),
        MenuCmd::StatusFallbackRule(9),
        MenuCmd::IconToggleSizeMarks,
        MenuCmd::IconBadgeStyle(0),
        MenuCmd::IconBadgeStyle(1),
        MenuCmd::SchemaSelect(0),
        MenuCmd::SchemaSelect(7),
        MenuCmd::ThemeSelect(3),
        MenuCmd::FilterMode(2),
        MenuCmd::ThemeStyle(1),
    ];
    for cmd in all {
        let id = MenuKind::Command(cmd).to_menu_id();
        let back = MenuKind::from_menu_id(id)
            .unwrap_or_else(|| panic!("{cmd:?} → id={id} 无法反解析（from_menu_id 漏了该 id）"));
        assert_eq!(
            back,
            MenuKind::Command(cmd),
            "{cmd:?} → id={id} → {back:?}：双向映射不一致（多为 id 撞号）"
        );
    }
}

/// 不可点击项（分隔符 / 子菜单 / 展示文本行）恒为 0，且 0 不得反解析成任何
/// 动作——否则点到分隔符/标题行会误触发某个命令。
#[test]
fn non_clickable_ids_are_inert() {
    assert_eq!(MenuKind::Separator.to_menu_id(), 0);
    assert_eq!(MenuKind::Submenu.to_menu_id(), 0);
    assert_eq!(MenuKind::Label.to_menu_id(), 0);
    assert!(MenuKind::from_menu_id(0).is_none());
}

/// 载荷是 `u8` 的号段只收 `base..=base+255`：`as u8` 会把越界 id 截断回段内
/// （`14256` → `PasswordForceEnglishRule(0)`），一个不认识的 id 就这样变成了「跟随全局」。
#[test]
fn u8_payload_ranges_reject_ids_past_255() {
    for base in [14000, 16000, 17000] {
        assert!(
            MenuKind::from_menu_id(base + 255).is_some(),
            "{base}+255 仍在段内"
        );
        assert!(
            MenuKind::from_menu_id(base + 256).is_none(),
            "{base}+256 超出 u8 载荷，不得截断成段内命令"
        );
    }
}
