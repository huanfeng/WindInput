// 命令直通车按键合成（key.tap / key.seq / key.hold / key.release）的纯逻辑，不依赖 Fcitx5 头文件。
//
// 服务端在 ext_presenter 形态下不自己合成按键，把组合编成下行帧 CMD_KEY_TAP/SEQ/HOLD/RELEASE
// 推给宿主（`handle_cmdbar_macos.rs::CoordKeys`，macOS 由 `.app` 的 KeySynthesizer 发 CGEvent）。
// 本 addon 经 Fcitx5 `InputContext::forwardKey` 把键交给当前输入上下文的应用——不进输入法
// 引擎、不回流（取舍见 wind_linux/AGENTS.md「命令直通车按键合成」）。
//
// 这里只做三件事：键名 → X keysym 与修饰位、组合 → 有序的按下/抬起事件列、按住键的记账与
// 限流。落到哪个 IC、什么时候补发抬起由 WindEngine 决定。
#pragma once

#include "Codec.h"

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace windlinux {

/// 一个要合成的键事件。`states` 按 X11 语义是事件之前的修饰态（同真实键盘：按下 Ctrl
/// 那一下 state 不含 Ctrl，抬起时含），位值见 KeyMap.h 的 xstate。
struct SynthKey {
    uint32_t sym = 0;
    uint32_t states = 0;
    bool release = false;
    bool operator==(const SynthKey& o) const
    {
        return sym == o.sym && states == o.states && release == o.release;
    }
};

/// 解析后的组合：修饰键 keysym（按给出顺序、去重，即按下顺序）+ 主键 keysym。
struct ResolvedCombo {
    std::vector<uint32_t> mods;
    uint32_t key = 0;
    bool operator==(const ResolvedCombo& o) const { return mods == o.mods && key == o.key; }
};

/// 主键名 → keysym；不认识返回 0。名字集合 = Windows `key_inject::parse_key`（含 `return` /
/// `esc` / `bksp` / `del` / `ins` / `pgup` / `pgdn` 等别名与符号键的字符写法）∪ macOS
/// `KeySynthesizer.keyCodeMap`（`capslock`、修饰键本身作主键）∪ `f1`…`f24`；`vk:NN` 按
/// Windows VK（十六进制，`0x` 可省，同 Windows）反查。字母、符号取无 Shift 的基础层
/// keysym（`c` 而非 `C`），Shift 放进 state——各前端按 keysym 反查键码、客户端按 state 解释，
/// 与 `xdotool key ctrl+shift+c` 同法。大小写不敏感。
uint32_t keyNameToKeysym(const std::string& name);

/// 修饰名 → 左侧修饰键 keysym（ctrl→Control_L、shift→Shift_L、alt→Alt_L、win→Super_L，
/// 另收服务端 `split_combo` 认的别名）；不认识返回 0。
uint32_t modifierNameToKeysym(const std::string& name);

/// 修饰键 keysym → 它贡献的 state 位；不是修饰键返回 0。
uint32_t modifierStateBit(uint32_t keysym);

/// 组合 → 可合成的形式。主键或任一修饰名不认识即整条拒绝（返回空，调用方 WARN 并丢弃）：
/// 同 Windows `parse_combo`——`Hyper+C` 不能退化成一个裸 `c`。
std::optional<ResolvedCombo> resolveCombo(const KeyComboPayload& combo);

/// 按下：修饰键依次按下，再按主键。`base` 是此前已按住的修饰态（key.hold 未放的键），
/// 叠进每个事件的 state——按住 Shift 时 tap End 就是 Shift+End。
std::vector<SynthKey> pressEvents(const ResolvedCombo& c, uint32_t base = 0);
/// 抬起：先抬主键，修饰键逆序抬起（同 Windows `SysKeys::release` / macOS）。
std::vector<SynthKey> releaseEvents(const ResolvedCombo& c, uint32_t base = 0);
/// 单击 = 按下 + 抬起。
std::vector<SynthKey> tapEvents(const ResolvedCombo& c, uint32_t base = 0);

// ── 限流 ──────────────────────────────────────────────────────────────

/// 单次 key.seq 最多几个组合；超过整条拒绝（不截断：截一半的「删行」比不删更糟）。
constexpr size_t kMaxSeqCombos = 64;
/// 滑动窗口限流：每 kSynthWindowMs 内 tap / seq / hold 合成的键事件（按下、抬起各算一个）
/// 至多 kMaxSynthEventsPerWindow 个；超出的帧整条丢弃。补发的抬起不受限——卡键比多一个
/// 抬起危险得多。一条满长的 seq（64 组合 × 最多 4 修饰 → 640 事件）也放得下。
constexpr uint64_t kSynthWindowMs = 1000;
constexpr size_t kMaxSynthEventsPerWindow = 1024;

class SynthRateLimiter {
public:
    /// 这一批 `events` 个事件放不放行；放行即计入窗口。
    bool allow(size_t events, uint64_t nowMs);

private:
    struct Batch {
        uint64_t atMs;
        size_t events;
    };
    std::vector<Batch> batches_;
    size_t inWindow_ = 0;
};

// ── key.hold 的记账（卡键保护）───────────────────────────────────────

/// key.hold 按下不放的最长时间（毫秒），到点本端自动补发抬起。默认 10 秒；环境变量
/// `WIND_KEY_HOLD_TIMEOUT_MS`（正整数）可覆盖——给 e2e 验这条兜底用，不是用户配置。
constexpr uint32_t kDefaultKeyHoldTimeoutMs = 10000;
uint32_t keyHoldTimeoutMs(const char* env);

/// 同时按住的组合上限：再 hold 新的就拒绝（一般的用法是一两个修饰键）。
constexpr size_t kMaxHeldCombos = 8;

class KeyHoldTracker {
public:
    enum class HoldResult { Pressed, AlreadyHeld, Full };

    /// 登记按住并给出按下事件（`out`）。已按住同一组合不重复按；满了拒绝。
    HoldResult hold(const ResolvedCombo& c, uint64_t nowMs, uint32_t timeoutMs,
                    std::vector<SynthKey>& out);
    /// 抬起一个按住的组合；没按住返回空（不凭空造一个抬起）。
    std::vector<SynthKey> release(const ResolvedCombo& c);
    /// 全部抬起（按住的逆序）：失焦 / 换 IC / reset / 服务重启或断线 / 析构。
    std::vector<SynthKey> releaseAll();
    /// 抬起已到最长保持时间的。
    std::vector<SynthKey> releaseExpired(uint64_t nowMs);
    /// 最早的到期时刻；没有按住的返回空。
    std::optional<uint64_t> nextDeadline() const;
    /// 按住的修饰键贡献的 state（tap / seq 的 base）。
    uint32_t heldStates() const;
    bool empty() const { return held_.empty(); }
    size_t size() const { return held_.size(); }

private:
    struct Held {
        ResolvedCombo combo;
        uint64_t deadlineMs;
    };
    /// 抬起第 i 个（先从表里摘掉，base 取其余仍按住的）。
    std::vector<SynthKey> releaseAt(size_t i);
    std::vector<Held> held_;
};

} // namespace windlinux
