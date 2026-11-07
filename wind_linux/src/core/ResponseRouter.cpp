#include "ResponseRouter.h"

#include <algorithm>

#include "ExtProtocol.h"
#include "Protocol.h"
#include "Utf.h"

namespace windlinux {

namespace {

/// 码点 → seqChar 口径：辅助平面取高代理（服务端据此把 2/3 号平面认成汉字）。
uint16_t toSeqUnit(uint32_t cp)
{
    return cp >= 0x10000 ? uint16_t(0xD800 + ((cp - 0x10000) >> 10)) : uint16_t(cp);
}

/// 首个码点；空串返回 0。
uint32_t firstCodepoint(const std::string& utf8)
{
    const size_t end = std::min<size_t>(utf8.size(), 4);
    for (size_t n = 1; n <= end; ++n) {
        if (n == utf8.size() || (uint8_t(utf8[n]) & 0xC0) != 0x80) {
            return lastCodepoint(utf8.substr(0, n));
        }
    }
    return utf8.empty() ? 0 : uint8_t(utf8[0]);
}

} // namespace

uint16_t DigitTracker::digitChar(uint32_t vk, uint32_t modifiers)
{
    // Ctrl/Alt/Win + 数字是宿主快捷键，一个字符都不往文档写；记了就是幻影数字。
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_WIN)) {
        return 0;
    }
    if (vk >= 0x30 && vk <= 0x39) {
        // Shift + 主键盘数字产出的是符号（!@#…）而非数字。
        return (modifiers & KEYMOD_SHIFT) ? 0 : uint16_t(vk);
    }
    if (vk >= 0x60 && vk <= 0x69) { // VK_NUMPAD0..9：主键盘与小键盘都要认
        return uint16_t(0x30 + (vk - 0x60));
    }
    return 0;
}

void DigitTracker::noteCommittedText(const std::string& utf8)
{
    if (utf8.empty()) {
        return;
    }
    uint32_t last = lastCodepoint(utf8);
    prevChar_ = (last >= 0x30 && last <= 0x39) ? uint16_t(last) : 0;
}

uint16_t SeqTracker::charFromVk(uint32_t vk, uint32_t modifiers)
{
    if (modifiers & (KEYMOD_CTRL | KEYMOD_ALT | KEYMOD_WIN)) {
        return kUnknown; // 宿主快捷键：粘贴进来的是什么看不见
    }
    bool shift = (modifiers & KEYMOD_SHIFT) != 0;
    if (vk >= 0x30 && vk <= 0x39) {
        return shift ? uint16_t('?') : uint16_t(vk); // Shift+数字是 !@#…
    }
    if (vk >= 0x41 && vk <= 0x5A) {
        return 'a';
    }
    if (vk >= 0x60 && vk <= 0x69) {
        return uint16_t(0x30 + (vk - 0x60));
    }
    if (vk == 0x20) {
        return ' ';
    }
    if ((vk >= 0x6A && vk <= 0x6F) || (vk >= 0xBA && vk <= 0xC0) || (vk >= 0xDB && vk <= 0xDE) ||
        vk == 0xE2) {
        return '?';
    }
    return kUnknown;
}

void SeqTracker::noteCommittedText(const std::string& utf8)
{
    if (utf8.empty()) {
        return;
    }
    seqChar_ = toSeqUnit(lastCodepoint(utf8));
    writtenFirst_ = int32_t(firstCodepoint(utf8));
    // 预期光标按字符（码点）前进，与 Fcitx 周边文本的光标量纲一致。
    int chars = 0;
    for (unsigned char c : utf8) {
        chars += (c & 0xC0) != 0x80;
    }
    advanceCursor(chars, true);
}

