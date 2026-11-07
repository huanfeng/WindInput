//! 二进制协议编解码器
//!
//! 与 Go 版本 `wind_input/internal/ipc/binary_codec.go` 对齐。

use crate::protocol::overlay::{OVERLAY_KIND_SOFTKEYBOARD, OVERLAY_KIND_TOOLBAR};
use crate::protocol::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("buffer too short: need {need}, got {got}")]
    BufferTooShort { need: usize, got: usize },
    #[error("unsupported protocol version: 0x{version:04X}")]
    UnsupportedVersion { version: u16 },
    #[error("payload too large: {size} > {max}")]
    PayloadTooLarge { size: usize, max: usize },
    #[error("invalid UTF-8 in payload: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

/// 最大载荷大小 (16MB，与 RPC 一致)
pub const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// 从字节流解码 IPC Header
pub fn decode_header(buf: &[u8]) -> Result<IpcHeader, CodecError> {
    if buf.len() < IpcHeader::SIZE {
        return Err(CodecError::BufferTooShort {
            need: IpcHeader::SIZE,
            got: buf.len(),
        });
    }
    let header = IpcHeader::from_bytes(&buf[..8].try_into().unwrap());

    // 版本兼容性检查：只检查主版本号
    let major = header.major_version();
    let expected_major = PROTOCOL_VERSION & VERSION_MASK;
    if major != expected_major {
        return Err(CodecError::UnsupportedVersion {
            version: header.version,
        });
    }

    // 载荷大小检查
    if header.length as usize > MAX_PAYLOAD_SIZE {
        return Err(CodecError::PayloadTooLarge {
            size: header.length as usize,
            max: MAX_PAYLOAD_SIZE,
        });
    }

    Ok(header)
}

/// 编码 IPC Header 到字节数组
pub fn encode_header(header: &IpcHeader) -> [u8; 8] {
    header.to_bytes()
}

/// 从载荷字节解码 KeyPayload
pub fn decode_key_payload(payload: &[u8]) -> Result<KeyPayload, CodecError> {
    KeyPayload::from_bytes(payload).ok_or(CodecError::BufferTooShort {
        need: KeyPayload::LEGACY_SIZE,
        got: payload.len(),
    })
}

/// 从载荷字节解码 FocusGainedPayload
pub fn decode_focus_gained(payload: &[u8]) -> Result<FocusGainedPayload, CodecError> {
    FocusGainedPayload::from_bytes(payload).ok_or(CodecError::BufferTooShort {
        need: FocusGainedPayload::SIZE,
        got: payload.len(),
    })
}

/// 编码扩展信封 [`CMD_EXT`]：`kindLen u32 + kind + bodyLen u32 + body`。
///
/// `body` 不透明（JSON 或二进制块均可），本层不解析——见 `CMD_EXT` 文档的两档划分。
pub fn encode_ext(kind: &str, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + kind.len() + body.len());
    p.extend_from_slice(&(kind.len() as u32).to_le_bytes());
    p.extend_from_slice(kind.as_bytes());
    p.extend_from_slice(&(body.len() as u32).to_le_bytes());
    p.extend_from_slice(body);
    frame(CMD_EXT, p)
}

/// 解扩展信封载荷 → `(kind, body)`。
///
/// 越界 / 非法 UTF-8 的 kind 一律视为**解析失败**返回 `None`，由调用方按「未知消息」忽略。
/// 刻意不做「尽力而为」的部分解析：一个截断的信封说明对端有 bug 或版本不匹配，
/// 拿半截 kind 去分发只会把错误引向更难查的地方。
pub fn decode_ext(payload: &[u8]) -> Option<(&str, &[u8])> {
    let kind_len = u32::from_le_bytes(payload.get(0..4)?.try_into().ok()?) as usize;
    let kind = std::str::from_utf8(payload.get(4..4 + kind_len)?).ok()?;
    let off = 4 + kind_len;
    let body_len = u32::from_le_bytes(payload.get(off..off + 4)?.try_into().ok()?) as usize;
    let body = payload.get(off + 4..off + 4 + body_len)?;
    Some((kind, body))
}

/// 解 CMD_FOCUS_GAINED 载荷尾部的 darwin bundleID 段（`bundleIdLen:u32 + utf8`，偏移 39）。
///
/// 该段是 macOS `.app` 专属：宿主 app 的 bundle id，服务端小写后当作「进程名」，供
/// compat.toml 规则匹配与 per-app 中英记忆使用（macOS 上无法像 Windows 那样由服务进程
/// `OpenProcess` 反查，只能由 `.app` 随焦点事件带上来）。
///
/// Windows DLL 不发该段；段缺失 / 长度越界 / 非法 UTF-8 一律返回空串——「取不到宿主名」
/// 与「宿主名为空」在下游是同一语义（跳过按应用逻辑），不必区分。
/// 读取 `off` 处的一个「`u32` 长度 + UTF-8 内容」变长段。
///
/// 返回 `(内容, 下一段起始偏移)`；段残缺（长度域不全、内容不足、非法 UTF-8）一律
/// 返回空串。**残缺不是错误**——变长段是纯追加的可选信息，旧 DLL 压根不发。
/// 第二个返回值为 `None` 表示「本段都没走完，后面不可能有东西」，用于串联下一段。
fn read_len_prefixed(payload: &[u8], off: usize) -> (&str, Option<usize>) {
    // 偏移与长度都来自对端，全程 checked：32 位目标上 `start + n` 可能回绕成一个「合法」的
    // 小区间，解出错位的垃圾而不是空串。
    let Some(start) = off.checked_add(4) else {
        return ("", None);
    };
    let Some(len_bytes) = payload.get(off..start) else {
        return ("", None);
    };
    let n = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
    let Some(end) = start.checked_add(n) else {
        return ("", None);
    };
    match payload.get(start..end) {
        Some(b) => (std::str::from_utf8(b).unwrap_or(""), Some(end)),
        None => ("", None),
    }
}

pub fn decode_focus_gained_bundle_id(payload: &[u8]) -> &str {
    read_len_prefixed(payload, FocusGainedPayload::VAR_SECTION_OFFSET).0
}

/// 焦点所在**顶层窗口**的类名；旧 DLL / 段缺失时返回空串。
///
/// ⚠ 必须**顺序走过 bundleId 段**再读，不能用固定偏移：bundleId 变长，macOS 上非空。
/// Windows DLL 发 `bundleIdLen=0` 占位，故两平台共用这一条走法。
///
/// 空串的语义是「不知道焦点在哪」而非「窗口不在清单里」——消费端
/// (`AppCompat::initial_mode_applies_to_window`) 据此保持现状，不按 per-app 规则重算。
pub fn decode_focus_gained_window_class(payload: &[u8]) -> &str {
    match read_len_prefixed(payload, FocusGainedPayload::VAR_SECTION_OFFSET) {
        (_, Some(next)) => read_len_prefixed(payload, next).0,
        (_, None) => "",
    }
}

/// 焦点所在**顶层窗口**的标题（变长段 ③）；旧 DLL / 服务端未推开采集 / 段缺失时返回空串。
///
/// 同样必须**顺序走过**前两段（bundleId、windowClass）再读。空串与「窗口没有标题」同义：
/// 带 `title` 条件的规则一律不命中。
pub fn decode_focus_gained_window_title(payload: &[u8]) -> &str {
    let Some(class_off) = read_len_prefixed(payload, FocusGainedPayload::VAR_SECTION_OFFSET).1
    else {
        return "";
    };
    match read_len_prefixed(payload, class_off) {
        (_, Some(next)) => read_len_prefixed(payload, next).0,
        (_, None) => "",
    }
}

/// 从载荷字节解码 InputStateReportPayload（CMD_INPUT_STATE_REPORT 0x0213）
pub fn decode_input_state_report(payload: &[u8]) -> Result<InputStateReportPayload, CodecError> {
    InputStateReportPayload::from_bytes(payload).ok_or(CodecError::BufferTooShort {
        need: InputStateReportPayload::SIZE,
        got: payload.len(),
    })
}

/// 解码 `CMD_UIELEMENT_STATE`（0x0217）载荷。
pub fn decode_uielement_state(payload: &[u8]) -> Result<UiElementStatePayload, CodecError> {
    UiElementStatePayload::from_bytes(payload).ok_or(CodecError::BufferTooShort {
        need: UiElementStatePayload::SIZE,
        got: payload.len(),
    })
}

/// 解码 `CMD_UIELEMENT_ACTION`（0x021A）载荷。
pub fn decode_uielement_action(payload: &[u8]) -> Result<UiElementActionPayload, CodecError> {
    UiElementActionPayload::from_bytes(payload).ok_or(CodecError::BufferTooShort {
        need: UiElementActionPayload::SIZE,
        got: payload.len(),
    })
}

/// 编码 `CMD_UIELEMENT_PAGE`（0x0219）响应：候选快照给 DLL 的 `ITfCandidateListUIElement`。
///
/// 线上格式（全部 LE）：
/// `selected u32 + pageSize u32 + currentPage u32 + count u32 + count × { len u16 + UTF-8 }`。
/// 单条超过 u16 的候选文本按字节截断到字符边界——那已经不是候选而是事故，宁可截也不要
/// 让 DLL 解析错位。
pub fn encode_uielement_page(page: &UiElementPage) -> Vec<u8> {
    let items: Vec<&str> = page
        .items
        .iter()
        .take(UIELEMENT_MAX_ITEMS)
        .map(|s| {
            let mut end = s.len().min(u16::MAX as usize);
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            &s[..end]
        })
        .collect();
    let body_len = 16 + items.iter().map(|s| 2 + s.len()).sum::<usize>();
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + body_len);
    buf.extend_from_slice(&IpcHeader::new(CMD_UIELEMENT_PAGE, body_len as u32).to_bytes());
    buf.extend_from_slice(&page.selected.to_le_bytes());
    buf.extend_from_slice(&page.page_size.max(1).to_le_bytes());
    buf.extend_from_slice(&page.current_page.to_le_bytes());
    buf.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for s in items {
        buf.extend_from_slice(&(s.len() as u16).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }
    buf
}

/// 解码 `CMD_UIELEMENT_PAGE` 载荷（不含 8 字节 IPC 头）。与 [`encode_uielement_page`] 互逆；
/// 服务端不消费它，放在这里是为了让往返测试与 DLL 侧解析器对着同一份定义。
pub fn decode_uielement_page(payload: &[u8]) -> Result<UiElementPage, CodecError> {
    let short = |need: usize| CodecError::BufferTooShort {
        need,
        got: payload.len(),
    };
    if payload.len() < 16 {
        return Err(short(16));
    }
    let u32_at = |i: usize| u32::from_le_bytes(payload[i..i + 4].try_into().unwrap());
    let (selected, page_size, current_page, count) = (u32_at(0), u32_at(4), u32_at(8), u32_at(12));
    let mut items = Vec::with_capacity(count.min(UIELEMENT_MAX_ITEMS as u32) as usize);
    let mut off = 16;
    for _ in 0..count {
        if payload.len() < off + 2 {
            return Err(short(off + 2));
        }
        let len = u16::from_le_bytes([payload[off], payload[off + 1]]) as usize;
        off += 2;
        if payload.len() < off + len {
            return Err(short(off + len));
        }
        items.push(String::from_utf8_lossy(&payload[off..off + len]).into_owned());
        off += len;
    }
    Ok(UiElementPage {
        items,
        selected,
        page_size,
        current_page,
    })
}

