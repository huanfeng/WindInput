#pragma once

// Host render 候选窗的 band 复核判据 + 落位诊断上报的编码。纯 C++17、不含 Win32 头，
// 本机 g++ 可测（tests/host_band_policy_test.cpp）。
//
// 背景：SearchHost 开机预启动、界面还没显示时就收到 activation，建窗时前台不是它，
// GetHostBand 只能枚举猜 band，猜出的层在开始菜单之下、也拿不到 owner，于是开机第一次在
// 开始菜单打字候选被盖住（2026-10-09 靶机）。CTextService::RecheckHostBand 在一段输入的
// 第一个键按前台 band 复核，判据在这里。

#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

namespace wind::hostband
{

// 是否需要整组重建 host 窗口。
//   fgBand        本进程前台窗口的 band；前台不是本进程时为 0
//   requestedBand 建窗时决定往哪个 band 建（不是 GetWindowBand 复核的实际值：建窗回退
//                 到 band=0 时实际值恒与前台不等，拿它比会每个键都重建一次）
//   hasOwner      建窗时拿到了本进程前台窗口作 owner
//   windowAlive   候选窗句柄仍有效（owner 被宿主销毁时被 own 的窗口会被连带销毁）
inline bool NeedsRebuild(uint32_t fgBand, uint32_t requestedBand, bool hasOwner, bool windowAlive)
{
    // 前台不是本进程，或是 band<=1 的普通窗口：没有可比的依据，维持现状。
    if (fgBand <= 1)
        return false;
    return !windowAlive || fgBand != requestedBand || !hasOwner;
}

// 落位诊断上报所用的扩展信封 kind，与 wind-ipc protocol.rs 的 ext_kind::DIAG_HOST_RENDER_PLACED 对齐。
constexpr const char* kPlacedKind = "diag.host_render_placed";

struct Placement
{
    uint32_t probedBand;    // GetHostBand()：建窗时探到的宿主 band
    uint32_t requestedBand; // 据此决定往哪建
    uint32_t actualBand;    // GetWindowBand 复核的实际 band
    bool hasOwner;
};

// 落位上报的 JSON body。prev 非空 = 复核重建（trigger=recheck），带上被换掉那个窗口的
// 依据——新窗的字段在 recheck 时必然 requested=前台、owner=true，不带旧值就分不清
// 当初是 band 判错还是只缺 owner。
inline std::string PlacedBody(uint32_t pid, const Placement& now, const Placement* prev)
{
    char buf[320];
    int n = std::snprintf(buf, sizeof(buf),
        "{\"pid\":%u,\"trigger\":\"%s\",\"probed\":%u,\"requested\":%u,\"actual\":%u,\"owner\":%s",
        pid, prev ? "recheck" : "setup", now.probedBand, now.requestedBand, now.actualBand,
        now.hasOwner ? "true" : "false");
    std::string s(buf, (n > 0 && n < (int)sizeof(buf)) ? (size_t)n : 0);
    if (prev)
    {
        n = std::snprintf(buf, sizeof(buf),
            ",\"prev_probed\":%u,\"prev_requested\":%u,\"prev_actual\":%u,\"prev_owner\":%s",
            prev->probedBand, prev->requestedBand, prev->actualBand, prev->hasOwner ? "true" : "false");
        s.append(buf, (n > 0 && n < (int)sizeof(buf)) ? (size_t)n : 0);
    }
    s.push_back('}');
    return s;
}

// CMD_EXT 信封载荷：kindLen u32 LE + kind + bodyLen u32 LE + body（与 wind-ipc codec::encode_ext 同布局）。
inline std::vector<uint8_t> EncodeExt(const std::string& kind, const std::string& body)
{
    std::vector<uint8_t> p;
    p.reserve(8 + kind.size() + body.size());
    auto putU32 = [&p](uint32_t v) {
        for (int i = 0; i < 4; ++i)
            p.push_back((uint8_t)((v >> (8 * i)) & 0xFF));
    };
    putU32((uint32_t)kind.size());
    p.insert(p.end(), kind.begin(), kind.end());
    putU32((uint32_t)body.size());
    p.insert(p.end(), body.begin(), body.end());
    return p;
}

} // namespace wind::hostband
