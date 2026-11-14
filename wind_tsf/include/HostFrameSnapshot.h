#pragma once

// Host render 帧从 SHM 拷出来（纯 C++17、不含 Win32 头，本机 g++ 可测：tests/host_frame_snapshot_test.cpp）。
//
// SHM 由服务端随时改写，渲染线程这边没有锁。内存安全靠两条：
//   - 帧头只读一次（`CopyHeader`），之后的校验、偏移、尺寸一律用这份拷贝；
//   - 用这份拷贝自洽地校验：宽高与 dataSize、stride 对得上，像素 / 矩形表 / 穿透层都落在缓冲区内。
//     帧头那 64 字节本身也可能读到一半新一半旧（如新 width/height + 旧 dataSize），不自洽就丢帧——
//     否则按旧的、更大的 dataSize 往按新宽高建的 DIB 里拷，写越界。
// 拷完再核一次 sequence（`CopyPayload`），只能发现「拷的途中帧头已经换了」：服务端正在写像素、帧头还没改
// 的那一刻拷到的像素会新旧混杂（撕裂一帧），这里发现不了，下一帧会纠正。不做 seqlock。
//
// 帧头 / 命中矩形的类型作模板参数：DLL 用 BinaryProtocol.h 的 SharedRenderHeader / HostRenderHitRect
// （那个头文件带 Win32 内联函数，本机编不了），测试用同布局的镜像。

#include "HostPassLayerPolicy.h"

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

namespace wind::hostframe
{

template <class Header, class Rect>
struct FrameSnapshot
{
    Header header{};
    std::vector<uint8_t> pixels; // dataSize 字节，主层 BGRA
    std::vector<Rect> rects;     // 命中矩形（表越界时为空，超上限截到上限）
    std::vector<uint8_t> pass;   // 穿透装饰层（本帧没有时为空）
};

// SHM 里此刻的 sequence（按 volatile 读，编译器不得拿先前读到的值顶替）。
template <class Header>
uint32_t ReadSequence(const uint8_t* shm)
{
    const volatile uint32_t* p =
        reinterpret_cast<const volatile uint32_t*>(shm + offsetof(Header, sequence));
    return *p;
}

template <class Header>
Header CopyHeader(const uint8_t* shm)
{
    Header h;
    std::memcpy(&h, shm, sizeof(h));
    return h;
}

// 按帧头拷贝 `h` 拷出本帧的像素、命中矩形与穿透层。宽高与 dataSize / stride 对不上（帧头读到新旧
// 混杂的一份）或像素越过缓冲区返回 false；拷完后 SHM 的 sequence 已不是 `h.sequence`（拷的途中帧头
// 换了）也返回 false。`afterCopy` 是测试钩子，在拷完、复核之前调用（模拟服务端在这一刻写了新帧）。
template <class Header, class Rect, class Hook>
bool CopyPayload(const uint8_t* shm, uint32_t bufferSize, const Header& h, uint32_t maxRects,
                 FrameSnapshot<Header, Rect>& out, Hook afterCopy)
{
    if (static_cast<uint64_t>(h.width) * h.height * 4 != h.dataSize ||
        static_cast<uint64_t>(h.width) * 4 != h.stride)
    {
        return false;
    }
    if (static_cast<uint64_t>(sizeof(Header)) + h.dataSize > bufferSize)
    {
        return false;
    }
    out.header = h;
    out.pixels.assign(shm + sizeof(Header), shm + sizeof(Header) + h.dataSize);

    out.rects.clear();
    uint32_t count = h.rectCount < maxRects ? h.rectCount : maxRects;
    const uint64_t tableEnd =
        static_cast<uint64_t>(h.rectsOffset) + static_cast<uint64_t>(count) * sizeof(Rect);
    if (count > 0 && h.rectsOffset >= sizeof(Header) && tableEnd <= bufferSize)
    {
        out.rects.resize(count);
        std::memcpy(out.rects.data(), shm + h.rectsOffset, count * sizeof(Rect));
    }

    out.pass.clear();
    const uint32_t passOff =
        wind::hostpass::PassLayerOffset(h.flags, h.passOffset, h.dataSize, bufferSize);
    if (passOff)
    {
        out.pass.assign(shm + passOff, shm + passOff + h.dataSize);
    }

    afterCopy();
    return ReadSequence<Header>(shm) == h.sequence;
}

template <class Header, class Rect>
bool CopyPayload(const uint8_t* shm, uint32_t bufferSize, const Header& h, uint32_t maxRects,
                 FrameSnapshot<Header, Rect>& out)
{
    return CopyPayload(shm, bufferSize, h, maxRects, out, [] {});
}

// 候选窗收起：放掉拷帧缓冲（一帧 2–4 MiB，否则每个宿主进程一直占着）。
template <class Header, class Rect>
void ReleaseBuffers(FrameSnapshot<Header, Rect>& out)
{
    out.pixels.clear();
    out.pixels.shrink_to_fit();
    out.rects.clear();
    out.rects.shrink_to_fit();
    out.pass.clear();
    out.pass.shrink_to_fit();
}

} // namespace wind::hostframe