/// 从载荷字节解码 DiagSnapshotPayload（CMD_DIAG_SNAPSHOT 0x0214）。
/// 变长类名区残缺不算失败（见 `DiagSnapshotPayload::from_bytes`），只有定长头不足才报错。
pub fn decode_diag_snapshot(payload: &[u8]) -> Result<DiagSnapshotPayload, CodecError> {
    DiagSnapshotPayload::from_bytes(payload).ok_or(CodecError::BufferTooShort {
        need: DiagSnapshotPayload::HEAD_SIZE,
        got: payload.len(),
    })
}

/// 编码 CommitText 响应 (CMD_COMMIT_TEXT 0x0101)
///
/// 格式: CommitTextHeader(12) + UTF-8 text + optional newComposition
///
/// flags: bit0=modeChanged(0x01), bit1=hasNewComposition(0x02), bit2=chineseMode(0x04),
///        bit3=replacingHeld(0x08)
pub fn encode_commit_text(
    text: &str,
    new_composition: Option<&str>,
    mode_changed: bool,
    chinese_mode: bool,
    has_new_composition: bool,
) -> Vec<u8> {
    encode_commit_text_inner(
        text,
        new_composition,
        mode_changed,
        chinese_mode,
        has_new_composition,
        false,
        false,
    )
}

/// 编码带「无占位联想会话」标志的 CommitText 响应（flags bit5，见 `COMMIT_FLAG_ASSOC_SESSION`）。
pub fn encode_commit_text_assoc_session(
    text: &str,
    mode_changed: bool,
    chinese_mode: bool,
) -> Vec<u8> {
    encode_commit_text_inner(text, None, mode_changed, chinese_mode, false, false, true)
}

/// 编码带 replacingHeld 标志的 CommitText 响应 (CMD_COMMIT_TEXT 0x0101, flags bit3)。
///
/// 语义：本次提交要**替换**掉 C++ 端 HoldComposition 里那个待定的中文符号，而不是追加在
/// 它后面。只有智能符号 press2（超时窗口内重按同一符号，中文→英文）需要它。
///
/// 其余一切上屏路径都是追加语义——C++ 端 `CommitText` 默认走 `AbsorbHeldIntoPrefix`，把
/// held 符号并进 prefix 一起提交。这个默认值是刻意选的：hold 期间可能触发提交的路径远不止
/// 一处（全角空格/数字、临时英文、各独占模式出字……），把安全的那一侧设为默认，新增路径
/// 自动正确；只有这一个真正要覆盖的点显式声明。
pub fn encode_commit_text_replacing_held(text: &str, chinese_mode: bool) -> Vec<u8> {
    encode_commit_text_inner(text, None, false, chinese_mode, false, true, false)
}

#[cfg(test)]
mod assoc_session_flag_tests {
    use super::*;

    /// 无占位联想会话的标志位只在该编码器里置；普通上屏不带。
    #[test]
    fn assoc_session_flag_only_on_its_encoder() {
        let flags = |b: &[u8]| u32::from_le_bytes(b[IpcHeader::SIZE..][..4].try_into().unwrap());
        let off = encode_commit_text("好", None, false, true, false);
        let on = encode_commit_text_assoc_session("好", false, true);
        assert_eq!(flags(&off) & 0x20, 0);
        assert_eq!(flags(&on) & 0x20, 0x20);
        assert_eq!(0x20, crate::protocol::COMMIT_FLAG_ASSOC_SESSION as u32);
        assert_eq!(flags(&on) & 0x02, 0, "不重启组合");
    }
}

fn encode_commit_text_inner(
    text: &str,
    new_composition: Option<&str>,
    mode_changed: bool,
    chinese_mode: bool,
    has_new_composition: bool,
    replacing_held: bool,
    assoc_session: bool,
) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let comp_bytes = new_composition.map(|s| s.as_bytes());
    let comp_len = comp_bytes.map_or(0, |b| b.len());

    let mut flags: u32 = 0;
    if mode_changed {
        flags |= 0x01;
    }
    if comp_bytes.is_some() || has_new_composition {
        flags |= 0x02;
    }
    if chinese_mode {
        flags |= 0x04;
    }
    if replacing_held {
        flags |= 0x08;
    }
    if assoc_session {
        flags |= 0x20;
    }

    let total = 12 + text_bytes.len() + comp_len;
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + total);

    // IpcHeader
    let ipc = IpcHeader::new(CMD_COMMIT_TEXT, total as u32);
    buf.extend_from_slice(&ipc.to_bytes());

    // CommitTextHeader
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&(text_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(comp_len as u32).to_le_bytes());

    // Text
    buf.extend_from_slice(text_bytes);

    // Optional new composition
    if let Some(comp) = comp_bytes {
        buf.extend_from_slice(comp);
    }

    buf
}

/// 编码 CommitResult 响应 (CMD_COMMIT_RESULT 0x0105)
///
/// 格式: CommitResultHeader(12) + UTF-8 text + optional UTF-8 newComposition
///
/// 用于 barrier 机制的提交响应（Space/Enter/数字选词）。
pub fn encode_commit_result(
    barrier_seq: u16,
    text: &str,
    new_composition: Option<&str>,
    mode_changed: bool,
    chinese_mode: bool,
) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let comp_bytes = new_composition.map(|s| s.as_bytes());
    let comp_len = comp_bytes.map_or(0, |b| b.len());

    let mut flags: u16 = 0;
    if mode_changed {
        flags |= COMMIT_FLAG_MODE_CHANGED;
    }
    if comp_bytes.is_some() {
        flags |= COMMIT_FLAG_HAS_NEW_COMPOSITION;
    }
    if chinese_mode {
        flags |= COMMIT_FLAG_CHINESE_MODE;
    }

    let total = 12 + text_bytes.len() + comp_len;
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + total);

    // IpcHeader
    let ipc = IpcHeader::new(CMD_COMMIT_RESULT, total as u32);
    buf.extend_from_slice(&ipc.to_bytes());

    // CommitResultHeader: barrierSeq(u16) + flags(u16) + textLength(u32) + compositionLength(u32)
    buf.extend_from_slice(&barrier_seq.to_le_bytes());
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&(text_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(comp_len as u32).to_le_bytes());

    // Text
    buf.extend_from_slice(text_bytes);

    // Optional new composition
    if let Some(comp) = comp_bytes {
        buf.extend_from_slice(comp);
    }

    buf
}

/// 从载荷字节解码 CommitRequestPayload
///
/// 格式: barrierSeq(u16) + triggerKey(u16) + modifiers(u32) + inputBufferLen(u32) + inputBuffer(UTF-8)
pub fn decode_commit_request(payload: &[u8]) -> Result<CommitRequestPayload, CodecError> {
    if payload.len() < 8 {
        return Err(CodecError::BufferTooShort {
            need: 8,
            got: payload.len(),
        });
    }
    let barrier_seq = u16::from_le_bytes([payload[0], payload[1]]);
    let trigger_key = u16::from_le_bytes([payload[2], payload[3]]);
    let modifiers = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);

    let input_buffer = if payload.len() > 8 {
        // 剩余字节为 inputBuffer（可能有长度前缀，也可能直接是 UTF-8）
        // Go 版 DecodeCommitRequestPayload 读取：barrierSeq(2) + triggerKey(2) + modifiers(4) + inputBufferLen(4) + inputBuffer
        if payload.len() >= 12 {
            let buf_len =
                u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]) as usize;
            if payload.len() >= 12 + buf_len {
                String::from_utf8(payload[12..12 + buf_len].to_vec()).unwrap_or_default()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    Ok(CommitRequestPayload {
        barrier_seq,
        trigger_key,
        modifiers,
        input_buffer,
    })
}

/// 编码 UpdateComposition 响应
pub fn encode_update_composition(text: &str, caret_pos: u32) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let payload_len = 4 + text_bytes.len(); // caretPos(u32) + text

    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_UPDATE_COMPOSITION, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&caret_pos.to_le_bytes());
    buf.extend_from_slice(text_bytes);

    buf
}

/// 编码 ACK 响应
pub fn encode_ack() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_ACK, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 ModePush 响应（FocusGained 同步路径）：4 字节 LE flags，携带中英 / 全半角 / 中英标点。
/// DLL 收到后在首键前写好 `_bChineseMode` / `_bFullWidth` / `_bChinesePunct`。
///
/// ⚠️ **标点位不可省**：DLL 的标点透传判据要按当下标点态在两份集合间二选一，而英文态那份
/// 是超集。漏了这一位，`_bChinesePunct` 会在每次焦点切换后被清成 false，于是中文标点态下
/// 误用英文态集合 —— `,` `.` `'` 这些本该转中文标点的键被直接透传成半角（B-9 的回归形态）。
/// 旧版 DLL 忽略不认识的 bit，跨版本安全。
pub fn encode_mode_push(chinese_mode: bool, full_width: bool, chinese_punct: bool) -> Vec<u8> {
    let mut flags: u32 = 0;
    if chinese_mode {
        flags |= STATUS_CHINESE_MODE;
    }
    if full_width {
        flags |= STATUS_FULL_WIDTH;
    }
    if chinese_punct {
        flags |= STATUS_CHINESE_PUNCT;
    }
    let ipc = IpcHeader::new(CMD_MODE_PUSH, 4);
    let mut out = ipc.to_bytes().to_vec();
    out.extend_from_slice(&flags.to_le_bytes());
    out
}

/// 编码 ShellExec 推送（CMD_SHELL_EXEC 0x020E）：让 TSF DLL 在前台应用进程中执行 ShellExecuteW。
///
/// 格式: 依次为 5 个 `len(u32 LE) + bytes(UTF-8)` 段：
/// target / params / dir / verb / show
///
/// - open(url/file): target = url, params = ""
/// - proc.run(cmd, args): target = cmd, params = args joined with space
/// - dir: 子进程工作目录，空串 = 不指定（TSF 侧即沿用宿主应用当前目录）
/// - verb: ShellExecute 动词（`open` / `runas` / …），空串 = `open`
/// - show: 初始窗口状态（`normal` / `min` / `max` / `hidden`），空串 = `normal`
///
/// `verb` / `show` 传**语义名而非 SW_ 数值**：协议自描述（日志里 `show=min` 比
/// `show=2` 好读），且 Windows API 常量留在 C++ 侧，跨平台的本 crate 不碰。
///
/// dir/verb/show 都是后加的段，两个方向都容错：旧 DLL 只读它认识的前几段、忽略
/// 尾部（表现为新选项不生效，不会崩）；新 DLL 每读一段前先查剩余长度，遇旧服务
/// 按空串处理。⚠️ 因此「装了新服务但宿主进程还挂着旧 DLL」时这些字段静默失效，
/// 排查时服务端日志是唯一能证明"发过什么"的一侧。
///
/// ShellExecuteW 的入参到此已全部覆盖（hwnd 恒 null），这条协议不会再长。
pub fn encode_shell_exec(target: &str, params: &str, dir: &str, verb: &str, show: &str) -> Vec<u8> {
    let segs = [target, params, dir, verb, show];
    let payload_len: usize = segs.iter().map(|s| 4 + s.len()).sum();
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);
    let ipc = IpcHeader::new(CMD_SHELL_EXEC, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    for s in segs {
        buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }
    buf
}

