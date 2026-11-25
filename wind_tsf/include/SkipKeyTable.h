#pragma once

#include <cstdint>

// ============================================================================
// SkipKeyTable —— 我们自己经 SendInput 注入的按键登记表
// ============================================================================
//
// 注入前压入，`OnTestKeyDown` / `OnTestKeyUp` 见到匹配条目即 `pfEaten = FALSE` 放行，不让注入的
// 退格/字符被当成用户按键再走一遍输入流程（文字重复上屏、配对跳出失效）。
//
// 压入者分两类，靠 `forKeyUp` 显式区分：
//   - 键是我们凭空造的：down 与 up 都绕过 IME，压两条。`CKeyEventSink::MarkSyntheticKey`
//     （CommitText / InsertText / ReplacePrecedingChars 的 SendInput 兜底）。
//   - 键是用户真按过、只是延后交付：只压 down，注入的 keyup 走正常路径。`_ReplayKeyToHost`、
//     `_SimulatePairKey` 与修饰键释放后的 defer 执行；`OnTestKeyUp` 的 direct_commit 顶码分支
//     指望那个 up 经过。
//
// 按值扫整表、按方向区分、带 TTL，防的三种故障（同 vk 两条时 keyup 吃掉给 down 的条目；批量
// 注入压 N 条却要覆盖 2N 个事件；队首残条挡死后续条目）各由 `tests/skip_key_table_test.cpp`
// 的用例钉住。
//
// 本类只管表自身语义，管不了接线：哪个消费点传 `forKeyUp = true`、压一条还是两条，接反了测试
// 照样全绿。接线共 6 处，列在 `KeyEventSink.h` 的 skip 表段落。
// 不含 Win32 头：时钟由调用方以 `GetTickCount64()` 传入，以便测试在非 Windows 工具链下编译。
class SkipKeyTable
{
public:
    // 容量：批量注入路径每字符占两条，128 够一次注入 64 个字符；`_SimulatePairKey`
    // 另按 moveLeft 循环压入。
    static constexpr int kMaxKeys = 128;

    // TTL 是保险：注入的键几 ms 内就回来，500ms 极宽松，只为让残条自愈。
    static constexpr uint64_t kTtlMs = 500;

    struct Entry
    {
        uint16_t vk = 0;
        bool forKeyUp = false;
        uint64_t tick = 0;
    };

    // 登记一个即将注入的按键。表满时丢弃并计入 `DroppedTotal()`，调用方必须打日志：
    // 被丢的注入键会被当成用户按键走完整 IME 流程。
    void Push(uint16_t vk, bool forKeyUp, uint64_t nowMs)
    {
        Purge(nowMs);
        if (_count >= kMaxKeys)
        {
            _droppedTotal++;
            return;
        }
        _keys[_count].vk = vk;
        _keys[_count].forKeyUp = forKeyUp;
        _keys[_count].tick = nowMs;
        _count++;
    }

    // 这个到达的按键是不是我们自己注入的？是则消费掉对应条目并返回 true。
    bool TryConsume(uint16_t vk, bool forKeyUp, uint64_t nowMs)
    {
        Purge(nowMs);
        // 按值扫整表而非只比队首：残条会挡死其后全部条目。表通常 0~2 条。
        for (int i = 0; i < _count; i++)
        {
            if (_keys[i].vk != vk || _keys[i].forKeyUp != forKeyUp)
                continue;
            for (int j = i + 1; j < _count; j++)
                _keys[j - 1] = _keys[j];
            _count--;
            return true;
        }
        return false;
    }

    // 清掉超时未被消费的条目。表只在 Push / TryConsume 里变化，两处各调一次即可，无需定时器。
    void Purge(uint64_t nowMs)
    {
        if (_count <= 0)
            return;
        int kept = 0;
        for (int i = 0; i < _count; i++)
        {
            if (nowMs - _keys[i].tick <= kTtlMs)
            {
                _keys[kept++] = _keys[i];
                continue;
            }
            _expiredTotal++;
            if (_expiredLogCount < kExpiredLogCap)
                _expiredLog[_expiredLogCount++] = _keys[i];
        }
        _count = kept;
    }

    // 组合状态复位（换焦点/失焦/中英切换）时整表清零。
    // 累计计数不清：它们是跨会话的诊断信号。
    void Clear() { _count = 0; _expiredLogCount = 0; }

    int Count() const { return _count; }

    // ── 诊断探针 ──────────────────────────────────────────────────────────
    // 过期条目累计数。>0 即证明注入键丢失（残条）在这台机器上发生过。
    int ExpiredTotal() const { return _expiredTotal; }
    // 因表满被丢弃的累计数。
    int DroppedTotal() const { return _droppedTotal; }

    // 取走待打日志的过期条目（取完即清）。返回写入 out 的条数。
    // 攒不下的部分由 `ExpiredTotal()` 兜住。
    int DrainExpired(Entry* out, int cap)
    {
        int n = _expiredLogCount < cap ? _expiredLogCount : cap;
        for (int i = 0; i < n; i++)
            out[i] = _expiredLog[i];
        _expiredLogCount = 0;
        return n;
    }

    static constexpr int kExpiredLogCap = 8;

private:
    Entry _keys[kMaxKeys] = {};
    int _count = 0;

    Entry _expiredLog[kExpiredLogCap] = {};
    int _expiredLogCount = 0;
    int _expiredTotal = 0;
    int _droppedTotal = 0;
};
