// 把服务回来的响应帧路由成对宿主文本框的操作（纯逻辑，不依赖 Fcitx5）。
//
// 这是 wind_macos/Sources/WindInputKit/IPC/BridgeResponseRouter.swift 的 C++ 移植，
// 语义逐条对齐（待定标点 held / 定格前缀 / hold 计时器 / 数字后智能标点记账）。
// 抽成纯逻辑的理由与 Swift 侧相同：单测不必起 Fcitx5。
//
// ⚠️ **新增下行 cmd 必须在 apply() 里显式接一臂**——default 是「消费按键但不出字」，
// 漏接的表现是按键被吃掉、屏幕上什么都没有（macOS 的历史案例：commitThenDefer 漏接
// 导致码表顶码上屏丢字）。判断要不要接只看**谁产出**，不看名字像不像 Windows 专有。
#pragma once

#include "Codec.h"

#include <cstdint>
#include <functional>
#include <string>

namespace windlinux {

/// 宿主文本框的抽象（Fcitx5 侧由 InputContext 实现，单测用桩）。
class TextSink {
public:
    virtual ~TextSink() = default;
    /// 真上屏。
    virtual void commitText(const std::string& utf8) = 0;
    /// 摆组合串（预编辑）。`utf8` 为空 = 清除组合。`caretBytes` 为组合内光标的 UTF-8 字节偏移。
    virtual void setPreedit(const std::string& utf8, size_t caretBytes) = 0;
    /// 删除光标前 `utf16Units` 个 **UTF-16 码元**（服务端的量纲，同 Windows / macOS）；实现方
    /// 按光标前文本换算成码点（`codepointsForUtf16Back`）。宿主不支持（无 surrounding text 能力）
    /// 时返回 false、什么都不做。调用方保证不超过 `kMaxDeleteUnits`。
    virtual bool deleteBeforeCursor(size_t utf16Units) = 0;
    /// 移动宿主光标 `chars` 个字符（负 = 左）。Fcitx5 侧合成方向键，一个方向键一个字符——
    /// 与 Windows（模拟 VK_LEFT）、macOS（合成方向键）同一量纲。调用方保证 |chars| ≤ `kMaxCursorMove`。
    virtual void moveCursor(int chars) = 0;
};

/// 数字后智能标点的 prevChar 记账——对位 Swift `SmartPunctDigitTracker`（语义与取舍见那边
/// 的长注释）。「数字后」**只认本记账**（2026-10-13 定，三平台一致）：宿主 surrounding text
/// 里的数字不算——那会把「光标恰好停在旧数字后面」也当成数字后，用户在那里打不出全角标点。
class DigitTracker {
public:
    uint16_t prevChar() const { return prevChar_; }
    /// 发给服务端的 prevChar（对位 Windows `WindSmartPunct::ResolvePrevChar`）。`hostChar` 是
    /// surrounding text 读到的光标前字符（0 = 读不到）：记账有数字就报记账；否则报宿主字符，
    /// 但 ASCII 数字抹成 0。非数字照报——服务端智能符号 press2 守卫拿它比对武装串末位。
    /// ⛔ 别让宿主字符「否决」记账：Qt / Electron 的 surrounding 是异步推送的，快打「3.」时
    /// 读到的可能还是插入 3 之前的内容，会误判成非数字后。
    uint16_t resolve(uint16_t hostChar) const
    {
        if (prevChar_ != 0) {
            return prevChar_;
        }
        return hostChar >= 0x30 && hostChar <= 0x39 ? 0 : hostChar;
    }
    void reset() { prevChar_ = 0; }
    /// 这一键产出的数字字符（0 = 不产出数字）。Ctrl/Alt/Win + 数字是宿主快捷键，不记。
    static uint16_t digitChar(uint32_t vk, uint32_t modifiers);
    /// 按键透传回宿主：产出数字则记，否则清零。
    void noteKeyPassthrough(uint32_t vk, uint32_t modifiers) { prevChar_ = digitChar(vk, modifiers); }
    /// 经引擎上屏的文本：末位是 ASCII 数字则记，否则清零；空串不动。
    void noteCommittedText(const std::string& utf8);

private:
    uint16_t prevChar_ = 0;
};

/// KeyPayload.seqChar 记账：输入序列里最近进文档的字符代表值（0 = 不可知）。对位 Windows
/// `wind_tsf/include/InputSequenceChar.h` 与 Swift `InputSequenceTracker`，取值规则三端一致：
/// 透传键报类别代表值（字母 'a'、数字本身、空格、其余可见符号 '?'），上屏文本取末尾码点的
/// 首个 UTF-16 单元。
/// 与 DigitTracker 分开：组合串（预编辑）没写进文档，不动它——否则「打 nihao → Esc」会留下
/// 编码末位 'o'，下一笔中文被误补空格。
///
/// **光标被挪过**（鼠标点到别处）看不见，靠两条补：宿主发 reset（GTK 点击会发）时调 `reset()`；
/// 宿主支持周边文本时（Qt 系点击只报光标位置、不发 reset），每次按键前用 `noteCursor` 比对预期光标——每次
/// 写进文档都把预期光标往前推，宿主报来的位置对不上、或光标前的字与记账对不上，就是被挪过。
/// 不支持周边文本的宿主两条都没有。
class SeqTracker {
public:
    /// 「在记账、但不知道」。0 只来自不带这个字段的旧客户端（Rust `SEQ_CHAR_UNKNOWN`）。
    static constexpr uint16_t kUnknown = 0xFFFF;
    uint16_t seqChar() const { return seqChar_; }
    void reset()
    {
        seqChar_ = kUnknown;
        expectedCursor_ = -1;
        beforeWrite_ = -1;
    }
    static uint16_t charFromVk(uint32_t vk, uint32_t modifiers);
    /// 可见字符透传：宿主写进一个字符，光标前进 1；其余键（导航、编辑、快捷键）光标去向不明。
    void noteKeyPassthrough(uint32_t vk, uint32_t modifiers)
    {
        seqChar_ = charFromVk(vk, modifiers);
        advanceCursor(seqChar_ == kUnknown ? -1 : 1, false);
    }
    /// 空串不动：没有东西写进文档。
    void noteCommittedText(const std::string& utf8);
    /// 光标跨过了一个已知是符号的字符（智能跳过右标点）。
    void noteCursorCrossedSymbol()
    {
        seqChar_ = '?';
        advanceCursor(1, false);
    }
    /// 写进文档的同时还删过字、挪过光标（智能符号替换、带光标位置的上屏）：预期光标作废，
    /// 下一次宿主报的位置重新当基准。记账本身不动。
    void forgetCursor() { expectedCursor_ = beforeWrite_ = -1; }
    /// 宿主报来光标位置（字符计）与光标前 / 后的字符（码点；0 = 那一侧没有字符，-1 = 给不出）。
    /// 位置与预期不符，或光标前的字符与记账对不上 = 被挪过：记账作废。`cursor < 0` = 宿主没给有效值。
    void noteCursor(int cursor, int32_t hostPrev = -1, int32_t hostNext = -1)
    {
        if (cursor < 0) {
            expectedCursor_ = beforeWrite_ = -1;
            return;
        }
        if (expectedCursor_ >= 0 && cursor != expectedCursor_) {
            // 上屏前那一刻的位置：宿主晚到的中间态回声（Qt 上屏时先报「编码已清、新字未插」），
            // 不是被挪过，也不当新基准。点回同一位置时光标后紧跟着刚上屏的字，据此区分。
            if (cursor == beforeWrite_ && (hostNext < 0 || hostNext != writtenFirst_)) {
                return;
            }
            seqChar_ = kUnknown;
        } else if (hostPrev >= 0 && !matchesHostChar(uint32_t(hostPrev))) {
            // 位置碰巧对得上（点到别的行 / 段的同一列）：光标前的字不是记账那个。
            seqChar_ = kUnknown;
        }
        expectedCursor_ = cursor;
        beforeWrite_ = -1;
    }

private:
    /// `chars < 0` = 光标去向不明。`commit` = 经输入法上屏（只有它有中间态回声）。
    void advanceCursor(int chars, bool commit)
    {
        beforeWrite_ = commit ? expectedCursor_ : -1;
        expectedCursor_ = (chars < 0 || expectedCursor_ < 0) ? -1 : expectedCursor_ + chars;
    }
    /// 宿主光标前的字符 `cp`（0 = 没有）与记账是否一致。透传键只记了类别代表值，按类别比。
    bool matchesHostChar(uint32_t cp) const;