/// 编码「只重取语言栏图标」推送（无载荷，见 [`CMD_REFRESH_ICON`]）。
pub fn encode_refresh_icon() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_REFRESH_ICON, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 PassThrough 响应
pub fn encode_pass_through() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_PASS_THROUGH, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 Consumed 响应
pub fn encode_consumed() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_CONSUMED, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 ClearComposition 响应
pub fn encode_clear_composition() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_CLEAR_COMPOSITION, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 ClearCompositionThenPassThrough 响应：收组合 + 把当前键交还宿主。
pub fn encode_clear_then_pass_through() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_CLEAR_THEN_PASS_THROUGH, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 StatusUpdate 响应 (CMD_STATUS_UPDATE 0x0202)
///
/// 格式: StatusHeader(12) + keyHashes(u32*N) + iconLabel(UTF-8)
///
/// 用于 bridge pipe 上的同步状态响应（如 ToggleMode、MenuCommand 等）。
/// 与 EncodeActivationStatusPush 载荷格式一致，但 command 不同。
// 状态位是**线协议的扁平字段**，逐个传即逐个写入报文；聚合成结构体只会在编码前多一层
// 搬运，还要让 C++ 侧的字段顺序去对齐一个 Rust 结构体。故这几个 encode 函数一律豁免。
#[allow(clippy::too_many_arguments)]
pub fn encode_status_update(
    chinese_mode: bool,
    full_width: bool,
    chinese_punct: bool,
    toolbar_visible: bool,
    caps_lock: bool,
    host_render_avail: bool,
    soft_keyboard: bool,
    soft_keyboard_keys: bool,
    hotkey_session: bool,
    key_down_hashes: &[u32],
    key_up_hashes: &[u32],
    icon_label: &str,
) -> Vec<u8> {
    encode_status_update_ex(
        CMD_STATUS_UPDATE,
        chinese_mode,
        full_width,
        chinese_punct,
        toolbar_visible,
        caps_lock,
        host_render_avail,
        soft_keyboard,
        soft_keyboard_keys,
        hotkey_session,
        key_down_hashes,
        key_up_hashes,
        icon_label,
    )
}

/// 编码 ActivationStatusPush (CMD_ACTIVATION_STATUS_PUSH 0x020C)
///
/// 格式与 StatusUpdate 完全一致，仅 command 字段不同。
/// 用于 IMEActivated/FocusGained 异步化后通过 push pipe 推送状态回包。
/// C++ 端 AsyncReader 收到后 Post 到 TSF 线程做 _SyncStateFromResponse + _EnsureHostRenderSetup。
///
/// 与 StatePush 的区别：本命令是 activation 握手回包，必须携带完整 hotkeys + hostRenderAvail。
#[allow(clippy::too_many_arguments)]
pub fn encode_activation_status_push(
    chinese_mode: bool,
    full_width: bool,
    chinese_punct: bool,
    toolbar_visible: bool,
    caps_lock: bool,
    host_render_avail: bool,
    soft_keyboard: bool,
    soft_keyboard_keys: bool,
    hotkey_session: bool,
    key_down_hashes: &[u32],
    key_up_hashes: &[u32],
    icon_label: &str,
) -> Vec<u8> {
    encode_status_update_ex(
        CMD_ACTIVATION_STATUS_PUSH,
        chinese_mode,
        full_width,
        chinese_punct,
        toolbar_visible,
        caps_lock,
        host_render_avail,
        soft_keyboard,
        soft_keyboard_keys,
        hotkey_session,
        key_down_hashes,
        key_up_hashes,
        icon_label,
    )
}

/// 编码 StatePush (CMD_STATE_PUSH 0x0206)
///
/// 格式与 StatusUpdate 一致但使用 CmdStatePush 命令码，且不含 hotkeys。
/// 用于焦点不变时的状态变化广播（如点击工具栏切换中英模式）。
#[allow(clippy::too_many_arguments)]
pub fn encode_state_push(
    chinese_mode: bool,
    full_width: bool,
    chinese_punct: bool,
    toolbar_visible: bool,
    caps_lock: bool,
    soft_keyboard: bool,
    soft_keyboard_keys: bool,
    hotkey_session: bool,
    icon_label: &str,
) -> Vec<u8> {
    encode_status_update_ex(
        CMD_STATE_PUSH,
        chinese_mode,
        full_width,
        chinese_punct,
        toolbar_visible,
        caps_lock,
        false, // host_render_avail
        soft_keyboard,
        soft_keyboard_keys,
        hotkey_session,
        &[], // no hotkeys
        &[],
        icon_label,
    )
}

/// 状态编码公共逻辑（StatusUpdate / StatePush / ActivationStatusPush 共用）
///
/// ⚠ **本消息不能再追加字段**：`icon_label` 是尾部不定长段，没有长度前缀，C++ 侧读的是
/// 「structuredSize 到 payload 末尾」的全部字节（`IPCClient.cpp` 的 iconLabel 解析）。
/// 在它后面加任何东西都会被当成标签内容——**不会报错，只会让标签变成一串垃圾**。
///
/// 真要加变长字段时，唯一正确的做法是先给 `icon_label` 补一个 `labelLen:u32` 前缀、
/// 两侧同步改，再往后追加；这是破坏性变更，DLL 与服务端必须同版本发布（开发期混用
/// 会解出垃圾，靠新旧 DLL 指纹辨认）。**不要为了绕开这一步而把状态塞进别的通道**——
/// 除非那个字段本来就该走别的通道（如 `CONFIG_KEY_LANGBAR_TOOLTIP`：它需要广播，
/// 而本消息是定向的，那是设计选择而非妥协）。
///
/// 加**布尔**字段则无此限制：`flags` 还有空位，加位不影响布局。
#[allow(clippy::too_many_arguments)]
fn encode_status_update_ex(
    command: u16,
    chinese_mode: bool,
    full_width: bool,
    chinese_punct: bool,
    toolbar_visible: bool,
    caps_lock: bool,
    host_render_avail: bool,
    soft_keyboard: bool,
    soft_keyboard_keys: bool,
    hotkey_session: bool,
    key_down_hashes: &[u32],
    key_up_hashes: &[u32],
    icon_label: &str,
) -> Vec<u8> {
    let mut flags: u32 = 0;
    if chinese_mode {
        flags |= STATUS_CHINESE_MODE;
    }
    if full_width {
        flags |= STATUS_FULL_WIDTH;
    }
    if chinese_punct {
        flags |= STATUS_CHINESE_PUNCT;
    }
    if toolbar_visible {
        flags |= STATUS_TOOLBAR_VISIBLE;
    }
    if caps_lock {
        flags |= STATUS_CAPS_LOCK;
    }
    if host_render_avail {
        flags |= STATUS_HOST_RENDER_AVAIL;
    }
    if soft_keyboard {
        flags |= STATUS_SOFT_KEYBOARD;
    }
    if soft_keyboard_keys {
        flags |= STATUS_SOFT_KEYBOARD_KEYS;
    }
    if hotkey_session {
        flags |= STATUS_HOTKEY_SESSION;
    }

    let key_down_count = key_down_hashes.len() as u32;
    let key_up_count = key_up_hashes.len() as u32;
    let label_bytes = icon_label.as_bytes();

    let hash_total = (key_down_count + key_up_count) as usize * 4;
    let payload_len = 12 + hash_total + label_bytes.len();

    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    // IpcHeader
    let ipc = IpcHeader::new(command, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());

    // StatusHeader
    buf.extend_from_slice(&flags.to_le_bytes());
    buf.extend_from_slice(&key_down_count.to_le_bytes());
    buf.extend_from_slice(&key_up_count.to_le_bytes());

    // Key hashes
    for h in key_down_hashes {
        buf.extend_from_slice(&h.to_le_bytes());
    }
    for h in key_up_hashes {
        buf.extend_from_slice(&h.to_le_bytes());
    }

    // Icon label
    buf.extend_from_slice(label_bytes);

    buf
}

/// 编码批处理响应
pub fn encode_batch_response(sub_messages: &[Vec<u8>]) -> Vec<u8> {
    // BatchHeader: eventCount(u16) + reserved(u16)
    let sub_total: usize = sub_messages.iter().map(|m| m.len()).sum();
    let payload_len = 4 + sub_total;

    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_BATCH_RESPONSE, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&(sub_messages.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // reserved

    for msg in sub_messages {
        buf.extend_from_slice(msg);
    }

    buf
}

/// 编码 CommitTextWithCursor 响应 (CMD_COMMIT_TEXT_WITH_CURSOR 0x0106)
///
/// 格式: textLength(4) + cursorOffset(4) + UTF-8 text
pub fn encode_commit_text_with_cursor(text: &str, cursor_offset: u32) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let payload_len = 8 + text_bytes.len();

    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_COMMIT_TEXT_WITH_CURSOR, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&(text_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&cursor_offset.to_le_bytes());
    buf.extend_from_slice(text_bytes);

    buf
}

/// 编码 MoveCursor 响应 (CMD_MOVE_CURSOR 0x0107)
///
/// 格式: count(4) — 向右移动的格数（合成几次 VK_RIGHT）
///
/// **语义变更**：该字段原名 `direction`（恒为 1，C++ 侧根本没读），现改为格数。
/// 直通 `ime.pair` 的多字符右段要越过不止一格，才需要它真的携带信息。
/// `0` 视同 1——协调器不发 0，但旧版 DLL 与新版 core 混搭时不该退化成「跳出没反应」。
pub fn encode_move_cursor(count: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + 4);

    let ipc = IpcHeader::new(CMD_MOVE_CURSOR, 4);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&count.to_le_bytes());

    buf
}

/// 编码 DeletePair 响应 (CMD_DELETE_PAIR 0x0108)
///
/// 无载荷：删除 1 个左侧字符 + 1 个右侧字符
pub fn encode_delete_pair() -> Vec<u8> {
    let ipc = IpcHeader::new(CMD_DELETE_PAIR, 0);
    ipc.to_bytes().to_vec()
}

/// 编码 ReplaceBackward 响应 (CMD_REPLACE_BACKWARD 0x0109)
///
/// 格式: count(4) + text_len(4) + UTF-8 text —— 删光标前 count 个字符后插入 text（智能符号替换）
pub fn encode_replace_backward(count: u32, text: &str) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let payload_len = 8 + text_bytes.len();
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_REPLACE_BACKWARD, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&(text_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(text_bytes);

    buf
}