bool SeqTracker::matchesHostChar(uint32_t cp) const
{
    if (seqChar_ == kUnknown) {
        return true;
    }
    if (cp == 0) {
        return false; // 记着有字，宿主说光标前没字（点到了行首）
    }
    const uint16_t u = toSeqUnit(cp);
    auto isAsciiAlpha = [](uint32_t c) { return (c | 0x20) >= 'a' && (c | 0x20) <= 'z'; };
    auto isDigit = [](uint32_t c) { return c >= '0' && c <= '9'; };
    if (seqChar_ == 'a') {
        return isAsciiAlpha(u); // 透传字母的代表值
    }
    if (seqChar_ == '?') {
        // 透传符号的代表值：按什么键出什么符号看布局，只要求不是字母、数字、空白。
        return !isAsciiAlpha(u) && !isDigit(u) && u != ' ' && u != '\t' && u != 0x3000;
    }
    return u == seqChar_;
}

void ResponseRouter::reset()
{
    compositionText_.clear();
    digits_.reset();
    seq_.reset();
    hasHeld_ = false;
    heldSymbol_.clear();
    pendingCommitPrefix_.clear();
    cancelHoldTimer();
}

bool ResponseRouter::apply(const Frame& frame, TextSink* sink, bool hostShortcut)
{
    switch (frame.cmd) {
    case CMD_PASS_THROUGH:
        // 按键交回宿主 → 待定标点必须**先真上屏**：服务端已清 held_text 并按上屏记过账。
        flushPendingPrefix(sink);
        return false;

    case CMD_CONSUMED:
    case CMD_ACK:
        return true;

    case CMD_COMMIT_TEXT:
        if (auto p = decodeCommitText(frame.payload)) {
            applyCommitText(*p, sink);
        }
        return true;

    case CMD_COMMIT_TEXT_WITH_CURSOR:
        if (auto p = decodeCommitTextWithCursor(frame.payload)) {
            applyCommitTextWithCursor(*p, sink);
        }
        return true;

    case CMD_UPDATE_COMPOSITION:
        if (auto p = decodeUpdateComposition(frame.payload)) {
            applyUpdateComposition(*p, sink);
        }
        return true;

    case CMD_CLEAR_COMPOSITION:
        applyClearComposition(sink);
        // 快捷键组合触发的清组合：组合已清，但按键交还宿主。
        return !hostShortcut;

    case CMD_CLEAR_THEN_PASS_THROUGH:
        // 联想态回车/退格透传：组合要收掉，键要交还宿主——两件事都做。Windows 要靠
        // SendInput 重放；Fcitx5 引擎与 IMKit 一样没有前置闸门，不 accept 即可。
        applyClearComposition(sink);
        return false;

    case CMD_KEY_TYPE: {
        // 命令直通车 key.type / clip.paste：整段文本直接上屏，不经组合。
        std::string text = decodeKeyType(frame.payload);
        if (!text.empty()) {
            insertCommitted(text, sink);
        }
        return true;
    }

    case CMD_MOVE_CURSOR:
        // 智能跳过：输入右标点时跳过已自动补全的右标点。direction=1 右移。
        if (auto dir = decodeMoveCursor(frame.payload); dir && *dir == 1) {
            if (sink) {
                sink->moveCursor(1);
            }
            // 光标跨过了那个右标点，光标前必是标点（恒非数字），清零即准确。
            digits_.reset();
            seq_.noteCursorCrossedSymbol();
        }
        return true;

    case CMD_DELETE_PAIR:
        // 预留：协调器当前不产出此响应（Windows/macOS 均未实装成对删除）。
        return true;

    case CMD_REPLACE_BACKWARD:
        // 智能符号：删除光标前 count 个字符并插入替换文本。宿主不支持删除时降级为仅插入
        // （不删除），保证不误删——与 macOS 取不到 selectedRange 时的降级一致。
        if (auto p = decodeReplaceBackward(frame.payload); p && sink) {
            if (p->text.empty() && p->count > 0) {
                digits_.reset(); // 纯删除（ime.undo_commit）：被删掉的数字不能幸存
                seq_.reset();
            }
            if (p->count > 0) {
                seq_.forgetCursor(); // 删了字：预期光标不再按字数推
                sink->deleteBeforeCursor(std::min(p->count, kMaxDeleteUnits));
            }
            insertCommitted(p->text, sink);
        }
        return true;

    case CMD_HOLD_COMPOSITION:
        // 智能符号 press1：待定标点作为组合预览显示。
        if (auto p = decodeHoldComposition(frame.payload)) {
            absorbHeldIntoPrefix(); // 前一个待定标点先定格（连打两个不同符号）
            hasHeld_ = true;
            heldSymbol_ = p->holdText;
            setCompositionRaw(pendingCommitPrefix_ + p->holdText, sink);
            armHoldTimer(p->timeoutMs);
        }
        return true;

    case CMD_COMMIT_AND_HOLD:
    case CMD_COMMIT_THEN_DEFER:
        // 先真上屏 commitText，再把余码/待定文本开成新组合。commitThenDefer 是码表顶码
        // direct_commit 的正常通路（跨平台，不是 Windows 专有）。Windows 的「延迟到触发键
        // keyup 才开组合」是 TSF 组合边界的规避手段，Fcitx5 无此约束，立即开即可。
        if (auto p = decodeCommitAndHold(frame.payload)) {
            CommitTextPayload c;
            c.flags = p->holdText.empty() ? 0 : COMMIT_FLAG_HAS_NEW_COMPOSITION;
            c.text = p->commitText;
            c.newComposition = p->holdText;
            applyCommitText(c, sink);
            // commitAndHold 的 holdText 是**待定标点**，须登记成 held，否则下一次
            // UpdateComposition 会把它覆盖掉；commitThenDefer 的 holdText 是余码，不登记。
            if (frame.cmd == CMD_COMMIT_AND_HOLD && !p->holdText.empty()) {
                hasHeld_ = true;
                heldSymbol_ = p->holdText;
            }
        }
        return true;

    default:
        return true; // 未知 cmd（含 STATUS_UPDATE / MODE_PUSH）：默认消费，避免重复出字符
    }
}