    uint16_t seqChar_ = kUnknown;
    int expectedCursor_ = -1;
    int beforeWrite_ = -1;
    int32_t writtenFirst_ = -1; ///< 最近一次上屏文本的首个码点
};

/// 服务端给的删除 / 光标移动计数是 u32，转 int 取负前先钳住：大于 INT_MAX 时取负是未定义行为；
/// 真实值只有个位数（智能标点、撤销上屏的一段词），上限远高于正常用量。
constexpr uint32_t kMaxDeleteUnits = 1024;
constexpr uint32_t kMaxCursorMove = 64;

class ResponseRouter {
public:
    /// 计时器调度器：ms 毫秒后在**主线程**回调 cb。未注入则 hold 计时器不工作（待定标点
    /// 只在下一次按键/失焦时收口，功能不丢，只是下划线留得久一点）。
    using TimerScheduler = std::function<void(uint32_t ms, std::function<void()> cb)>;
    /// 计时器到点时借用「此刻的宿主」：调用方拿当前焦点造一个 sink 交给 `use`，焦点已不在
    /// 任何文本框时交 nullptr。借用而非返回指针：sink 是短命的适配器，生命周期归调用方。
    using SinkProvider = std::function<void(const std::function<void(TextSink*)>& use)>;

    void setTimerScheduler(TimerScheduler s) { scheduler_ = std::move(s); }
    void setSinkProvider(SinkProvider p) { sinkProvider_ = std::move(p); }