/// 编码 CommitAndHoldComposition 响应 (CMD_COMMIT_AND_HOLD 0x010B)
///
/// 格式：timeout_ms(4) + commit_len(4) + hold_len(4) + commit_utf8 + hold_utf8
/// C++ 端先提交 commit_text（候选），再开 HoldComposition 放入 hold_text（中文标点）。
pub fn encode_commit_and_hold(timeout_ms: u32, commit_text: &str, hold_text: &str) -> Vec<u8> {
    let commit_bytes = commit_text.as_bytes();
    let hold_bytes = hold_text.as_bytes();
    let payload_len = 12 + commit_bytes.len() + hold_bytes.len();
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_COMMIT_AND_HOLD, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&timeout_ms.to_le_bytes());
    buf.extend_from_slice(&(commit_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(hold_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(commit_bytes);
    buf.extend_from_slice(hold_bytes);
    buf
}

/// 编码 CommitThenDeferComposition 响应 (CMD_COMMIT_THEN_DEFER 0x010C)
///
/// 格式：timeout_ms(4) + commit_len(4) + defer_len(4) + commit_utf8 + defer_utf8
/// C++ 端先真提交 commit_text，余码 deferred_composition 延迟到触发键 keyup 才开新组合。
pub fn encode_commit_then_defer(
    timeout_ms: u32,
    commit_text: &str,
    deferred_composition: &str,
) -> Vec<u8> {
    let commit_bytes = commit_text.as_bytes();
    let defer_bytes = deferred_composition.as_bytes();
    let payload_len = 12 + commit_bytes.len() + defer_bytes.len();
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_COMMIT_THEN_DEFER, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&timeout_ms.to_le_bytes());
    buf.extend_from_slice(&(commit_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(defer_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(commit_bytes);
    buf.extend_from_slice(defer_bytes);
    buf
}

/// 编码 HoldComposition 响应 (CMD_HOLD_COMPOSITION 0x010A)
///
/// 格式：timeout_ms(4) + text_len(4) + UTF-8 text
/// C++ 端开启组合显示 text，timeout_ms 毫秒后自动提交（智能符号 HoldComposition 方案）。
pub fn encode_hold_composition(timeout_ms: u32, text: &str) -> Vec<u8> {
    let text_bytes = text.as_bytes();
    let payload_len = 8 + text_bytes.len();
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload_len);

    let ipc = IpcHeader::new(CMD_HOLD_COMPOSITION, payload_len as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&timeout_ms.to_le_bytes());
    buf.extend_from_slice(&(text_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(text_bytes);
    buf
}

/// 编码 HostRenderSetup 响应 (CMD_HOST_RENDER_SETUP 0x0501，Windows)
///
/// 线格式（对齐 C++ IPCClient.cpp:1524 解码端 / BinaryProtocol.h HostRenderSetupEntryHeader）：
/// instanceId(u32) + entryCount(u32) + N × { kind(u32) + maxBufferSize(u32)
///   + shmNameLen(u32) + eventNameLen(u32) + shmName(UTF-8) + eventName(UTF-8) }
pub fn encode_host_render_setup(
    instance_id: u32,
    entries: &[crate::protocol::HostRenderSetupEntry],
) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&instance_id.to_le_bytes());
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        let shm = e.shm_name.as_bytes();
        let evt = e.event_name.as_bytes();
        payload.extend_from_slice(&e.window_kind.to_le_bytes());
        payload.extend_from_slice(&e.max_buffer_size.to_le_bytes());
        payload.extend_from_slice(&(shm.len() as u32).to_le_bytes());
        payload.extend_from_slice(&(evt.len() as u32).to_le_bytes());
        payload.extend_from_slice(shm);
        payload.extend_from_slice(evt);
    }
    let mut buf = Vec::with_capacity(IpcHeader::SIZE + payload.len());
    let ipc = IpcHeader::new(CMD_HOST_RENDER_SETUP, payload.len() as u32);
    buf.extend_from_slice(&ipc.to_bytes());
    buf.extend_from_slice(&payload);
    buf
}

#[cfg(test)]
mod refresh_icon_tests {
    use super::*;

    /// 命令号在两个仓里各硬编码一份（本处与 `wind_tsf/include/BinaryProtocol.h` 的
    /// `CMD_REFRESH_ICON`），没有任何编译期约束把它们绑在一起。写死期望值是为了让
    /// **单侧改动**在这里失败——两侧漂移的症状是「推了没反应」：DLL 收到一个不认识的
    /// 命令直接走完 else 链丢弃，两边日志都不会有错误。本仓已按同一形态栽过
    /// （触发键名两仓拼写不一致，静默不匹配）。
    #[test]
    fn refresh_icon_command_id_is_frozen_at_0x0216() {
        assert_eq!(
            CMD_REFRESH_ICON, 0x0216,
            "改了命令号必须同步 wind_tsf/include/BinaryProtocol.h 的 CMD_REFRESH_ICON"
        );
    }

    /// 无载荷是本命令的设计要点而非省事：图标内容的唯一真相在共享内存里，
    /// 载荷里再放一份就是第二条真相通路。这条测试把「不许带载荷」钉住。
    #[test]
    fn refresh_icon_frame_is_header_only() {
        let f = encode_refresh_icon();
        assert_eq!(f.len(), IpcHeader::SIZE, "刷新图标帧应当只有 header");
        assert_eq!(u16::from_le_bytes([f[2], f[3]]), CMD_REFRESH_ICON);
        // header 里声明的载荷长度也必须是 0——DLL 按它推进读指针，非零会让下一帧错位。
        assert_eq!(u32::from_le_bytes(f[4..8].try_into().unwrap()), 0);
    }
}

#[cfg(test)]
mod shell_exec_tests {
    use super::*;

    /// 按「长度前缀 + 内容」逐段解码，最多取 `max_segs` 段。
    /// `max_segs=2` 即模拟只认 target/params 的**旧版 DLL**。
    fn decode_segs(buf: &[u8], max_segs: usize) -> Vec<String> {
        let mut p = &buf[IpcHeader::SIZE..];
        let mut out = Vec::new();
        while out.len() < max_segs && p.len() >= 4 {
            let n = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
            if p.len() < 4 + n {
                break;
            }
            out.push(String::from_utf8(p[4..4 + n].to_vec()).unwrap());
            p = &p[4 + n..];
        }
        out
    }

    #[test]
    fn encodes_five_segments_in_order() {
        let buf = encode_shell_exec("d.exe", "a b", "D:/W", "runas", "min");
        assert_eq!(
            decode_segs(&buf, 5),
            vec!["d.exe", "a b", "D:/W", "runas", "min"]
        );
    }

    /// 未指定的选项走空串段而非省略段——段数恒定，解码方不必猜"少的是哪一个"。
    #[test]
    fn omitted_options_are_empty_segments_not_missing_ones() {
        let buf = encode_shell_exec("d.exe", "", "", "", "");
        assert_eq!(decode_segs(&buf, 5), vec!["d.exe", "", "", "", ""]);
    }

    /// **向后兼容**：只认前两段的旧 DLL 读同一份新报文，仍应拿到正确的
    /// target/params。新段追加在尾部，旧解析器读完就停，不会错位。
    /// 这是「新服务 + 旧 DLL」组合下 proc.run 不至于整体失效的依据。
    #[test]
    fn old_two_segment_reader_still_gets_target_and_params() {
        let buf = encode_shell_exec("d.exe", "a b", "D:/W", "runas", "min");
        assert_eq!(decode_segs(&buf, 2), vec!["d.exe", "a b"]);
    }

    /// 载荷长度必须与声明一致，否则 C++ 侧以 header.length 为边界会截断尾部段。
    #[test]
    fn payload_len_in_header_covers_all_segments() {
        let buf = encode_shell_exec("d.exe", "a b", "D:/W", "runas", "min");
        let declared = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
        assert_eq!(declared, buf.len() - IpcHeader::SIZE);
    }

    /// 非 ASCII 段按 UTF-8 字节计长（中文路径必经之路）。
    #[test]
    fn non_ascii_segments_use_byte_length() {
        let buf = encode_shell_exec("d.exe", "", "D:/我的 词库", "", "");
        assert_eq!(decode_segs(&buf, 3)[2], "D:/我的 词库");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从三种状态编码里取出 flags 字段（三者布局相同：header 之后第一个 u32）。
    fn flags_of(buf: &[u8]) -> u32 {
        // 三种状态编码共用 `encode_status_update_ex`，布局相同：8 字节 header 之后
        // 第一个 u32 就是 flags（小端）。偏移写错的话下面 `on ^ off` 那条断言会红 ——
        // 取到别处的字节不可能恰好只差 STATUS_HOTKEY_SESSION 这一位。
        u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]])
    }

    /// ★★★ 三条状态通路**都**必须把 `hotkey_session` 编进 flags。
    ///
    /// 这一位是 level-triggered 的：C++ 侧收到任何一条状态响应都会无条件镜像它。所以
    /// 「漏带」不等于「带 false 无害」——它会把一个正活着的会话当场清掉。可达场景：
    /// 加词开着时按 lshift 切中英，那条走 `encode_status_update` 的同步响应若不带本位，
    /// 加词的 Enter/Esc 立刻失灵。
    ///
    /// 三个入口的参数全是 bool 且相邻，漏传/错位编译器不会报错，只能靠本条钉住。
    /// 「把会话位编码进一条状态消息」的三个入口的共同形状。
    ///
    /// 抽成别名只为躲开 clippy::type_complexity——三个入口签名一致正是本用例的前提，
    /// 写成一个名字反而更说明这件事。
    type StatusEncoder = Box<dyn Fn(bool) -> Vec<u8>>;

    #[test]
    fn every_status_encoding_carries_the_hotkey_session_bit() {
        let cases: [(&str, StatusEncoder); 3] = [
            (
                "encode_status_update",
                Box::new(|on| {
                    encode_status_update(
                        true,
                        false,
                        true,
                        true,
                        false,
                        false,
                        false,
                        false,
                        on,
                        &[],
                        &[],
                        "中",
                    )
                }),
            ),
            (
                "encode_activation_status_push",
                Box::new(|on| {
                    encode_activation_status_push(
                        true,
                        false,
                        true,
                        true,
                        false,
                        false,
                        false,
                        false,
                        on,
                        &[],
                        &[],
                        "中",
                    )
                }),
            ),
            (
                "encode_state_push",
                Box::new(|on| {
                    encode_state_push(true, false, true, true, false, false, false, on, "中")
                }),
            ),
        ];

        for (name, enc) in cases {
            let on = flags_of(&enc(true));
            let off = flags_of(&enc(false));
            assert_ne!(
                on & STATUS_HOTKEY_SESSION,
                0,
                "{name}: hotkey_session=true 时必须置 STATUS_HOTKEY_SESSION（flags=0x{on:08X}）"
            );
            assert_eq!(
                off & STATUS_HOTKEY_SESSION,
                0,
                "{name}: hotkey_session=false 时不得置位（flags=0x{off:08X}）"
            );
            // 只有这一位变——防止参数错位把别的状态位一起改了。
            assert_eq!(
                on ^ off,
                STATUS_HOTKEY_SESSION,
                "{name}: 切换 hotkey_session 只应改变这一位，实际差异=0x{:08X}",
                on ^ off
            );
        }
    }

    /// 拼一份 macOS `.app` 形态的 FOCUS_GAINED 载荷（39 定长 + bundleID 段）。
    fn focus_payload_with_bundle(id: &str) -> Vec<u8> {
        let mut p = vec![0u8; 39];
        p.extend_from_slice(&(id.len() as u32).to_le_bytes());
        p.extend_from_slice(id.as_bytes());
        p
    }

    #[test]
    fn uielement_state_and_action_roundtrip() {
        let st = UiElementStatePayload {
            pid: 4242,
            flags: UIELEMENT_FLAG_HOST_DRAWS,
        };
        assert_eq!(decode_uielement_state(&st.to_bytes()).unwrap(), st);
        assert!(st.host_draws());
        // UI-less 线程位单独置位也算接管（从激活起就不弹窗，见常量注释）。
        assert!(
            UiElementStatePayload {
                pid: 1,
                flags: UIELEMENT_FLAG_UI_LESS_THREAD
            }
            .host_draws()
        );
        assert!(!UiElementStatePayload { pid: 1, flags: 0 }.host_draws());
        assert!(decode_uielement_state(&[0u8; 7]).is_err());

        let act = UiElementActionPayload {
            action: UIELEMENT_ACTION_SET_SELECTION,
            arg: 17,
        };
        assert_eq!(decode_uielement_action(&act.to_bytes()).unwrap(), act);
        assert!(decode_uielement_action(&[0u8; 3]).is_err());
    }

    #[test]
    fn uielement_page_roundtrip() {
        let page = UiElementPage {
            items: vec!["你好".into(), "拟好".into(), "".into(), "abc".into()],
            selected: 1,
            page_size: 9,
            current_page: 0,
        };
        let frame = encode_uielement_page(&page);
        assert_eq!(u16::from_le_bytes([frame[2], frame[3]]), CMD_UIELEMENT_PAGE);
        let body_len = u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize;
        assert_eq!(body_len, frame.len() - 8);
        assert_eq!(decode_uielement_page(&frame[8..]).unwrap(), page);
        // 空列表也要能往返（宿主 Abort 后 DLL 可能在 EndUIElement 前再拉一次）。
        let empty = UiElementPage {
            page_size: 5,
            ..Default::default()
        };
        assert_eq!(
            decode_uielement_page(&encode_uielement_page(&empty)[8..]).unwrap(),
            empty
        );
        // page_size=0 会让 DLL 侧除零：编码器钉到 ≥1。
        let zero = UiElementPage {
            items: vec!["a".into()],
            page_size: 0,
            ..Default::default()
        };
        assert_eq!(
            decode_uielement_page(&encode_uielement_page(&zero)[8..])
                .unwrap()
                .page_size,
            1
        );
        // 条数超上限按 UIELEMENT_MAX_ITEMS 截断，count 与实际条数一致。
        let long = UiElementPage {
            items: (0..UIELEMENT_MAX_ITEMS + 50)
                .map(|i| i.to_string())
                .collect(),
            page_size: 9,
            ..Default::default()
        };
        let decoded = decode_uielement_page(&encode_uielement_page(&long)[8..]).unwrap();
        assert_eq!(decoded.items.len(), UIELEMENT_MAX_ITEMS);
        // 截断的载荷必须报错而不是静默给半截列表。
        let mut truncated = encode_uielement_page(&page)[8..].to_vec();
        truncated.truncate(truncated.len() - 2);
        assert!(decode_uielement_page(&truncated).is_err());
    }

    #[test]
    fn ext_envelope_roundtrip() {
        let f = encode_ext("settings.open", br#"{"args":["--page=dict"]}"#);
        assert_eq!(u16::from_le_bytes([f[2], f[3]]), CMD_EXT);
        let (kind, body) = decode_ext(&f[8..]).expect("解不出信封");
        assert_eq!(kind, "settings.open");
        assert_eq!(body, br#"{"args":["--page=dict"]}"#);
        // 空 body（纯信号型 kind）也须能往返。
        let f2 = encode_ext("diag.hud", b"");
        assert_eq!(decode_ext(&f2[8..]), Some(("diag.hud", &b""[..])));
    }

    #[test]
    fn ext_envelope_rejects_truncated_or_invalid() {
        // 截断：kind 长度声明超出实际字节。
        let mut bad = 99u32.to_le_bytes().to_vec();
        bad.extend_from_slice(b"abc");
        assert_eq!(decode_ext(&bad), None);
        // 截断：body 长度声明超出实际字节。
        let mut bad2 = 3u32.to_le_bytes().to_vec();
        bad2.extend_from_slice(b"abc");
        bad2.extend_from_slice(&99u32.to_le_bytes());
        assert_eq!(decode_ext(&bad2), None);
        // 非法 UTF-8 的 kind。
        let mut bad3 = 2u32.to_le_bytes().to_vec();
        bad3.extend_from_slice(&[0xFF, 0xFE]);
        bad3.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode_ext(&bad3), None);
        // 空载荷。
        assert_eq!(decode_ext(&[]), None);
    }

    #[test]
    fn focus_gained_bundle_id_roundtrip() {
        let p = focus_payload_with_bundle("com.apple.TextEdit");
        assert_eq!(decode_focus_gained_bundle_id(&p), "com.apple.TextEdit");
        // 定长段本身仍须能解（bundleID 是纯追加，不影响既有字段）。
        assert!(decode_focus_gained(&p).is_ok());
    }

    #[test]
    fn focus_gained_bundle_id_absent_or_malformed_is_empty() {
        // Windows DLL 的 39 字节包：无 bundleID 段。
        assert_eq!(decode_focus_gained_bundle_id(&[0u8; 39]), "");
        // 旧 macOS `.app` 的 12 字节短包。
        assert_eq!(decode_focus_gained_bundle_id(&[0u8; 12]), "");
        // 长度字段越界（截断的帧）：不得 panic，按「取不到」处理。
        let mut p = vec![0u8; 39];
        p.extend_from_slice(&999u32.to_le_bytes());
        p.extend_from_slice(b"abc");
        assert_eq!(decode_focus_gained_bundle_id(&p), "");
        // 非法 UTF-8 同样按「取不到」处理。
        let mut q = vec![0u8; 39];
        q.extend_from_slice(&2u32.to_le_bytes());
        q.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(decode_focus_gained_bundle_id(&q), "");
    }

    /// 两个变长段的组合：`[39][bundleIdLen][bundleId][classLen][class]`。
    fn focus_payload_with_sections(bundle: &str, class: &str) -> Vec<u8> {
        let mut p = vec![0u8; 39];
        p.extend_from_slice(&(bundle.len() as u32).to_le_bytes());
        p.extend_from_slice(bundle.as_bytes());
        p.extend_from_slice(&(class.len() as u32).to_le_bytes());
        p.extend_from_slice(class.as_bytes());
        p
    }

    #[test]
    fn focus_gained_window_class_roundtrip_on_both_platform_shapes() {
        // Windows 形态：bundleId 空占位 + 类名。
        let win = focus_payload_with_sections("", "Shell_TrayWnd");
        assert_eq!(decode_focus_gained_window_class(&win), "Shell_TrayWnd");
        assert_eq!(decode_focus_gained_bundle_id(&win), "");

        // macOS 形态：两段都非空。★ 这条钉住「类名段必须顺序走过 bundleId」——
        // 若哪天有人改回固定偏移，非空 bundleId 会让它读到垃圾，而定长字段全对、
        // 编译与其它测试都不会有任何信号。
        let mac = focus_payload_with_sections("com.apple.TextEdit", "NSWindow");
        assert_eq!(decode_focus_gained_bundle_id(&mac), "com.apple.TextEdit");
        assert_eq!(decode_focus_gained_window_class(&mac), "NSWindow");

        // 定长段不受影响
        assert!(decode_focus_gained(&win).is_ok());
        assert!(decode_focus_gained(&mac).is_ok());
    }

    #[test]
    fn focus_gained_window_class_absent_or_malformed_is_empty() {
        // 旧 DLL 的 39 字节包：两段都没有。
        assert_eq!(decode_focus_gained_window_class(&[0u8; 39]), "");
        // 只有 bundleId 段（本次改动之前的 macOS 包）。
        assert_eq!(
            decode_focus_gained_window_class(&focus_payload_with_bundle("com.apple.TextEdit")),
            ""
        );
        // bundleId 段自身越界 ⇒ 后面不可能有东西，不得 panic。
        let mut p = vec![0u8; 39];
        p.extend_from_slice(&999u32.to_le_bytes());
        p.extend_from_slice(b"abc");
        assert_eq!(decode_focus_gained_window_class(&p), "");
        // 类名段长度越界。
        let mut q = focus_payload_with_sections("", "");
        q.truncate(q.len() - 4);
        q.extend_from_slice(&999u32.to_le_bytes());
        assert_eq!(decode_focus_gained_window_class(&q), "");
    }

    /// 三个变长段：`[39][bundleIdLen][bundleId][classLen][class][titleLen][title]`。
    fn focus_payload_with_title(bundle: &str, class: &str, title: &str) -> Vec<u8> {
        let mut p = focus_payload_with_sections(bundle, class);
        p.extend_from_slice(&(title.len() as u32).to_le_bytes());
        p.extend_from_slice(title.as_bytes());
        p
    }

    #[test]
    fn focus_gained_window_title_roundtrip_on_both_platform_shapes() {
        // Windows 形态（新 DLL、服务端推开了采集）：bundleId 空占位 + 类名 + 标题。
        let win = focus_payload_with_title("", "Chrome_WidgetWin_1", "新标签页 - Google Chrome");
        assert_eq!(
            decode_focus_gained_window_title(&win),
            "新标签页 - Google Chrome"
        );
        assert_eq!(decode_focus_gained_window_class(&win), "Chrome_WidgetWin_1");
        assert_eq!(decode_focus_gained_bundle_id(&win), "");
        // ★ 非空 bundleId + 类名：标题段必须顺序走过前两段，固定偏移会读到垃圾。
        let mac = focus_payload_with_title("com.apple.TextEdit", "NSWindow", "Untitled");
        assert_eq!(decode_focus_gained_window_title(&mac), "Untitled");
        assert_eq!(decode_focus_gained_window_class(&mac), "NSWindow");
        // 新 DLL、未推开采集：标题段在、长度 0。
        let off = focus_payload_with_title("", "Notepad", "");
        assert_eq!(decode_focus_gained_window_title(&off), "");
        assert_eq!(decode_focus_gained_window_class(&off), "Notepad");
        // 定长段不受影响
        let fg = decode_focus_gained(&win).unwrap();
        assert_eq!(fg.client_token, 0);
    }

    /// 新旧互通：旧 DLL 的帧（没有标题段）解出空串，类名照旧；截断 / 越界 / 非法 UTF-8 不 panic。
    #[test]
    fn focus_gained_window_title_absent_or_malformed_is_empty() {
        // 旧 DLL：39 字节、或只带 bundleId + 类名两段（P3 之前的 Windows DLL）。
        assert_eq!(decode_focus_gained_window_title(&[0u8; 39]), "");
        let old = focus_payload_with_sections("", "Shell_TrayWnd");
        assert_eq!(decode_focus_gained_window_title(&old), "");
        assert_eq!(decode_focus_gained_window_class(&old), "Shell_TrayWnd");
        // 只有 bundleId 段（旧 macOS 包）。
        assert_eq!(
            decode_focus_gained_window_title(&focus_payload_with_bundle("com.apple.TextEdit")),
            ""
        );
        // 类名段越界 ⇒ 后面不可能有东西。
        let mut p = focus_payload_with_sections("", "");
        p.truncate(p.len() - 4);
        p.extend_from_slice(&999u32.to_le_bytes());
        assert_eq!(decode_focus_gained_window_title(&p), "");
        // 标题段长度越界。
        let mut q = focus_payload_with_title("", "C", "");
        q.truncate(q.len() - 4);
        q.extend_from_slice(&999u32.to_le_bytes());
        q.extend_from_slice(b"abc");
        assert_eq!(decode_focus_gained_window_title(&q), "");
        assert_eq!(
            decode_focus_gained_window_class(&q),
            "C",
            "标题段坏了不连累类名"
        );
        // 标题非法 UTF-8。
        let mut r = focus_payload_with_sections("", "C");
        r.extend_from_slice(&2u32.to_le_bytes());
        r.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(decode_focus_gained_window_title(&r), "");
    }

    /// 长度域取极值（`u32::MAX`）：64 位上越界、32 位上 `start + n` 会回绕——一律按「取不到」。
    #[test]
    fn focus_gained_sections_with_huge_lengths_are_empty_not_wrapped() {
        let mut p = focus_payload_with_sections("", "C");
        p.extend_from_slice(&u32::MAX.to_le_bytes());
        p.extend_from_slice(b"abc");
        assert_eq!(decode_focus_gained_window_title(&p), "");
        let mut q = vec![0u8; 39];
        q.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode_focus_gained_window_class(&q), "");
        assert_eq!(
            read_len_prefixed(&q, usize::MAX - 2),
            ("", None),
            "偏移本身溢出"
        );
    }

    #[test]
    fn compat_title_match_value_is_one_byte_flag() {
        assert_eq!(encode_compat_title_match_value(true), vec![1]);
        assert_eq!(encode_compat_title_match_value(false), vec![0]);
    }

    #[test]
    fn test_encode_host_render_setup_layout_matches_cpp() {
        use crate::protocol::HostRenderSetupEntry;
        let entries = vec![HostRenderSetupEntry {
            window_kind: 0,
            max_buffer_size: 4 * 1024 * 1024,
            shm_name: "Local\\W_SHM".to_string(),   // 11 B
            event_name: "Local\\W_EVT".to_string(), // 11 B
        }];
        let buf = encode_host_render_setup(7, &entries);
        // IpcHeader 8B: version u16 + command u16 + payload_len u32（以现有 IpcHeader::to_bytes 为准）
        let p = &buf[8..];
        // instanceId(4) + entryCount(4)
        assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 1);
        // entry header 16B: kind + maxBufferSize + shmNameLen + eventNameLen
        assert_eq!(u32::from_le_bytes(p[8..12].try_into().unwrap()), 0);
        assert_eq!(
            u32::from_le_bytes(p[12..16].try_into().unwrap()),
            4 * 1024 * 1024
        );
        assert_eq!(u32::from_le_bytes(p[16..20].try_into().unwrap()), 11);
        assert_eq!(u32::from_le_bytes(p[20..24].try_into().unwrap()), 11);
        assert_eq!(&p[24..35], b"Local\\W_SHM");
        assert_eq!(&p[35..46], b"Local\\W_EVT");
        assert_eq!(p.len(), 46);
        // payload_len 一致
        assert_eq!(
            u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize,
            46
        );
    }

    #[test]
    fn test_host_render_hit_rect_layout() {
        use crate::protocol::HostRenderHitRect;
        let r = HostRenderHitRect {
            index: -1,
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };
        let b = r.to_bytes();
        assert_eq!(b.len(), 20);
        assert_eq!(i32::from_le_bytes(b[0..4].try_into().unwrap()), -1);
        assert_eq!(i32::from_le_bytes(b[16..20].try_into().unwrap()), 4);
    }

    #[test]
    fn test_encode_hold_composition_layout() {
        let buf = encode_hold_composition(500, "，");
        // IpcHeader: 8 bytes (cmd u16 LE + version u16 LE + payload_len u32 LE)
        // payload: timeout_ms(4) + text_len(4) + "，"(3 UTF-8 bytes) = 11 bytes
        assert_eq!(buf.len(), 8 + 11);
        // cmd = 0x010A LE (at offset 2-4)
        assert_eq!(buf[2], 0x0A);
        assert_eq!(buf[3], 0x01);
        // payload_len = 11 LE (at offset 4-8)
        assert_eq!(u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]), 11);
        // timeout_ms = 500 LE (at offset 8-12)
        assert_eq!(u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]), 500);
        // text_len = 3 LE (at offset 12-16)
        assert_eq!(u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]), 3);
        // UTF-8 bytes of "，" = [0xEF, 0xBC, 0x8C] (at offset 16+)
        assert_eq!(&buf[16..], "，".as_bytes());
    }

    /// 智能符号 press2 的提交必须带 flags bit3，C++ 端据此改走「覆盖 held 符号」而非
    /// 「并入前缀一起上屏」。漏置位的后果是 press2 打出「。.」而不是「.」。
    #[test]
    fn test_encode_commit_text_replacing_held_sets_flag_bit3() {
        let buf = encode_commit_text_replacing_held(".", true);
        let p = IpcHeader::SIZE;
        let flags = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
        assert_eq!(flags & 0x08, 0x08, "replacingHeld 位必须置位");
        assert_eq!(flags & 0x04, 0x04, "chineseMode 位应透传");
        assert_eq!(flags & 0x03, 0, "不得误置 modeChanged / hasNewComposition");
        // 命令码仍是 CMD_COMMIT_TEXT——复用同一响应类型，只靠 flags 区分语义
        assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), CMD_COMMIT_TEXT);
        assert_eq!(&buf[p + 12..], b".");
    }

    /// 反向守卫：普通提交路径绝不能带上 bit3，否则 hold 期间任何上屏都会吞掉待定符号。
    #[test]
    fn test_encode_commit_text_never_sets_replacing_held() {
        for (comp, mode_changed, cn, has_comp) in
            [(None, false, false, false), (Some("ni"), true, true, true)]
        {
            let buf = encode_commit_text("　", comp, mode_changed, cn, has_comp);
            let p = IpcHeader::SIZE;
            let flags = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
            assert_eq!(flags & 0x08, 0, "普通 CommitText 不得置 replacingHeld");
        }
    }

    #[test]
    fn test_encode_commit_then_defer_layout() {
        let buf = encode_commit_then_defer(150, "可能", "y");
        let commit = "可能".as_bytes(); // 6 字节
        let defer = "y".as_bytes(); // 1 字节
        // header + timeout(4) + commit_len(4) + defer_len(4) + commit + defer
        assert_eq!(buf.len(), IpcHeader::SIZE + 12 + commit.len() + defer.len());
        // 命令码在 header 内
        let cmd = u16::from_le_bytes([buf[2], buf[3]]);
        assert_eq!(cmd, CMD_COMMIT_THEN_DEFER);
        // payload 起始处 timeout_ms
        let p = IpcHeader::SIZE;
        assert_eq!(
            u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]),
            150
        );
        assert_eq!(
            u32::from_le_bytes([buf[p + 4], buf[p + 5], buf[p + 6], buf[p + 7]]),
            commit.len() as u32
        );
        assert_eq!(
            u32::from_le_bytes([buf[p + 8], buf[p + 9], buf[p + 10], buf[p + 11]]),
            defer.len() as u32
        );
    }

    /// GH#175：占位档位按 kind 原样编成 1 字节；值与 C++ `PlaceholderForKind` 一一对应。
    #[test]
    fn composition_placeholder_value_is_the_kind_byte() {
        use crate::protocol::{
            COMPOSITION_PLACEHOLDER_BLANK, COMPOSITION_PLACEHOLDER_SPACE,
            COMPOSITION_PLACEHOLDER_ZWSP,
        };
        assert_eq!(COMPOSITION_PLACEHOLDER_SPACE, 0);
        assert_eq!(COMPOSITION_PLACEHOLDER_ZWSP, 1);
        assert_eq!(COMPOSITION_PLACEHOLDER_BLANK, 2);
        for k in [
            COMPOSITION_PLACEHOLDER_SPACE,
            COMPOSITION_PLACEHOLDER_ZWSP,
            COMPOSITION_PLACEHOLDER_BLANK,
        ] {
            assert_eq!(encode_composition_placeholder_value(k), vec![k]);
        }
    }

    /// 多行上屏方式：commit = 0、paste = 1，与 C++ 侧的判读一致。
    #[test]
    fn multiline_commit_value_is_one_byte() {
        assert_eq!(encode_multiline_commit_value(false), vec![0]);
        assert_eq!(encode_multiline_commit_value(true), vec![1]);
    }
}

