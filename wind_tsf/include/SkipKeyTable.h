#pragma once

#include <cstdint>

// ============================================================================
// SkipKeyTable —— 我们自己经 SendInput 注入的按键登记表
// ============================================================================
//
// 注入前压入，`OnTestKeyDown` / `OnTestKeyUp` 见到匹配条目即 `pfEaten = FALSE`
// 直接放行，不让它进 IME 自己的按键逻辑（否则注入的退格/字符会被当成用户真按的
// 键重新走一遍输入流程，表现为文字重复上屏、配对跳出失效）。
//
// ── 压入者分两类，语义不同，靠 `forKeyUp` 显式区分 ──────────────────────────
//
//   - **键是我们凭空造的** ⇒ down 与 up 都该绕过 IME，压两条。
//     `CKeyEventSink::MarkSyntheticKey`（CommitText / InsertText /
//     ReplacePrecedingChars 的 SendInput 兜底，一次可注入一整段文本、每字符一个键）。
//   - **键是用户真按过、我们只是延后交付** ⇒ 只压 down，注入的 keyup 走正常路径。
//     `_ReplayKeyToHost`；`_SimulatePairKey` 与修饰键释放后的 defer 执行同样只压
//     down —— 那是已验证的既有行为，`OnTestKeyUp` 顶部的 direct_commit 顶码分支
//     指望那个 up 经过。
//
// ── 为什么单独成类 ────────────────────────────────────────────────────────
//
// 此前条目是裸 `WORD` 数组、直接长在 `CKeyEventSink` 里，两类语义靠「同一时刻表里
// 只有我这一条」的**隐含约定**共存，而 down / up 两个消费点又共用它。约定一破就出
// 三种故障，都表现为「注入键被当成用户真按的键走完整 IME 流程」：
//
//   ① 表里排着两条同 vk（连按 ← ←，或前一条注入还没回来）⇒ 注入的 keyup 把**给下
//      一个 down 的那条**吃掉，那个 down 于是漏进 IME 逻辑。
//   ② 批量注入压 N 条却要覆盖 2N 个事件 ⇒ 后一半没得放行。
//   ③ 队首残条挡死其后**全部**条目（注入没能回到本 sink：前台窗口已换、宿主此刻不
//      走 TSF、SendInput 被 UIPI 拦）。
//
// 三者在真机上都难以稳定复现——② 只在批量长度 >1 时发生，③ 要碰运气制造注入丢失，
// 且残条会被 `ResetComposingState`（换焦点/失焦/中英切换）整表清掉 ⇒ 切个窗就自愈，
// 报障形态永远是「时灵时不灵」。搬到这里、把时钟参数化之后，三者各是一条几行的
// 单元测试（`tests/skip_key_table_test.cpp`），且对改造前的实现全红。
//
// ⚠️ **本类只管表自身的语义，管不了接线**：哪个消费点传 `forKeyUp = true`、哪个
// 压入者压一条还是两条，接反了这里的测试照样全绿。接线共 6 处，列在
// `KeyEventSink.h` 的 skip 表段落，只能靠人工核对 + 真机。
//
// ⚠️ 不含任何 Win32 头：时钟由调用方以 `GetTickCount64()` 传入。这既是可测性的
// 前提，也让本文件能被测试在非 Windows 工具链下直接编译。
class SkipKeyTable
{
public:
    // 容量：批量注入路径每字符占两条，128 够一次注入 64 个字符；`_SimulatePairKey`
    // 另按 moveLeft 循环压入。
    static constexpr int kMaxKeys = 128;

    // TTL 是保险不是主体：注入的键在同线程消息队列里几 ms 内就回来，500ms 极宽松。
    // 它的职责只是让 ③ 那类残条自愈，而不是替代 forKeyUp 对 ①② 的根治。
    static constexpr uint64_t kTtlMs = 500;

    struct Entry
    {
        uint16_t vk = 0;
        bool forKeyUp = false;
        uint64_t tick = 0;
    };

    // 登记一个即将注入的按键。表满时丢弃并计入 `DroppedTotal()`——调用方**必须**
    // 把它打成日志：静默丢弃意味着那个注入键会被当成用户真按的键走完整 IME 流程。
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
        // **按值扫整表**，不只比队首：一条对不上的残条会把它后面的全部条目挡死，
        // 而残条恰恰是最常见的失效形态（形态③）。表通常 0~2 条，线性扫可忽略。
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

    // 清掉超时未被消费的条目。Push / TryConsume 内部各调一次即可覆盖全部时机——
    // 这张表只在这两个动作里变化，不需要定时器。
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
    // 累计计数**不清**：它们是跨会话的诊断信号，清掉就看不出「这台机器上到底发生
    // 过没有」。
    void Clear() { _count = 0; _expiredLogCount = 0; }

    int Count() const { return _count; }

    // ── 诊断探针 ──────────────────────────────────────────────────────────
    // 过期条目累计数。>0 即证明形态③ 在这台机器上真实发生过。
    int ExpiredTotal() const { return _expiredTotal; }
    // 因表满被丢弃的累计数。
    int DroppedTotal() const { return _droppedTotal; }

    // 取走待打日志的过期条目（取完即清）。返回写入 out 的条数。
    // 攒不下的部分由 `ExpiredTotal()` 兜住——真正要紧的信息是「发生了」，vk 明细
    // 有几条就够定位。
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
