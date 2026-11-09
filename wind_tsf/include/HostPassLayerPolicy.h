#pragma once

// Host render 帧里穿透装饰层（皮肤 `hit = "pass"`）的判读。纯 C++17、不含 Win32 头，本机 g++ 可测
// （tests/host_pass_layer_policy_test.cpp）。
//
// 服务端把穿透装饰拆成与主层同尺寸的第二层，追加在 SHM 帧的矩形表之后（偏移在 passOffset）。
// DLL 把它画进第二个 WS_EX_TRANSPARENT 窗口：点击穿过它直达宿主，主窗口里那块是透明的，同样不接鼠标
// （与服务端本地窗口路径的 WindInputCandidateDecor 一致）。

#include <cstddef>
#include <cstdint>

namespace wind::hostpass
{

// 与 BinaryProtocol.h 的 SHARED_FLAG_PASS_LAYER / SHARED_FLAG_PASS_BELOW 同值（测试里对拍）。
constexpr uint32_t kFlagPassLayer = 0x0020;
constexpr uint32_t kFlagPassBelow = 0x0040;
constexpr uint32_t kHeaderSize = 64;

// 本帧的穿透层在 SHM 里的偏移；不带、或偏移落在帧头里 / 没对齐 / 整层越过 bufferSize 时返回 0
// （按没有处理：只显示主层，穿透窗口收起）。dataSize 是主层像素字节数（穿透层同大小）。
inline uint32_t PassLayerOffset(uint32_t flags, uint32_t passOffset, uint32_t dataSize,
                                uint32_t bufferSize)
{
    if ((flags & kFlagPassLayer) == 0 || passOffset < kHeaderSize || passOffset % 4 != 0)
    {
        return 0;
    }
    if (static_cast<uint64_t>(passOffset) + dataSize > bufferSize)
    {
        return 0;
    }
    return passOffset;
}

// 穿透窗口排在候选窗下面（装饰全在窗体下）还是上面。
inline bool PassLayerBelow(uint32_t flags)
{
    return (flags & kFlagPassBelow) != 0;
}

// 穿透窗口建不出来时的退路：把穿透层合回主层（预乘 BGRA 的 over；below 时穿透层垫在下面），
// 穿透装饰按 block 显示，与旧 DLL 收到的单层帧同一观感。与服务端 wind-scene `composite_over` /
// `composite_under` 同一算式。
inline void CompositePass(uint8_t* main, const uint8_t* pass, size_t bytes, bool below)
{
    for (size_t i = 0; i + 4 <= bytes; i += 4)
    {
        const uint8_t* top = below ? main + i : pass + i;
        const uint8_t* bottom = below ? pass + i : main + i;
        uint8_t out[4];
        const uint32_t inv = 255u - top[3];
        for (int c = 0; c < 4; ++c)
        {
            const uint32_t v = top[c] + (bottom[c] * inv + 127u) / 255u;
            out[c] = static_cast<uint8_t>(v > 255u ? 255u : v);
        }
        for (int c = 0; c < 4; ++c)
        {
            main[i + c] = out[c];
        }
    }
}

} // namespace wind::hostpass