// ── darwin host-render push 帧编码器 (W4) ──
// 字节布局对照 Swift wind_macos/.../BinaryCodec.swift decoder。均小端，返回完整帧。

/// 追加一个长度前缀(u32 LE)的 UTF-8 字符串
fn push_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// 组帧：IpcHeader(cmd,len) + payload
fn frame(cmd: u16, payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(IpcHeader::SIZE + payload.len());
    out.extend_from_slice(&IpcHeader::new(cmd, payload.len() as u32).to_bytes());
    out.extend_from_slice(&payload);
    out
}

/// CmdHostRenderFrame (0x0502): seq:u32 + x:i32 + y:i32 + w:u32 + h:u32 + flags:u32 + scale:u32 (28B)
pub fn encode_host_render_frame(
    seq: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    flags: u32,
    scale: u32,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(28);
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&x.to_le_bytes());
    p.extend_from_slice(&y.to_le_bytes());
    p.extend_from_slice(&w.to_le_bytes());
    p.extend_from_slice(&h.to_le_bytes());
    p.extend_from_slice(&flags.to_le_bytes());
    p.extend_from_slice(&scale.to_le_bytes());
    frame(CMD_HOST_RENDER_FRAME, p)
}

/// CmdCandidateRects (0x0503): count:u32 + count×(index,x,y,w,h 各 i32 LE)。
/// index<0 为翻页按钮 (-1=上页 -2=下页)。坐标为 panel-local。
pub fn encode_candidate_rects(rects: &[(i32, i32, i32, i32, i32)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + rects.len() * 20);
    p.extend_from_slice(&(rects.len() as u32).to_le_bytes());
    for (idx, x, y, w, h) in rects {
        for v in [idx, x, y, w, h] {
            p.extend_from_slice(&v.to_le_bytes());
        }
    }
    frame(CMD_CANDIDATE_RECTS, p)
}