    /// 路由一帧。返回值 = 按键是否被输入法消费（false ⇒ 交还宿主）。
    ///
    /// `hostShortcut`：触发本帧的按键带 Ctrl/Alt/Super。此时 ClearComposition 只表示「把组合
    /// 清掉」，不表示这个键归输入法——宿主仍须收到它去执行复制/粘贴（对位 Swift 同名参数与
    /// macOS issue #64）。
    bool apply(const Frame& frame, TextSink* sink, bool hostShortcut = false);

    /// 结束组合（失焦 / 宿主要求 reset）。待定标点转为提交而非丢弃。
    void applyClearComposition(TextSink* sink);

    /// 焦点换了：本端状态全部作废（不碰宿主）。
    void reset();

    bool hasComposition() const { return !compositionText_.empty(); }
    const std::string& compositionText() const { return compositionText_; }
    DigitTracker& digitTracker() { return digits_; }
    SeqTracker& seqTracker() { return seq_; }
    /// 按键透传回宿主：两份记账一起喂（漏喂一份的症状是那一份的功能在某条透传路上失灵）。
    void noteKeyPassthrough(uint32_t vk, uint32_t modifiers)
    {
        digits_.noteKeyPassthrough(vk, modifiers);
        seq_.noteKeyPassthrough(vk, modifiers);
    }

private:
    void applyCommitText(const CommitTextPayload& p, TextSink* sink);
    void applyUpdateComposition(const UpdateCompositionPayload& p, TextSink* sink);
    void applyCommitTextWithCursor(const CommitTextWithCursorPayload& p, TextSink* sink);
    void insertCommitted(const std::string& text, TextSink* sink);
    void applyMarkedText(const std::string& text, size_t caretUtf16, TextSink* sink);
    void setCompositionRaw(const std::string& text, TextSink* sink);
    void flushPendingPrefix(TextSink* sink);
    void clearPreeditIfAny(TextSink* sink);
    void absorbHeldIntoPrefix();
    std::string takePendingPrefix();
    void cancelHoldTimer() { ++holdGeneration_; }
    void armHoldTimer(uint32_t ms);

    std::string compositionText_;
    bool hasHeld_ = false;
    std::string heldSymbol_;          // 智能符号 HoldComposition 挂着的待定标点
    std::string pendingCommitPrefix_; // 已定格、尚未真上屏的前缀
    uint64_t holdGeneration_ = 0;
    DigitTracker digits_;
    SeqTracker seq_;
    TimerScheduler scheduler_;
    SinkProvider sinkProvider_;
};

} // namespace windlinux
