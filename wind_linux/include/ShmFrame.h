// 候选窗帧的 POSIX SHM 读端 + 落位几何（纯逻辑，不依赖 Fcitx5 / X11）。
//
// 服务进程把候选窗光栅化成 BGRA（预乘 alpha）写进 SHM（`wind-bridge/src/shared_memory_posix.rs`
// + `shared_render_frame.rs`），再经 push 通道发 CMD_HOST_RENDER_FRAME 通知。本端按名只读打开、
// 取出一帧的拷贝。对位 Swift `SharedMemoryReader.swift` 与 `CandidatePanel.show`。
//
// SHM 布局（64 字节头 = BinaryProtocol.h 的 SharedRenderHeader，小端）：
//   magic 'WIND' | version | sequence | flags | screenX | screenY | width | height |
//   stride | dataSize | … ；像素紧跟头部。服务端先写像素、最后写头，读端以 sequence 判新帧。
#pragma once

#include "Codec.h"

#include <cstdint>
#include <optional>
#include <string>
#include <vector>

namespace windlinux {

struct SharedFrame {
    uint32_t sequence = 0;
    uint32_t flags = 0;
    int32_t screenX = 0;
    int32_t screenY = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t stride = 0;
    std::vector<uint8_t> bgra; // stride * height 字节，已拷出 SHM（避免读到下一帧的撕裂数据）
};

class ShmFrameReader {
public:
    /// 服务端建段时的大小（manager_macos.rs 的 SHM_MAX）；读端按同一大小映射。
    static constexpr size_t kDefaultSize = 4 * 1024 * 1024;

    ShmFrameReader() = default;
    ~ShmFrameReader() { close(); }
    ShmFrameReader(const ShmFrameReader&) = delete;
    ShmFrameReader& operator=(const ShmFrameReader&) = delete;

    /// 按名只读打开，映射 `min(段的实际大小, maxSize)`。失败（原因见 `lastError`）时调用方
    /// 下一帧再试：
    ///   - 服务未建段（ENOENT）；
    ///   - 段的属主不是 `owner`（缺省本用户）——别的用户抢先建的同名段，内容不可信；
    ///   - 段比一个帧头还小：服务端 `shm_open(O_CREAT)` 与 `ftruncate` 之间大小为 0，这时按
    ///     4MB 映射、一读就 SIGBUS，连 fcitx5 一起带走。
    /// 映射以实际大小为界，`snapshot` 再按映射大小校验帧头里的尺寸，读不出界。
    bool open(const std::string& name, size_t maxSize = kDefaultSize, uint32_t owner = currentUid());
    const std::string& lastError() const { return lastError_; }
    static uint32_t currentUid();
    bool isOpen() const { return ptr_ != nullptr; }
    /// 服务重启会 shm_unlink + 重建段（新 inode），旧映射成了孤儿：收到 SERVICE_READY 必须
    /// close，下一帧按名重开。
    void close();

    /// 帧宽高上限：远大于任何真实候选窗 / 菜单，又在 X11 的 16 位尺寸之内——超了 `uint16_t()`
    /// 截断，xcb 请求超长（REQ_LEN_EXCEED）直接把连接关掉。
    static constexpr uint32_t kMaxFrameDim = 16384;

    /// 读当前帧。magic 不对 / 尺寸越界 / 宽高超过 `kMaxFrameDim` 返回 false。dataSize==0（隐藏帧）
    /// 返回 true、像素为空。
    bool snapshot(SharedFrame& out) const;

private:
    int fd_ = -1;
    void* ptr_ = nullptr;
    size_t size_ = 0;
    std::string lastError_;
};

// ── 落位 ────────────────────────────────────────────────────────────

/// 服务端给的帧坐标是「候选窗左上角」的建议落点（跟随光标时即光标下沿）。宿主侧再做两件事，
/// 逐条对齐 macOS `CandidatePanel.show`：
///   - 水平：右溢/左溢时回拉，整框可见；
///   - 垂直：下方放不下 → 翻到光标上方（光标高按 18 估，避免遮住光标），仍越界则夹进工作区。
/// `absolute`（FLAG_ABSOLUTE_POS，用户固定位置）只做边界钳制、不翻转——固定点不跟光标走，
/// 翻转只会让靠近屏幕底边的固定点被莫名弹到顶上。
/// 坐标系：屏幕左上为原点、y 向下（X11 根窗口坐标 = wire 坐标）。
Rect placePanel(int32_t x, int32_t y, int32_t w, int32_t h, const Rect& workArea, bool absolute);

/// 光栅浮层（CMD_OVERLAY_FRAME）的落位：返回**窗口**矩形（尺寸 = 位图尺寸）。服务端拿不到
/// 屏幕几何，只给「首选点 + 备选点」或「锚点 + 留白」，这里按工作区选定，公式逐条对位
/// Windows 各窗口的本地定位（`status_tip.rs` / `toast.rs` / `tooltip.rs`）：
///   - ABSOLUTE：原样；
///   - FLIP：内容右溢改用 altX；下溢改用 altY，altY 越过上沿则贴下沿；
///   - FOLLOW_CANDIDATE：先把四个坐标平移 (shiftX, shiftY)（候选窗实际落点 − 建议落点），再同 FLIP；
///   - ANCHOR：工作区内按锚点摆，离边 margin；居中两档不用留白。
/// 最后内容盒一律夹进工作区（阴影扩边允许出屏，同 Windows）。
///   - EXACT（菜单）：原样，**不夹回**——服务端已按工作区算定，命中测试按这个位置做。
Rect placeOverlay(const OverlayFramePayload& p, const Rect& workArea, int32_t shiftX,
                  int32_t shiftY);

/// 命中测试：返回命中的候选下标（翻页按钮为 -1 上页 / -2 下页）；没命中返回 `kNoHit`。
constexpr int32_t kNoHit = INT32_MIN;
int32_t hitTest(const std::vector<CandidateHitRect>& rects, int32_t px, int32_t py);

// ── 皮肤输入区（CMD_CANDIDATE_SKIN，设计 theme-skin-animation.md §17.3）──

/// 候选窗该只在哪些矩形里接鼠标（位图像素，已裁进 `w × h`、去掉空矩形）。皮肤没带输入区表
/// （普通主题、旧服务）返回空 optional：X11 复位成缺省输入区，Wayland 用整块候选窗。
/// 带了表但裁完为空，返回空表——整窗都不接鼠标。
std::optional<std::vector<Rect>> candidateInputRegion(const CandidateSkin& skin, uint32_t w,
                                                      uint32_t h);

/// 位图像素矩形折成逻辑坐标（÷ scale）：向外取整，宽高至少 1。Wayland 输入区用。
Rect toLogicalCover(const Rect& r, double scale);

/// 这一帧能不能照原位贴（皮肤动画换帧）：帧带 `FRAME_FLAG_ANIM_FRAME`，且 SHM 里读到的就是通知的那一帧
/// （`shmSeq == notifySeq`）。通知与 SHM 之间没有锁，读到更新的一帧时内容、尺寸可能都变了，按普通帧贴。
bool animInPlace(uint32_t flags, uint32_t notifySeq, uint32_t shmSeq);

} // namespace windlinux