/// CmdModeStatus (0x0504): flags:u32 + effective_mode:u32 + labelLen:u32 + label(UTF-8)
pub fn encode_mode_status(flags: u32, effective_mode: u32, label: &str) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&flags.to_le_bytes());
    p.extend_from_slice(&effective_mode.to_le_bytes());
    push_string(&mut p, label);
    frame(CMD_MODE_STATUS, p)
}

/// 统一菜单树的线格式节点（wind-ipc 本地类型，避免反向依赖 wind-ui）。
/// 上游（coordinator）把 `MenuItemSpec` 映射为此结构后编码。
#[derive(Debug, Clone, Default)]
pub struct MenuNode {
    /// 菜单 id（macOS .app 经 NSMenuItem.tag 回传；分隔线/子菜单父项为 0）。
    pub id: i32,
    pub separator: bool,
    pub checked: bool,
    pub disabled: bool,
    pub label: String,
    pub children: Vec<MenuNode>,
}

/// CmdMenuShow (0x0506): 统一菜单树（响应 CmdShowContextMenu）。
/// 递归布局，与 Swift `BinaryCodec.decodeMenuItems` 对齐：
///   count:u32 + count×item；item = id:i32 + flags:u8 + labelLen:u32 + label(UTF-8) + children(递归)
///   flags 位：bit0=separator bit1=checked bit2=disabled
pub fn encode_menu_show(items: &[MenuNode]) -> Vec<u8> {
    let mut p = Vec::new();
    push_menu_items(&mut p, items);
    frame(CMD_MENU_SHOW, p)
}

