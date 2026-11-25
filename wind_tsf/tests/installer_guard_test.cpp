// InstallerGuard 单元测试
//
// 跑法（纯 C++17、不含 Win32 头，本机不需要 MSVC）：
//   g++ -std=c++17 -I../include -o installer_guard_test installer_guard_test.cpp && ./installer_guard_test
// 或经 CMake：
//   cmake -B build -DWIND_TSF_TESTS=ON && cmake --build build --target installer_guard_test
//
// 变异检验：每个兜底去掉都有用例变红——
//   · ShouldBlockStart 改成「见 hasFlag 即挡」→ 标着「← 见 hasFlag 即挡在此变红」的 3 条全红
//   · ShouldBlockStart 去掉 ownerHardStale 这条硬兜底 → OwnerHardStaleDoesNotBlock 变红
//   · ParseOwner 去掉溢出检查、改用 wcstoull → OverflowRejected / LeadingSpaceRejected 变红
//   · ParseOwner 不校验 pid != 0 → ZeroPidRejected 变红
//   · IsOlderThan 把 nowFt <= refFt 的保护去掉 → ClockSkewTreatedAsFresh 变红
//   · kStaleAfterMs 改大改小 → BoundaryAtExactly10Min 变红
//   · kOwnerHardStaleMs 改大改小 → OwnerHardStaleBoundary 变红
// 改测试时请保住这个性质——一条永远绿的测试不会告诉你任何事。

#include "InstallerGuard.h"

#include <cstdio>