void ResponseRouter::applyCommitText(const CommitTextPayload& p, TextSink* sink)
{
    // hold 预览态活跃时，本次提交必须交代那个待定符号的去向（规则逐字对齐 Windows
    // `CTextService::CommitText` 与 Swift 同名函数）：
    //   replacingHeld (press2)：本就是要拿英文符号换掉它 → 丢弃；
    //   其余一切：并入前缀，与本次文本一起上屏（追加语义是安全的默认）。
    bool replacingHeld = (p.flags & COMMIT_FLAG_REPLACING_HELD) != 0;
    if (replacingHeld) {
        hasHeld_ = false;
        heldSymbol_.clear();
    } else {
        absorbHeldIntoPrefix();
    }
    std::string prefix = pendingCommitPrefix_;
    pendingCommitPrefix_.clear();
    // 先上屏、再收掉旧组合，与 Fcitx5 自带引擎同序。Fcitx5 的 commitString 不会顺带清
    // client preedit（IMKit 的 insertText 自带「替换 marked text」语义，Swift 侧因此不需要
    // 这一步），不清的话宿主里会同时留着上屏文本和旧编码。反过来先清空预编辑，Electron
    // 输入框冷启动后的第一次会把随后的 CommitString 丢掉。
    insertCommitted(prefix + p.text, sink);
    clearPreeditIfAny(sink);

    if (!p.newComposition.empty()) {
        // 内联 preedit：commit 后立即开始新一轮组合
        compositionText_ = p.newComposition;
        applyMarkedText(p.newComposition, utf16Length(p.newComposition), sink);
    }
}

void ResponseRouter::applyCommitTextWithCursor(const CommitTextWithCursorPayload& p,
                                               TextSink* sink)
{
    insertCommitted(takePendingPrefix() + p.text, sink);
    clearPreeditIfAny(sink);
    // 自动配对插入 `（）` 后把光标退回到配对中间。
    if (p.cursorOffset > 0 && sink) {
        seq_.forgetCursor(); // 光标退回了配对中间
        sink->moveCursor(-static_cast<int>(std::min(p.cursorOffset, kMaxCursorMove)));
    }
}