fn push_menu_items(out: &mut Vec<u8>, items: &[MenuNode]) {
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for it in items {
        out.extend_from_slice(&it.id.to_le_bytes());
        let flags = (it.separator as u8) | ((it.checked as u8) << 1) | ((it.disabled as u8) << 2);
        out.push(flags);
        push_string(out, &it.label);
        push_menu_items(out, &it.children);
    }
}

/// 写单个按键 combo：keyLen u32 + key + modCount u32 + modCount×(modLen u32 + mod)。
/// 与 Swift `BinaryCodec.decodeCombo` 对齐。
fn push_key_combo(out: &mut Vec<u8>, key: &str, mods: &[String]) {
    push_string(out, key);
    out.extend_from_slice(&(mods.len() as u32).to_le_bytes());
    for m in mods {
        push_string(out, m);
    }
}

/// CmdKeyTap (0x050E): 单个 combo。key 为 canonical 键名（如 "v"/"enter"/"left"），
/// mods 为 {"ctrl","shift","alt","win"} 子集（win 在 .app 侧映射 Command）。
pub fn encode_key_tap(key: &str, mods: &[String]) -> Vec<u8> {
    let mut p = Vec::new();
    push_key_combo(&mut p, key, mods);
    frame(CMD_KEY_TAP, p)
}

/// CmdKeyHold (0x0510): 单个 combo（按下保持）。
pub fn encode_key_hold(key: &str, mods: &[String]) -> Vec<u8> {
    let mut p = Vec::new();
    push_key_combo(&mut p, key, mods);
    frame(CMD_KEY_HOLD, p)
}

/// CmdKeyRelease (0x0511): 单个 combo（抬起）。
pub fn encode_key_release(key: &str, mods: &[String]) -> Vec<u8> {
    let mut p = Vec::new();
    push_key_combo(&mut p, key, mods);
    frame(CMD_KEY_RELEASE, p)
}

/// CmdKeySeq (0x050F): comboCount u32 + comboCount×combo。
pub fn encode_key_seq(combos: &[(String, Vec<String>)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(combos.len() as u32).to_le_bytes());
    for (key, mods) in combos {
        push_key_combo(&mut p, key, mods);
    }
    frame(CMD_KEY_SEQ, p)
}

/// CmdKeyType (0x0512): 整段 UTF-8 文本（无长度前缀），.app 走 insertText 上屏。
pub fn encode_key_type(text: &str) -> Vec<u8> {
    frame(CMD_KEY_TYPE, text.as_bytes().to_vec())
}

/// [`CMD_OVERLAY_FRAME`] 的元数据（像素在该层的 SHM 段里）。枚举值见 [`crate::protocol::overlay`]。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlayFrameMeta {
    pub kind: u32,
    /// 该层 SHM 段写帧后的序号；隐藏帧同样写段（`write_hidden`）后取号。
    pub seq: u32,
    /// 位图尺寸（含软阴影扩边）。隐藏帧为 0。
    pub width: u32,
    pub height: u32,
    /// `SharedRenderHeader::FLAG_*`：`FLAG_VISIBLE` 缺席即隐藏该层。
    pub flags: u32,
    pub place: u32,
    /// 内容盒首选左上（屏幕坐标）。
    pub x: i32,
    pub y: i32,
    /// `FLIP` / `FOLLOW_CANDIDATE` 的备选左上（右溢 / 下溢时用）。
    pub alt_x: i32,
    pub alt_y: i32,
    pub anchor: u32,
    /// `ANCHOR` 离工作区边缘的留白（像素）。
    pub margin: i32,
    /// 内容盒在位图内的偏移与尺寸（偏移 = 软阴影左/上扩边）。
    pub content_x: i32,
    pub content_y: i32,
    pub content_w: u32,
    pub content_h: u32,
    /// 自动隐藏时长；`0` = 常驻，直到下一帧或隐藏帧。计时归宿主（同 macOS `.app`）。
    pub duration_ms: i32,
    /// 位图像素 / 宿主坐标单位。Linux 宿主坐标就是像素（恒 1.0，addon 不读这个字段）；macOS 坐标是点、
    /// 位图是 `scale` 倍的像素，`.app` 据此把位图尺寸与内容盒折回点。`0.0` = 未设，按 1 处理。
    pub scale: f32,
    /// 可拖动区域（位图内 `x, y, w, h`），仅 [`OVERLAY_KIND_TOOLBAR`] / [`OVERLAY_KIND_SOFTKEYBOARD`]
    /// 的帧会编码，其它层忽略此字段。
    pub drag: Vec<[i32; 4]>,
}

