use super::{cli_toast_message, clip_single_line, parse_hex_rgba};
use wind_ui_types::ToastKind;

/// 成功时用 CLI 自己打印的**最后一行**——那是 `dict import` 的总结行。
/// 不自造文案：core 再写一遍"导入成功"就会与 CLI 的说法漂移。
#[test]
fn success_uses_cli_last_line() {
    let (msg, kind) = cli_toast_message(
        "",
        "",
        Some(0),
        "✓ 用户词库: 新增 12 · 更新 3\n✓ 词频: 导入 40\n",
        "",
    )
    .expect("默认模式成功要弹");
    assert_eq!(msg, "✓ 词频: 导入 40");
    assert_eq!(kind, ToastKind::Success);
}

/// 显式 `ok=` 压过 CLI 输出。
#[test]
fn explicit_ok_text_wins() {
    let (msg, _) = cli_toast_message("on", "词库已更新", Some(0), "✓ 新增 12", "").unwrap();
    assert_eq!(msg, "词库已更新");
}

/// 成功但**没有任何输出**时仍要有话说——否则用户看到的还是"什么都没发生"。
#[test]
fn success_without_output_still_reports() {
    let (msg, _) = cli_toast_message("on", "", Some(0), "  \n", "").unwrap();
    assert_eq!(msg, "命令已完成");
}

/// 失败取 stderr 首行；没有 stderr 时退到 stdout；都没有才用退出码兜底。
/// 兜底带退出码是刻意的：光说"命令失败"对排查毫无价值。
#[test]
fn failure_prefers_stderr_then_stdout_then_code() {
    let (msg, kind) = cli_toast_message(
        "",
        "",
        Some(1),
        "",
        "读取 x.yaml 失败: 系统找不到指定的文件。",
    )
    .unwrap();
    assert_eq!(msg, "读取 x.yaml 失败: 系统找不到指定的文件。");
    assert_eq!(kind, ToastKind::Error);

    let (msg, _) = cli_toast_message("", "", Some(2), "用法: wind_input dict import …", "")
        .expect("退出码 2（用法错）也要报");
    assert!(msg.starts_with("用法"), "{msg}");

    let (msg, _) = cli_toast_message("", "", Some(3), "", "").unwrap();
    assert_eq!(msg, "命令失败（退出码 3）");
    // 被信号/强杀：没有退出码，也不能假装成功
    let (msg, kind) = cli_toast_message("", "", None, "", "").unwrap();
    assert_eq!(msg, "命令被中止");
    assert_eq!(kind, ToastKind::Error);
}

/// 三种模式的取舍：`off` 一律不弹；`error` 只在失败时弹（供 restart / config set
/// 这类服务侧已有提示的子命令，避免双提示）；默认两头都弹。
#[test]
fn toast_modes_gate_what_is_shown() {
    assert!(cli_toast_message("off", "", Some(0), "✓ ok", "").is_none());
    assert!(cli_toast_message("off", "", Some(1), "", "炸了").is_none());
    assert!(cli_toast_message("error", "", Some(0), "✓ ok", "").is_none());
    assert!(
        cli_toast_message("error", "", Some(1), "", "炸了").is_some(),
        "error 模式下失败仍必须弹——否则这个模式就等于 off"
    );
    // 显式 ok= 也压不过 off：模式是总闸。
    assert!(cli_toast_message("off", "词库已更新", Some(0), "", "").is_none());
}

/// 多行/超长输出压成单行并按**字符**截断（按字节切中文会切出半个字）。
#[test]
fn message_is_single_line_and_clipped_by_chars() {
    let long = "汉".repeat(300);
    let (msg, _) = cli_toast_message("on", &long, Some(0), "", "").unwrap();
    assert_eq!(msg.chars().count(), 201, "200 字 + 省略号");
    assert!(msg.ends_with('…'));

    assert_eq!(clip_single_line("a\r\nb\tc  d", 100), "a b c d");
    // 恰好等于上限不加省略号
    assert_eq!(clip_single_line("abcde", 5), "abcde");
    // 截断点落在空格之后：判据若按"原串非空白字符数"算会漏掉省略号。
    assert_eq!(clip_single_line("ab cd ef", 7), "ab cd e…");
}

#[test]
fn hex_color_parsing() {
    assert_eq!(parse_hex_rgba("#52C41A"), Some([0x52, 0xC4, 0x1A, 255]));
    assert_eq!(parse_hex_rgba("#52C41A80"), Some([0x52, 0xC4, 0x1A, 0x80]));
    // 非法一律 None → 落回按 kind 取色，而不是 panic 或黑色
    for bad in ["", "52C41A", "#52C", "#GGGGGG", "#52C41A8"] {
        assert_eq!(parse_hex_rgba(bad), None, "{bad}");
    }
    // ★ 多字节：`#中中` 恰好 6 **字节**，会穿过长度闸门，再按字节切片就在
    // 字符中间 panic。契约是"非法返回 None"，故这里必须也是 None。
    assert_eq!(parse_hex_rgba("#中中"), None);
    assert_eq!(parse_hex_rgba("#αβγδ"), None);
}