void ResponseRouter::applyUpdateComposition(const UpdateCompositionPayload& p, TextSink* sink)
{
    // 待定标点定格进前缀，与新组合显示在同一段预编辑里；光标右移前缀长度（UTF-16）。
    absorbHeldIntoPrefix();
    std::string text = pendingCommitPrefix_ + p.text;
    size_t caret = utf16Length(pendingCommitPrefix_) + p.caretPos;
    compositionText_ = text;
    applyMarkedText(text, caret, sink);
}

void ResponseRouter::applyClearComposition(TextSink* sink)
{
    std::string prefix = takePendingPrefix();
    if (!prefix.empty()) {
        insertCommitted(prefix, sink);
    }
    if (sink) {
        sink->setPreedit("", 0);
    }
    compositionText_.clear();
}

void ResponseRouter::insertCommitted(const std::string& text, TextSink* sink)
{
    // 真写进文档的唯一出口：上屏 + 记账。sink 为空 = 什么都没写进文档，也就不记账。
    if (!sink) {
        return;
    }
    sink->commitText(text);
    digits_.noteCommittedText(text);
    seq_.noteCommittedText(text);
}

void ResponseRouter::applyMarkedText(const std::string& text, size_t caretUtf16, TextSink* sink)
{
    if (!sink) {
        return;
    }
    size_t len16 = utf16Length(text);
    if (caretUtf16 > len16) {
        caretUtf16 = len16;
    }
    sink->setPreedit(text, utf16OffsetToUtf8(text, caretUtf16));
    // 组合串也是「看得见地摆在光标前」的东西，同样记账；光标不在串尾时拿不准，清零。
    if (caretUtf16 >= len16) {
        digits_.noteCommittedText(text);
    } else {
        digits_.reset();
    }
}

void ResponseRouter::setCompositionRaw(const std::string& text, TextSink* sink)
{
    compositionText_ = text;
    applyMarkedText(text, utf16Length(text), sink);
}

void ResponseRouter::flushPendingPrefix(TextSink* sink)
{
    std::string prefix = takePendingPrefix();
    if (prefix.empty()) {
        return;
    }
    insertCommitted(prefix, sink);
    clearPreeditIfAny(sink);
}

void ResponseRouter::clearPreeditIfAny(TextSink* sink)
{
    // 上屏之后再调；为什么不能反过来见 applyCommitText。
    if (!compositionText_.empty() && sink) {
        sink->setPreedit("", 0);
    }
    compositionText_.clear();
}

void ResponseRouter::absorbHeldIntoPrefix()
{
    if (!hasHeld_) {
        return;
    }
    pendingCommitPrefix_ += heldSymbol_;
    hasHeld_ = false;
    heldSymbol_.clear();
    cancelHoldTimer();
}

std::string ResponseRouter::takePendingPrefix()
{
    absorbHeldIntoPrefix();
    cancelHoldTimer();
    std::string out = std::move(pendingCommitPrefix_);
    pendingCommitPrefix_.clear();
    return out;
}

void ResponseRouter::armHoldTimer(uint32_t ms)
{
    cancelHoldTimer();
    if (ms == 0 || !scheduler_) {
        return; // 0 = 不自动落定
    }
    uint64_t gen = holdGeneration_;
    scheduler_(ms, [this, gen]() {
        // 「代」变了说明这中间已经有过按键/提交/清除，那条路自己处置过了。
        if (holdGeneration_ != gen || !hasHeld_) {
            return;
        }
        // 到点把待定标点定稿成正文（内容不变，只收掉预编辑的下划线）。
        if (sinkProvider_) {
            sinkProvider_([this](TextSink* sink) { flushPendingPrefix(sink); });
        } else {
            flushPendingPrefix(nullptr);
        }
    });
}

} // namespace windlinux