/// CmdOverlayFrame (0x0513, 72B)，均小端：
///
/// kind seq w h flags place（u32）+ x y alt_x alt_y（i32）+ anchor（u32）+ margin content_x content_y
/// （i32）+ content_w content_h（u32）+ duration_ms（i32）+ scale（f32）。
///
/// 前 68 字节是 Linux addon 认的基础布局（它只要求 ≥ 68），末尾 `scale` 供 macOS `.app` 读。
///
/// 工具栏 / 软键盘（kind 10 / 11）在其后多一段拖动区：`count u32` + `count × (x y w h) i32`。
/// 其它 kind 不编码这一段——macOS Swift 解码器不认尾段，绝不能让它收到。
pub fn encode_overlay_frame(m: &OverlayFrameMeta) -> Vec<u8> {
    let mut p = Vec::with_capacity(72);
    for v in [m.kind, m.seq, m.width, m.height, m.flags, m.place] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    for v in [m.x, m.y, m.alt_x, m.alt_y] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    p.extend_from_slice(&m.anchor.to_le_bytes());
    for v in [m.margin, m.content_x, m.content_y] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    for v in [m.content_w, m.content_h] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    p.extend_from_slice(&m.duration_ms.to_le_bytes());
    p.extend_from_slice(&m.scale.to_le_bytes());
    if matches!(m.kind, OVERLAY_KIND_TOOLBAR | OVERLAY_KIND_SOFTKEYBOARD) {
        p.extend_from_slice(&(m.drag.len() as u32).to_le_bytes());
        for r in &m.drag {
            for v in r {
                p.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    frame(CMD_OVERLAY_FRAME, p)
}

/// [`CMD_PANEL_POINTER`] 的载荷（20 字节，小端）：kind event button（u32）+ x y（i32）。
pub fn encode_panel_pointer(kind: u32, event: u32, button: u32, x: i32, y: i32) -> Vec<u8> {
    let mut p = Vec::with_capacity(20);
    for v in [kind, event, button] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    for v in [x, y] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    p
}

/// 解 [`encode_panel_pointer`]；不足 20 字节返回 `None`，多出的尾部忽略。
pub fn decode_panel_pointer(p: &[u8]) -> Option<(u32, u32, u32, i32, i32)> {
    if p.len() < 20 {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
    Some((u(0), u(4), u(8), u(12) as i32, u(16) as i32))
}

/// 编码配置同步消息 (CMD_SYNC_CONFIG 0x0303)
///
/// 载荷格式（对齐 TSF IPCClient async reader CONFIG_SYNC handler）：
/// [keyLen: 2 bytes LE] [valueLen: 4 bytes LE] [key: UTF-8] [value: bytes]
pub fn encode_sync_config(key: &str, value: &[u8]) -> Vec<u8> {
    let key_bytes = key.as_bytes();
    let mut payload = Vec::with_capacity(2 + 4 + key_bytes.len() + value.len());
    payload.extend_from_slice(&(key_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
    payload.extend_from_slice(key_bytes);
    payload.extend_from_slice(value);
    frame(CMD_SYNC_CONFIG, payload)
}

/// 编码英文自动配对配置的值部分（对齐 TSF KeyEventSink::OnSyncConfig CONFIG_KEY_ENGLISH_PAIRS）
///
/// 格式：enabled(u8) + count(u8) + [left:u16(LE) + right:u16(LE)]...
pub fn encode_english_pairs_value(enabled: bool, pairs: &[(char, char)]) -> Vec<u8> {
    let mut value = Vec::with_capacity(2 + pairs.len() * 4);
    value.push(enabled as u8);
    value.push(pairs.len() as u8);
    for (left, right) in pairs {
        value.extend_from_slice(&(*left as u16).to_le_bytes());
        value.extend_from_slice(&(*right as u16).to_le_bytes());
    }
    value
}

/// 编码密码框抑制策略开关的值部分（对齐 TSF `OnSyncConfig` 的 CONFIG_KEY_PASSWORD_SUPPRESS）。
/// 格式：enabled(u8)。DLL 需要它才能在 `OnTestKeyDown` 本地判定是否放行——吃键决策早于 IPC。
pub fn encode_password_suppress_value(enabled: bool) -> Vec<u8> {
    vec![enabled as u8]
}

/// 编码组合区兜底占位字符的值部分（对齐 TSF `OnSyncConfig` 的 CONFIG_KEY_COMPOSITION_PLACEHOLDER）。
/// 格式：kind(u8)，取 `protocol::COMPOSITION_PLACEHOLDER_*`（0 = 空格 U+0020、1 = ZWSP U+200B、
/// 2 = 盲文空白 U+2800）。旧 DLL 认不出的值回落空格。GH#175。
pub fn encode_composition_placeholder_value(kind: u8) -> Vec<u8> {
    vec![kind]
}

/// 编码多行上屏方式的值部分（对齐 TSF `OnSyncConfig` 的 CONFIG_KEY_MULTILINE_COMMIT）。
/// 格式：mode(u8)，0 = commit、1 = paste。
pub fn encode_multiline_commit_value(paste: bool) -> Vec<u8> {
    vec![paste as u8]
}

/// 编码「本进程可能命中 compat 标题规则」开关的值部分（对齐 TSF `OnSyncConfig` 的
/// CONFIG_KEY_COMPAT_TITLE_MATCH）。格式：enabled(u8)。默认关，关闭时 DLL 不采集窗口标题。
pub fn encode_compat_title_match_value(enabled: bool) -> Vec<u8> {
    vec![enabled as u8]
}

/// 编码诊断快照采集开关的值部分（对齐 TSF `OnSyncConfig` 的 CONFIG_KEY_DIAG_SNAPSHOT）。
/// 格式：enabled(u8)。默认关，随输入诊断 HUD 显隐推送；关闭时 DLL 完全不采集。
pub fn encode_diag_snapshot_value(enabled: bool) -> Vec<u8> {
    vec![enabled as u8]
}

/// 编码配对跳出键的值部分（对齐 TSF KeyEventSink::OnSyncConfig CONFIG_KEY_JUMP_OUT_KEYS）。
///
/// 格式：right_symbol(u8) + count(u8) + [vk:u16(LE)]...
///
/// `right_symbol` = 输入右符号本身是否跳出（配置里的 `right_symbol` 特殊值）；它不是键名，
/// 右符号是哪个键取决于配对表，故与 VK 列表分开编码。`vks` 为 VK 码列表（调用方应去重、
/// 排序以稳定输出）。
pub fn encode_jump_out_keys_value(right_symbol: bool, vks: &[u32]) -> Vec<u8> {
    let mut value = Vec::with_capacity(2 + vks.len() * 2);
    value.push(right_symbol as u8);
    value.push(vks.len() as u8);
    for vk in vks {
        value.extend_from_slice(&(*vk as u16).to_le_bytes());
    }
    value
}

/// 编码配对状态时效（对齐 TSF `OnSyncConfig` CONFIG_KEY_PAIR_STATE_TTL）。
///
/// 格式：secs(u16 LE)。`0` = 不过期。上限取 u16（约 18 小时），超出即饱和——
/// 再长的时效与「不过期」在实际使用上没有区别。
pub fn encode_pair_state_ttl_value(secs: u32) -> Vec<u8> {
    (secs.min(u16::MAX as u32) as u16).to_le_bytes().to_vec()
}

/// 编码 Ctrl+空格 切换开关的值部分（对齐 TSF `OnSyncConfig` 的 CONFIG_KEY_CTRL_SPACE_TOGGLE）。
/// 格式：enabled(u8)。GH#172。
pub fn encode_ctrl_space_toggle_value(enabled: bool) -> Vec<u8> {
    vec![enabled as u8]
}

/// 编码空格行自定义映射的列掩码（对齐 TSF `OnSyncConfig` 的 CONFIG_KEY_SPACE_CUSTOM_COLS）。
/// 格式：mask(u8)。
pub fn encode_space_custom_cols_value(mask: u8) -> Vec<u8> {
    vec![mask]
}

/// 编码「英半列有自定义映射的源字符集合」（对齐 TSF `OnSyncConfig` CONFIG_KEY_CUSTOM_EN_PUNCT）。
///
/// 格式：count(u8) + [ch:u16(LE)]...  源字符均为 ASCII 标点，一个 UTF-16 单元足够。
/// `chars` 应已去重排序（见 `wind_punct::custom_english_punct_chars`）以稳定输出。
pub fn encode_custom_en_punct_value(chars: &[char]) -> Vec<u8> {
    let n = chars.len().min(u8::MAX as usize);
    let mut value = Vec::with_capacity(1 + n * 2);
    value.push(n as u8);
    for c in chars.iter().take(n) {
        value.extend_from_slice(&(*c as u16).to_le_bytes());
    }
    value
}

#[cfg(test)]
mod custom_en_punct_tests {
    use super::*;

    #[test]
    fn layout_is_count_then_utf16_le() {
        assert_eq!(encode_custom_en_punct_value(&[]), vec![0]);
        // '"' = 0x22, '\'' = 0x27
        assert_eq!(
            encode_custom_en_punct_value(&['"', '\'']),
            vec![2, 0x22, 0x00, 0x27, 0x00]
        );
    }
}

#[cfg(test)]
mod darwin_push_tests {
    use super::*;

    fn cmd_of(frame: &[u8]) -> u16 {
        u16::from_le_bytes([frame[2], frame[3]])
    }

    #[test]
    fn host_render_frame_layout_is_28_bytes_le() {
        let f = encode_host_render_frame(7, -3, 20, 100, 40, 0x3, 2);
        assert_eq!(f.len(), 8 + 28);
        assert_eq!(cmd_of(&f), CMD_HOST_RENDER_FRAME);
        let p = &f[8..];
        assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 7);
        assert_eq!(i32::from_le_bytes(p[4..8].try_into().unwrap()), -3);
        assert_eq!(i32::from_le_bytes(p[8..12].try_into().unwrap()), 20);
        assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 100);
        assert_eq!(u32::from_le_bytes(p[16..20].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(p[20..24].try_into().unwrap()), 0x3);
        assert_eq!(u32::from_le_bytes(p[24..28].try_into().unwrap()), 2);
    }

    #[test]
    fn overlay_frame_layout_is_72_bytes_le() {
        let f = encode_overlay_frame(&OverlayFrameMeta {
            kind: 2,
            seq: 9,
            width: 120,
            height: 40,
            flags: 0x5,
            place: 1,
            x: -30,
            y: 200,
            alt_x: -31,
            alt_y: 150,
            anchor: 7,
            margin: 16,
            content_x: 3,
            content_y: 4,
            content_w: 110,
            content_h: 30,
            duration_ms: 1500,
            scale: 2.0,
            drag: vec![],
        });
        assert_eq!(f.len(), 8 + 72);
        assert_eq!(cmd_of(&f), CMD_OVERLAY_FRAME);
        let p = &f[8..];
        let u = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        assert_eq!(
            (u(0), u(4), u(8), u(12), u(16), u(20)),
            (2, 9, 120, 40, 0x5, 1)
        );
        assert_eq!((i(24), i(28), i(32), i(36)), (-30, 200, -31, 150));
        assert_eq!(u(40), 7);
        assert_eq!((i(44), i(48), i(52)), (16, 3, 4));
        assert_eq!((u(56), u(60)), (110, 30));
        assert_eq!(i(64), 1500);
        assert_eq!(f32::from_le_bytes(p[68..72].try_into().unwrap()), 2.0);
    }

    #[test]
    fn overlay_frame_drag_tail_only_for_panel_kinds() {
        let base = OverlayFrameMeta {
            kind: 3,
            drag: vec![[1, 2, 3, 4], [5, 6, 7, 8]],
            ..Default::default()
        };
        // 非交互层：带不带拖动区都是 72 字节，逐字节相同。
        let plain = encode_overlay_frame(&base);
        assert_eq!(plain.len(), 8 + 72);
        let no_drag = encode_overlay_frame(&OverlayFrameMeta {
            drag: vec![],
            ..base.clone()
        });
        assert_eq!(plain, no_drag);

        // 工具栏：多 4 + 2*16 字节，并能原样解回。
        let f = encode_overlay_frame(&OverlayFrameMeta {
            kind: OVERLAY_KIND_TOOLBAR,
            ..base.clone()
        });
        assert_eq!(f.len(), 8 + 72 + 4 + 2 * 16);
        let p = &f[8 + 72..];
        let i = |o: usize| i32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        assert_eq!(i(0), 2);
        let rects: Vec<[i32; 4]> = (0..2)
            .map(|k| std::array::from_fn(|j| i(4 + k * 16 + j * 4)))
            .collect();
        assert_eq!(rects, base.drag);

        // 软键盘同样带尾段；零个拖动区只多 count。
        let f = encode_overlay_frame(&OverlayFrameMeta {
            kind: OVERLAY_KIND_SOFTKEYBOARD,
            drag: vec![],
            ..base
        });
        assert_eq!(f.len(), 8 + 72 + 4);
    }

    #[test]
    fn panel_pointer_roundtrip() {
        let p = encode_panel_pointer(11, 3, 1, -5, 40);
        assert_eq!(p.len(), 20);
        assert_eq!(decode_panel_pointer(&p), Some((11, 3, 1, -5, 40)));
        assert_eq!(decode_panel_pointer(&p[..19]), None);
    }

    #[test]
    fn candidate_rects_layout_count_then_5xi32() {
        let f = encode_candidate_rects(&[(0, 1, 2, 30, 24), (-1, 5, 6, 12, 12)]);
        assert_eq!(cmd_of(&f), CMD_CANDIDATE_RECTS);
        let p = &f[8..];
        assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(p[4..8].try_into().unwrap()), 0);
        assert_eq!(i32::from_le_bytes(p[8..12].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(p[24..28].try_into().unwrap()), -1);
    }

    #[test]
    fn mode_status_label_utf8_length_prefixed() {
        let f = encode_mode_status(0x5, 1, "五笔");
        assert_eq!(cmd_of(&f), CMD_MODE_STATUS);
        let p = &f[8..];
        assert_eq!(u32::from_le_bytes(p[0..4].try_into().unwrap()), 0x5);
        assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 1);
        let n = u32::from_le_bytes(p[8..12].try_into().unwrap()) as usize;
        assert_eq!(n, "五笔".len());
        assert_eq!(&p[12..12 + n], "五笔".as_bytes());
    }

    // 递归解码器：镜像 Swift BinaryCodec.decodeMenuItems，作为 encode_menu_show 的规范验证。
    struct DecodedItem {
        id: i32,
        flags: u8,
        label: String,
        children: Vec<DecodedItem>,
    }
    fn decode_menu_items(p: &[u8], off: &mut usize) -> Vec<DecodedItem> {
        let n = u32::from_le_bytes(p[*off..*off + 4].try_into().unwrap()) as usize;
        *off += 4;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let id = i32::from_le_bytes(p[*off..*off + 4].try_into().unwrap());
            *off += 4;
            let flags = p[*off];
            *off += 1;
            let ln = u32::from_le_bytes(p[*off..*off + 4].try_into().unwrap()) as usize;
            *off += 4;
            let label = String::from_utf8(p[*off..*off + ln].to_vec()).unwrap();
            *off += ln;
            let children = decode_menu_items(p, off);
            out.push(DecodedItem {
                id,
                flags,
                label,
                children,
            });
        }
        out
    }

    #[test]
    fn menu_show_roundtrips_nested_tree_le() {
        let tree = vec![
            MenuNode {
                id: 100,
                label: "英文".into(),
                checked: true,
                ..Default::default()
            },
            MenuNode {
                id: 0,
                label: "主题".into(),
                children: vec![
                    MenuNode {
                        id: 2000,
                        label: "默认".into(),
                        checked: true,
                        ..Default::default()
                    },
                    MenuNode {
                        separator: true,
                        ..Default::default()
                    },
                    MenuNode {
                        id: 4001,
                        label: "亮色".into(),
                        disabled: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ];
        let f = encode_menu_show(&tree);
        assert_eq!(cmd_of(&f), CMD_MENU_SHOW);
        let p = &f[8..];
        let mut off = 0usize;
        let top = decode_menu_items(p, &mut off);
        assert_eq!(off, p.len(), "整帧应被消费完（无游离字节）");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].id, 100);
        assert_eq!(top[0].flags & 0x02, 0x02); // checked
        assert_eq!(top[0].label, "英文");
        assert!(top[0].children.is_empty());
        // 子菜单父项 id=0、无勾选，含 3 子项
        assert_eq!(top[1].label, "主题");
        assert_eq!(top[1].id, 0);
        let sub = &top[1].children;
        assert_eq!(sub.len(), 3);
        assert_eq!(sub[0].id, 2000);
        assert_eq!(sub[0].flags & 0x02, 0x02); // checked
        assert_eq!(sub[1].flags & 0x01, 0x01); // separator
        assert_eq!(sub[2].id, 4001);
        assert_eq!(sub[2].flags & 0x04, 0x04); // disabled
    }
}
