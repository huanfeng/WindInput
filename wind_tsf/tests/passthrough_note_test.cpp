// PassthroughNote 单元测试
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -I../include -o passthrough_note_test passthrough_note_test.cpp && ./passthrough_note_test
// 或经 CMake：
//   cmake -B build -DWIND_TSF_TESTS=ON && cmake --build build --target passthrough_note_test
//
// 守的是智能符号「透传上报」的记账语义。这条通路的失效方向是**不对称**的：
//   漏报 ⇒ 服务端误判 press2，删掉用户刚打的字（不可接受）；
//   多报 ⇒ 用户重按一次 press1（可接受）——**除非触发源是我们自己的注入机制**，那时多报
//          会变成每次 press1 都自解武装，即整条功能失效。
// 下面每条用例都对着这两句里的一句。
//
// ⚠️ 测不到的部分（同 SkipKeyTable 的局限）：守卫挂在哪几个函数、`Suppress()` 接在哪个
// 分支。那是接线，只能靠 KeyEventSink.cpp 的注释与真机验证。

#include "PassthroughNote.h"

#include <cstdio>
#include <initializer_list>

namespace
{

using WindPassthrough::PassthroughState;

// 用真实 VK 值，方便与日志、KeyEventSink 里的分支对照。
constexpr uint32_t VK_A_ = 0x41;
constexpr uint32_t VK_1_ = 0x31;
constexpr uint32_t VK_OEM_PERIOD_ = 0xBE;
constexpr uint32_t VK_SHIFT_ = 0x10;
constexpr uint32_t VK_LWIN_ = 0x5B;
constexpr uint32_t VK_CAPITAL_ = 0x14;
constexpr uint32_t VK_PACKET_ = 0xE7;
constexpr uint32_t VK_ASYNC_COMMIT_ = 0xE8;
constexpr uint32_t VK_RIGHT_ = 0x27;

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

#define CASE(name)            \
    g_case = name;            \
    std::printf("CASE %s\n", name);

// 透传一个普通字母（没吃下、不是自生成、不是修饰键）。
void Passthrough(PassthroughState& st, uint32_t vk)
{
    st.NoteKeyDown(vk, /*eaten=*/false, /*suppressed=*/false);
}

void TestEatenKeyIsNotNoted()
{
    CASE("吃下的键不记账");
    PassthroughState st;
    st.NoteKeyDown(VK_OEM_PERIOD_, /*eaten=*/true, /*suppressed=*/false);
    // press1 自己就是被吃下的那个键。它若记账，press2 一到就会被自己解除 ⇒ 功能全废。
    CHECK(!st.Pending());  // ← 把 `eaten` 判据删掉即在此变红
}

void TestPassthroughKeyIsNoted()
{
    CASE("透传的普通键要记账");
    PassthroughState st;
    Passthrough(st, VK_A_);
    CHECK(st.Pending());  // ← 整个记账被删掉即在此变红
    PassthroughState st2;
    Passthrough(st2, VK_1_);
    CHECK(st2.Pending());
}

void TestBareModifiersAreNotNoted()
{
    CASE("纯修饰键不记账");
    // 「按住 Shift 连按两次 ？」是正常 press2。中间那个 Shift 若记账就把它解除了。
    for (uint32_t vk : {VK_SHIFT_, uint32_t(0x11), uint32_t(0x12), VK_LWIN_, uint32_t(0xA0)})
    {
        PassthroughState st;
        Passthrough(st, vk);
        CHECK(!st.Pending());  // ← 修饰键排除被删即在此变红
    }
}

void TestCapsLockIsNoted()
{
    CASE("CapsLock 要记账（它换掉了标点产物列）");
    // 刻意不在排除项里：CapsLock 一开，服务端就按英文列算标点产物，press2 再按旧方向
    // 替换本就是错的，解除武装反而对。若哪天有人「顺手」把它并进修饰键集合，这条会红。
    PassthroughState st;
    Passthrough(st, VK_CAPITAL_);
    CHECK(st.Pending());
}

void TestSuppressedIsNotNoted()
{
    CASE("skip 表命中（自生成键）不记账");
    PassthroughState st;
    // 例：ReplacePrecedingChars 兜底注入的退格，经 skip 表放行 ⇒ suppressed。
    st.NoteKeyDown(uint32_t(0x08), /*eaten=*/false, /*suppressed=*/true);
    CHECK(!st.Pending());  // ← `suppressed` 判据被删即在此变红
}

void TestSelfInjectedVkIsNotNotedEvenWithoutSuppress()
{
    CASE("自生成 VK 即使没走 suppress 也不记账");
    // 这条锁的是 Chrome/QQ 那批宿主：它们无视 pfEaten=FALSE 仍调 OnKeyDown，届时 skip
    // 条目已被 OnTestKeyDown 消费掉、`suppressed` 传不进来，只剩 VK 值这道兜底。
    // 漏了它 ⇒ CommitText 走 SendInput 兜底的宿主上 press1 每次上屏都自解武装 ⇒ 整条失效。
    for (uint32_t vk : {VK_PACKET_, VK_ASYNC_COMMIT_})
    {
        PassthroughState st;
        st.NoteKeyDown(vk, /*eaten=*/false, /*suppressed=*/false);
        CHECK(!st.Pending());  // ← IsSelfInjectedVk 兜底被删即在此变红
    }
}

void TestRealKeysStayNotedEvenIfWeAlsoInjectThem()
{
    CASE("物理键不因「我们也会注入它」而被排除");
    // VK_RIGHT 会被 _SimulatePairKey 注入，但它同时是用户真按得到的键。把它并进
    // IsSelfInjectedVk（或做成「记住刚注入过的 vk」的备忘）就会在残留窗口里误抑制真实
    // 按键 —— 那是**漏报**方向。这条守住这个取舍。
    PassthroughState st;
    Passthrough(st, VK_RIGHT_);
    CHECK(st.Pending());
}

void TestTakeConsumesExactlyOnce()
{
    CASE("keydown 发送时恰好消费一次");
    PassthroughState st;
    Passthrough(st, VK_A_);
    CHECK(st.TakeOnKeyDownSend());
    CHECK(!st.Pending());
    // 第二次没有新的透传，不得再报——否则一次透传会连累后面每一按。
    CHECK(!st.TakeOnKeyDownSend());  // ← Take 里漏了清零即在此变红
}

void TestKeyUpMustNotConsume()
{
    CASE("keyup 不消费（事实归下一个 keydown）");
    // toggle 键（Shift/Ctrl/CapsLock）的 keyup 也会走 _SendKeyToService。本状态机没有
    // keyup 入口，调用方也不得在 keyup 调 Take——这里用「不调」来表达：事实必须活到
    // 下一个 keydown。症状若反：中间按过 Shift 就又能误删一次。
    PassthroughState st;
    Passthrough(st, VK_A_);
    // ……keyup 发送发生在此处，什么都不做……
    CHECK(st.Pending());
    CHECK(st.TakeOnKeyDownSend());
}

void TestRestoreOnSendFailure()
{
    CASE("发送失败要把事实放回去");
    PassthroughState st;
    Passthrough(st, VK_A_);
    const bool noted = st.TakeOnKeyDownSend();
    CHECK(noted);
    CHECK(!st.Pending());
    // SendKeyEvent 返回 FALSE（IPC 断开 / 服务端重启）：这一发没送达。
    st.RestoreOnSendFailure();
    CHECK(st.Pending());  // ← 少了恢复即在此变红（漏报方向：下一按会被误判 press2）
    CHECK(st.TakeOnKeyDownSend());
}

void TestResetClears()
{
    CASE("焦点/文档切换清空");
    PassthroughState st;
    Passthrough(st, VK_A_);
    st.Reset();
    CHECK(!st.Pending());
}

void TestNotedStateIsSticky()
{
    CASE("多次透传只是同一个事实，不累积也不互相抵消");
    PassthroughState st;
    Passthrough(st, VK_A_);
    Passthrough(st, VK_A_);
    st.NoteKeyDown(VK_SHIFT_, /*eaten=*/false, /*suppressed=*/false);  // 修饰键不清已有事实
    st.NoteKeyDown(VK_OEM_PERIOD_, /*eaten=*/true, /*suppressed=*/false);  // 吃下的键也不清
    CHECK(st.Pending());
    CHECK(st.TakeOnKeyDownSend());
    CHECK(!st.Pending());
}

} // namespace

int main()
{
    TestEatenKeyIsNotNoted();
    TestPassthroughKeyIsNoted();
    TestBareModifiersAreNotNoted();
    TestCapsLockIsNoted();
    TestSuppressedIsNotNoted();
    TestSelfInjectedVkIsNotNotedEvenWithoutSuppress();
    TestRealKeysStayNotedEvenIfWeAlsoInjectThem();
    TestTakeConsumesExactlyOnce();
    TestKeyUpMustNotConsume();
    TestRestoreOnSendFailure();
    TestResetClears();
    TestNotedStateIsSticky();

    if (g_failures == 0)
    {
        std::printf("\nAll passthrough_note tests passed.\n");
        return 0;
    }
    std::printf("\n%d check(s) FAILED.\n", g_failures);
    return 1;
}
