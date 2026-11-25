// InstallerRunning 闸门的纯判定逻辑（不含 Win32 调用，便于单测）。
//
// 安装/卸载期间安装器在 HKLM\Software\<app>\InstallerRunning 立标记，本 DLL 见到它就不去
// 拉起服务，装完由安装计划最后一步清掉。安装器异常死亡时标记会永久残留，表现为输入法整个
// 不工作且无提示（服务起不来、重启服务也没用）。故安装器另写 `InstallerRunningOwner` =
// `"<pid>|<进程创建时间>"`，本 DLL 据此判断立标记的进程是否还活着，没了即是遗物。
//
// 身份另起值名而不并进 `InstallerRunning`：旧版 DLL 用 `WCHAR value[8]` 读该值，超过 7 个字符
// 会得到 ERROR_MORE_DATA、把标记读成不存在。故 `InstallerRunning` 必须保持 `"1"`。
//
// 值名是跨语言、跨仓约定，见 Globals.h 中 WIND_APP_REGKEY 的注释。
#ifndef WIND_INSTALLER_GUARD_H
#define WIND_INSTALLER_GUARD_H

#include <cstdint>

namespace InstallerGuard
{

/// 立标记的进程身份。
///
/// 光有 PID 不够：PID 会被复用，只比 PID 会把遗物误判成主人还活着。`(PID, 创建时间)` 不会重复。
struct Owner
{
    uint32_t pid = 0;
    /// 进程创建时间，FILETIME 原值（100ns 自 1601-01-01 UTC）。两端只需口径一致。
    uint64_t createTime = 0;
};

/// 无 `InstallerRunningOwner` 可校验时，多久算陈旧。
///
/// 只为「新 DLL + 旧安装器」存在：旧安装器不写 owner，若「没有 owner」直接判失效，安装期间服务
/// 会被拉起。正常安装 1～2 分钟内就会清掉标记，10 分钟给慢机器留十倍余量。
///
/// 参照点是注册表键（不是该值）的最后写入时间，同键下别的值被写也会刷新它；方向安全，只会挡得更久。
constexpr uint64_t kStaleAfterMs = 10ull * 60ull * 1000ull;

/// 有 `InstallerRunningOwner` 时的硬兜底：主人进程启动至今超过这个时长，无论它是否
/// 「看起来还活着」，都按遗物处理。
///
/// 调用方看不清目标进程时一律答「活着」（见 IPCClient.cpp 的 `IsOwnerAlive`），PID 被一个
/// 打不开的进程复用后就会永久卡死；这一条把它变成有界卡死。参照点用 owner 串里的创建时间，
/// 不会被同键下别的值刷新。24 小时远长于任何真实安装。
constexpr uint64_t kOwnerHardStaleMs = 24ull * 60ull * 60ull * 1000ull;

/// 十进制解析一段 `wchar_t`，到 `stop` 或串尾为止。
///
/// 不用 `wcstoull`：它吞前导空白、接受正负号、看 locale；这里是固定格式，任何偏离都判失败。
/// 溢出同样判失败（截断出来的 PID 会指向另一个进程）。
inline bool ParseU64(const wchar_t*& p, wchar_t stop, uint64_t* out)
{
    if (p == nullptr || *p < L'0' || *p > L'9')
    {
        return false;
    }
    uint64_t v = 0;
    while (*p >= L'0' && *p <= L'9')
    {
        const uint64_t d = static_cast<uint64_t>(*p - L'0');
        if (v > (UINT64_MAX - d) / 10ull)
        {
            return false; // 溢出
        }
        v = v * 10ull + d;
        ++p;
    }
    if (*p != stop)
    {
        return false;
    }
    *out = v;
    return true;
}

/// 解析 `InstallerRunningOwner` 的值：`"<pid>|<createTime>"`，十进制、无空格。
///
/// 解析失败一律返回 false（调用方回落到年龄兜底），不做部分解析：残缺的身份比没有更糟。
///
/// 改判据时须人工同步 wind-installer 的
/// `src/installer/registry.rs::owner_format_tests::parse_owner_like_reader`（写端的冻结快照，不会因这边变红）。
inline bool ParseOwner(const wchar_t* value, Owner* out)
{
    if (value == nullptr || out == nullptr)
    {
        return false;
    }
    const wchar_t* p = value;
    uint64_t pid = 0;
    if (!ParseU64(p, L'|', &pid) || pid == 0 || pid > 0xFFFFFFFFull)
    {
        return false;
    }
    ++p; // 跳过 '|'
    uint64_t create = 0;
    if (!ParseU64(p, L'\0', &create))
    {
        return false;
    }
    out->pid = static_cast<uint32_t>(pid);
    out->createTime = create;
    return true;
}

/// 距参照时刻 `refFt` 是否已超过 `limitMs`。两个时间都是 FILETIME 原值（100ns）。
///
/// `nowFt` 不晚于 `refFt`（时钟回拨等）时返回 false：拿不准就当标记有效，宁可多挡一会儿。
inline bool IsOlderThan(uint64_t refFt, uint64_t nowFt, uint64_t limitMs)
{
    if (nowFt <= refFt)
    {
        return false;
    }
    const uint64_t elapsedMs = (nowFt - refFt) / 10000ull; // 100ns → ms
    return elapsedMs > limitMs;
}

/// 年龄兜底：注册表键的最后写入时间距今是否已超过 [`kStaleAfterMs`]。仅在没有 owner
/// 可校验（旧安装器写的标记）时使用。
inline bool IsStaleByAge(uint64_t keyWriteFt, uint64_t nowFt)
{
    return IsOlderThan(keyWriteFt, nowFt, kStaleAfterMs);
}

/// 硬兜底：owner 进程的创建时刻距今是否已超过 [`kOwnerHardStaleMs`]。
///
/// 创建时间为 0（写端从不这么写，见 registry.rs）会被判为「早已陈旧」，方向与
/// 「身份对不上 → 不是原来那个进程」一致。
inline bool IsOwnerHardStale(uint64_t ownerCreateFt, uint64_t nowFt)
{
    return IsOlderThan(ownerCreateFt, nowFt, kOwnerHardStaleMs);
}

/// 闸门的最终判定（把上面两条合成一句话，便于整体单测）。
///
/// - `hasFlag`         `InstallerRunning` 是否为 "1"
/// - `hasOwner`        `InstallerRunningOwner` 是否解析成功
/// - `ownerAlive`      该 owner 进程是否仍在运行且创建时间吻合（`hasOwner` 为假时忽略）
/// - `staleByAge`      [`IsStaleByAge`] 的结果（`hasOwner` 为真时忽略）
/// - `ownerHardStale`  [`IsOwnerHardStale`] 的结果（`hasOwner` 为假时忽略）
///
/// 返回 true 表示**应当阻止启动服务**。
inline bool ShouldBlockStart(bool hasFlag, bool hasOwner, bool ownerAlive, bool staleByAge,
                             bool ownerHardStale)
{
    if (!hasFlag)
    {
        return false;
    }
    if (hasOwner)
    {
        // 硬兜底先行：调用方看不清目标进程时一律答「活着」，没有它会永久卡死。
        if (ownerHardStale)
        {
            return false;
        }
        // 其余只认身份；十分钟年龄兜底不参与，超过十分钟的慢安装仍应被保护。
        return ownerAlive;
    }
    return !staleByAge;
}

} // namespace InstallerGuard

#endif // WIND_INSTALLER_GUARD_H
