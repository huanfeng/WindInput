// SkipKeyTable 单元测试
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -I../include -o skip_key_table_test skip_key_table_test.cpp && ./skip_key_table_test
// 或经 CMake：
//   cmake -B build -DWIND_TSF_TESTS=ON && cmake --build build --target skip_key_table_test
//
// 变异检验已做：把 SkipKeyTable 换回「裸 WORD + 只比队首 + 无通道 + 无 TTL」的旧实现，
// 除 TestBatchInjection 外全部变红（20 个断言），标着 ← 旧实现在此变红 的即是。
// 改测试时请保住这个性质——一条永远绿的测试不会告诉你任何事。
// TestBatchInjection 为何不在其列，见它自己的注释：那条缺陷在接线不在表。

#include "SkipKeyTable.h"

#include <cstdio>

namespace
{

// 用真实 VK 值，方便与日志、KeyEventSink 里的分支对照。
constexpr uint16_t VK_BACK_ = 0x08;
constexpr uint16_t VK_LEFT_ = 0x25;
constexpr uint16_t VK_HOME_ = 0x24;
constexpr uint16_t VK_END_ = 0x23;
constexpr uint16_t VK_PACKET_ = 0xE7;

int g_failures = 0;
const char* g_case = "";

#define CHECK(expr)                                                                      \
    do                                                                                   \
    {                                                                                    \
        if (!(expr))                                                                     \
        {                                                                                \
            std::printf("  FAIL  %s:%d  [%s]  %s\n", __FILE__, __LINE__, g_case, #expr); \
            g_failures++;                                                                \
        }                                                                                \
    } while (0)

#define CASE(name)                     \
    g_case = name;                     \
    std::printf("- %s\n", name);

// ── 形态①：连按同一个键 ────────────────────────────────────────────────────
//
// 联想窗弹出时连按 ← ←：每一下都被 OnTestKeyDown 吃掉、由 _ReplayKeyToHost 重放，
// 于是表里可能同时排着两条 VK_LEFT。此前条目不带通道、down 与 up 共用一张表 ⇒
// 第一次注入的 keyup 会把**给第二个 down 的那条**吃掉，第二个 ← 于是漏进 IME 逻辑
// （不补发 WM_KEYDOWN 的宿主直接丢键）。
void TestSameKeyTwice()
{
    CASE("形态①：同 vk 两条，注入的 keyup 不得吃掉给下一个 down 的条目");
    SkipKeyTable t;
    t.Push(VK_LEFT_, /*forKeyUp=*/false, 0);
    t.Push(VK_LEFT_, /*forKeyUp=*/false, 0);

    CHECK(t.TryConsume(VK_LEFT_, false, 1));  // 第一个 down 放行
    CHECK(!t.TryConsume(VK_LEFT_, true, 1));  // 注入的 keyup 不该命中 ← 旧实现在此变红
    CHECK(t.TryConsume(VK_LEFT_, false, 2));  // 第二个 down 仍放行   ← 旧实现在此变红
    CHECK(t.Count() == 0);
}

// ── 形态②：批量注入 N 个键 = 2N 个事件 ────────────────────────────────────
//
// CommitText / ReplacePrecedingChars 的 SendInput 兜底路径（终端模拟器、微信、部分
// 纯文本编辑器）每个键注入 down + up 两个事件。此前 MarkSyntheticKey 每键只压一条，
// 于是 2N 个事件被 down/up 交替消费掉前 N 个，**后一半照样进 IME 逻辑**——正是那条
// 路径的注释开头声明要防的「替换后的符号重复上屏」。N==1 时恰好不出问题，所以它
// 藏了很久。
//
// ⚠️ **本条没有变异保护，已实测**：那个缺陷在调用方（MarkSyntheticKey 压几条），
// 不在表里，对旧实现照样全绿。它钉的是表这一侧的前提——「成对条目能被成对消费、
// 且 down 与 up 互不干扰」；「每键压两条」属接线，由 KeyEventSink.h 的 skip 表段落
// 与真机负责。留着它是因为前提一旦被破坏（例如有人把通道判据改成单向匹配），
// MarkSyntheticKey 会跟着坏，而那时这条会红。
void TestBatchInjection()
{
    CASE("形态②：批量注入的 down 与 up 全部放行");
    SkipKeyTable t;
    constexpr int kKeys = 3;
    for (int i = 0; i < kKeys; i++)
    {
        // MarkSyntheticKey 的语义：键是我们凭空造的 ⇒ 两条都压。
        t.Push(VK_BACK_, false, 0);
        t.Push(VK_BACK_, true, 0);
    }
    CHECK(t.Count() == kKeys * 2);

    for (int i = 0; i < kKeys; i++)
    {
        CHECK(t.TryConsume(VK_BACK_, false, 1));
        CHECK(t.TryConsume(VK_BACK_, true, 1));
    }
    CHECK(t.Count() == 0);

    // Unicode 注入走 KEYEVENTF_UNICODE，到达时 wParam 是 VK_PACKET，同样成对。
    t.Push(VK_PACKET_, false, 10);
    t.Push(VK_PACKET_, true, 10);
    CHECK(t.TryConsume(VK_PACKET_, false, 11));
    CHECK(t.TryConsume(VK_PACKET_, true, 11));
}

// ── 形态③：残条不得挡死其后条目 ────────────────────────────────────────────
//
// 注入没能回到本 sink（前台窗口已换、宿主此刻不走 TSF、SendInput 被 UIPI 拦）时队首
// 会留下一条永远等不到的条目。此前只比对队首 ⇒ 它挡死其后**全部**条目，直到
// ResetComposingState 整表清零（换焦点/失焦/中英切换）——期间自动配对合成的左移键
// 首当其冲，表现为「配对跳出时灵时不灵」。
void TestStaleEntryDoesNotBlock()
{
    CASE("形态③：队首残条不挡后续条目");
    SkipKeyTable t;
    t.Push(VK_HOME_, false, 0);  // 这条永远不会回来
    t.Push(VK_LEFT_, false, 0);

    CHECK(t.TryConsume(VK_LEFT_, false, 1));  // ← 旧实现在此变红（队首是 HOME）
    CHECK(t.Count() == 1);                    // HOME 还在，未过期
    CHECK(t.ExpiredTotal() == 0);
}

// ── TTL：残条自愈，且是唯一的可观测信号 ────────────────────────────────────
void TestTtlExpiry()
{
    CASE("TTL：超时残条被清掉并计入诊断探针");
    SkipKeyTable t;
    t.Push(VK_HOME_, false, 0);
    t.Push(VK_LEFT_, false, 0);

    // 边界：恰好 kTtlMs 仍在有效期内（<= 而非 <）。
    CHECK(t.TryConsume(VK_LEFT_, false, SkipKeyTable::kTtlMs));
    CHECK(t.Count() == 1);
    CHECK(t.ExpiredTotal() == 0);

    // 越过 TTL：下一次操作把它清掉。
    t.Push(VK_END_, false, SkipKeyTable::kTtlMs + 1);
    CHECK(t.ExpiredTotal() == 1);
    CHECK(t.Count() == 1);  // 只剩刚压的 END

    // 过期条目可被取走打日志（明细取完即清，累计数不清）。
    SkipKeyTable::Entry drained[SkipKeyTable::kExpiredLogCap];
    CHECK(t.DrainExpired(drained, SkipKeyTable::kExpiredLogCap) == 1);
    CHECK(drained[0].vk == VK_HOME_);
    CHECK(t.DrainExpired(drained, SkipKeyTable::kExpiredLogCap) == 0);
    CHECK(t.ExpiredTotal() == 1);

    // 过期的条目不该还能被消费。
    CHECK(!t.TryConsume(VK_HOME_, false, SkipKeyTable::kTtlMs + 2));
}

// ── 通道是双向的：给 up 的条目不该被 down 消费 ─────────────────────────────
void TestChannelIsolation()
{
    CASE("通道隔离：down 与 up 的条目互不消费");
    SkipKeyTable t;
    t.Push(VK_BACK_, true, 0);
    CHECK(!t.TryConsume(VK_BACK_, false, 1));  // down 不该吃掉给 up 的条目
    CHECK(t.TryConsume(VK_BACK_, true, 1));

    t.Push(VK_BACK_, false, 2);
    CHECK(!t.TryConsume(VK_BACK_, true, 3));  // 反向同理
    CHECK(t.TryConsume(VK_BACK_, false, 3));
}

// ── 表满：丢弃必须可见 ────────────────────────────────────────────────────
void TestOverflowIsVisible()
{
    CASE("表满：丢弃计入 DroppedTotal，不静默");
    SkipKeyTable t;
    for (int i = 0; i < SkipKeyTable::kMaxKeys; i++)
        t.Push(VK_PACKET_, false, 0);
    CHECK(t.Count() == SkipKeyTable::kMaxKeys);
    CHECK(t.DroppedTotal() == 0);

    t.Push(VK_PACKET_, false, 0);
    CHECK(t.DroppedTotal() == 1);
    CHECK(t.Count() == SkipKeyTable::kMaxKeys);

    // 容量必须容得下批量注入路径的一次典型调用（每字符两条）。
    CHECK(SkipKeyTable::kMaxKeys >= 64 * 2);
}

// ── Clear：整表清零但保留累计诊断 ──────────────────────────────────────────
void TestClearKeepsDiagnostics()
{
    CASE("Clear：清表但不清累计计数");
    SkipKeyTable t;
    t.Push(VK_LEFT_, false, 0);
    t.Push(VK_HOME_, false, 0);
    t.Purge(SkipKeyTable::kTtlMs + 1);
    CHECK(t.Count() == 0);
    CHECK(t.ExpiredTotal() == 2);

    t.Push(VK_END_, false, 1000);
    t.Clear();
    CHECK(t.Count() == 0);
    // 累计计数是跨会话的诊断信号，被 ResetComposingState 清掉就看不出这台机器上
    // 到底发生过没有。
    CHECK(t.ExpiredTotal() == 2);
}

}  // namespace

int main()
{
    std::printf("SkipKeyTable tests\n");
    TestSameKeyTwice();
    TestBatchInjection();
    TestStaleEntryDoesNotBlock();
    TestTtlExpiry();
    TestChannelIsolation();
    TestOverflowIsVisible();
    TestClearKeepsDiagnostics();

    if (g_failures == 0)
    {
        std::printf("OK\n");
        return 0;
    }
    std::printf("%d assertion(s) FAILED\n", g_failures);
    return 1;
}