namespace
{

int g_failures = 0;
const char* g_case = "";

#define CHECK(expr)                                                                      \
    do                                                                                   \
    {                                                                                    \
        if (!(expr))                                                                     \
        {                                                                                \
            ++g_failures;                                                                \
            std::printf("  FAIL [%s] %s (line %d)\n", g_case, #expr, __LINE__);           \
        }                                                                                \
    } while (0)

using InstallerGuard::Owner;

// FILETIME 单位：1ms = 10000 个 100ns 刻。
constexpr uint64_t kMs = 10000ull;

void TestParseOwnerAccepts()
{
    g_case = "ParseOwnerAccepts";
    Owner o;
    CHECK(InstallerGuard::ParseOwner(L"2628|133712345678901234", &o));
    CHECK(o.pid == 2628u);
    CHECK(o.createTime == 133712345678901234ull);

    // 创建时间为 0 语法上合法（解析器只当不透明值）。但写端绝不能写 0：比对会当场失败、
    // 闸门在安装刚开始就放行；取不到身份时宁可不写 owner，见 wind-installer 的 registry.rs。
    CHECK(InstallerGuard::ParseOwner(L"1|0", &o));
    CHECK(o.pid == 1u);
    CHECK(o.createTime == 0ull);

    // 32 位 PID 上界
    CHECK(InstallerGuard::ParseOwner(L"4294967295|1", &o));
    CHECK(o.pid == 4294967295u);
}

void TestParseOwnerRejects()
{
    g_case = "ParseOwnerRejects";
    Owner o;
    CHECK(!InstallerGuard::ParseOwner(nullptr, &o));
    CHECK(!InstallerGuard::ParseOwner(L"", &o));
    CHECK(!InstallerGuard::ParseOwner(L"2628", &o));            // 缺分隔符与第二段
    CHECK(!InstallerGuard::ParseOwner(L"2628|", &o));           // 第二段为空
    CHECK(!InstallerGuard::ParseOwner(L"|133712345678", &o));   // 第一段为空
    CHECK(!InstallerGuard::ParseOwner(L"abc|123", &o));
    CHECK(!InstallerGuard::ParseOwner(L"2628|12x34", &o));      // 第二段混入非数字
    CHECK(!InstallerGuard::ParseOwner(L"2628|123|456", &o));    // 多余字段
    CHECK(!InstallerGuard::ParseOwner(L"2628|123 ", &o));       // 尾部垃圾
    CHECK(!InstallerGuard::ParseOwner(L"1", &o));               // 只有旧格式的 "1"

    g_case = "LeadingSpaceRejected";
    // wcstoull 会吞掉前导空白并接受正负号，那会让 " 2628|1" 和 "+2628|1" 都被当成合法身份。
    // 这个格式是本产品自己写下的，任何偏离都该判为「不认识」。
    CHECK(!InstallerGuard::ParseOwner(L" 2628|1", &o));
    CHECK(!InstallerGuard::ParseOwner(L"+2628|1", &o));
    CHECK(!InstallerGuard::ParseOwner(L"-2628|1", &o));

    g_case = "ZeroPidRejected";
    // PID 0 是 System Idle Process，永远「活着」，认了它闸门就永久卡死。
    CHECK(!InstallerGuard::ParseOwner(L"0|133712345678", &o));

    g_case = "OverflowRejected";
    // 截断出来的 PID 会指向另一个进程；溢出必须判失败而不是取低位。
    CHECK(!InstallerGuard::ParseOwner(L"4294967296|1", &o));               // 超 32 位 PID
    CHECK(!InstallerGuard::ParseOwner(L"1|99999999999999999999999", &o));  // 超 64 位
}

void TestIsStaleByAge()
{
    g_case = "IsStaleByAge";
    const uint64_t base = 133712345678901234ull;
    CHECK(!InstallerGuard::IsStaleByAge(base, base));                       // 刚写下
    CHECK(!InstallerGuard::IsStaleByAge(base, base + 60ull * 1000 * kMs));  // 1 分钟，安装正常时长

    g_case = "BoundaryAtExactly10Min";
    // 刻意写死 10 分钟而不用 kStaleAfterMs 算：拿常量验常量的断言永远绿，阈值是对外承诺要钉死。
    // 用 static_assert 而非 CHECK：常量比较在 MSVC /W4 下报 C4127。
    static_assert(InstallerGuard::kStaleAfterMs == 10ull * 60ull * 1000ull,
                  "陈旧阈值是对读端行为的承诺，改它须同时改本测试与 InstallerGuard.h 的注释");
    const uint64_t tenMin = 10ull * 60ull * 1000ull * kMs;
    CHECK(!InstallerGuard::IsStaleByAge(base, base + tenMin));          // 恰好 10 分钟：仍算新鲜
    CHECK(InstallerGuard::IsStaleByAge(base, base + tenMin + kMs));     // 10 分零 1 毫秒：陈旧

    g_case = "ClockSkewTreatedAsFresh";
    // 时钟回拨（或键刚被写过、now 取得略早）时拿不准，就当它有效：宁可多等十分钟，
    // 也不要在安装真的正在进行时把服务拉起来。
    CHECK(!InstallerGuard::IsStaleByAge(base, base - 1ull));
    CHECK(!InstallerGuard::IsStaleByAge(base, 0ull));
}

void TestIsOwnerHardStale()
{
    g_case = "OwnerHardStaleBoundary";
    const uint64_t base = 133712345678901234ull;
    // 同样刻意写死 24 小时，不拿 kOwnerHardStaleMs 去算——理由见上一条。
    static_assert(InstallerGuard::kOwnerHardStaleMs == 24ull * 60ull * 60ull * 1000ull,
                  "硬兜底时长是对读端行为的承诺，改它须同时改本测试与 InstallerGuard.h 的注释");
    const uint64_t oneDay = 24ull * 60ull * 60ull * 1000ull * kMs;
    CHECK(!InstallerGuard::IsOwnerHardStale(base, base));                    // 刚启动
    CHECK(!InstallerGuard::IsOwnerHardStale(base, base + 30ull * 60 * 1000 * kMs)); // 半小时，慢安装仍受保护
    CHECK(!InstallerGuard::IsOwnerHardStale(base, base + oneDay));           // 恰好 24 小时：还不算
    CHECK(InstallerGuard::IsOwnerHardStale(base, base + oneDay + kMs));      // 24 小时零 1 毫秒

    g_case = "ZeroCreateTimeIsHardStale";
    // 创建时间为 0 的身份不该把闸门永久钉住（写端不会这么写，但读端得有个确定答案）。
    CHECK(InstallerGuard::IsOwnerHardStale(0ull, base));
}

void TestShouldBlockStart()
{
    // 参数顺序：hasFlag, hasOwner, ownerAlive, staleByAge, ownerHardStale
    g_case = "NoFlagNeverBlocks";
    // 没有标记，后面几个参数怎么摆都不该挡。
    CHECK(!InstallerGuard::ShouldBlockStart(false, false, false, false, false));
    CHECK(!InstallerGuard::ShouldBlockStart(false, true, true, false, false));

    g_case = "OwnerAliveBlocks";
    // 安装真的在进行：主人活着，挡住。这是闸门的正当用途，不能被兜底误伤。
    CHECK(InstallerGuard::ShouldBlockStart(true, true, true, false, false));
    CHECK(InstallerGuard::ShouldBlockStart(true, true, true, true, false)); // 慢安装超过 10 分钟也照挡

    g_case = "OwnerDeadDoesNotBlock";
    // ← 见 hasFlag 即挡在此变红：安装器死了、标记成了遗物，必须放行，否则输入法永久不工作。
    CHECK(!InstallerGuard::ShouldBlockStart(true, true, false, false, false));
    CHECK(!InstallerGuard::ShouldBlockStart(true, true, false, true, false));

    g_case = "OwnerHardStaleDoesNotBlock";
    // owner 路径唯一的出口。IsOwnerAlive 打不开进程时一律答「活着」，PID 被 SYSTEM 服务复用
    // 或跨用户打不开安装器 PID 时 ownerAlive 恒为 true，没有这条就永久卡死。
    CHECK(!InstallerGuard::ShouldBlockStart(true, true, true, false, true));
    CHECK(!InstallerGuard::ShouldBlockStart(true, true, true, true, true));

    g_case = "NoOwnerIgnoresHardStale";
    // 没有 owner 就没有可信的创建时间，硬兜底这一路不该有发言权，判断仍归年龄兜底。
    CHECK(InstallerGuard::ShouldBlockStart(true, false, false, false, true));

    g_case = "NoOwnerFallsBackToAge";
    // 新 DLL + 旧安装器：没有身份可查，只能看年龄。
    CHECK(InstallerGuard::ShouldBlockStart(true, false, false, false, false));   // 还新鲜 → 照挡（保住旧安装器的安装期）
    // ← 见 hasFlag 即挡在此变红
    CHECK(!InstallerGuard::ShouldBlockStart(true, false, false, true, false));   // 已陈旧 → 放行
}

} // namespace

int main()
{
    std::printf("InstallerGuard tests\n");
    TestParseOwnerAccepts();
    TestParseOwnerRejects();
    TestIsStaleByAge();
    TestIsOwnerHardStale();
    TestShouldBlockStart();

    if (g_failures == 0)
    {
        std::printf("all passed\n");
        return 0;
    }
    std::printf("%d failure(s)\n", g_failures);
    return 1;
}
