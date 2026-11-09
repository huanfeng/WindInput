#include "ShmFrame.h"

#include "ExtProtocol.h"
#include "Protocol.h"

#include <algorithm>
#include <cerrno>
#include <cmath>
#include <cstring>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

namespace windlinux {

uint32_t ShmFrameReader::currentUid()
{
    return uint32_t(::getuid());
}

bool ShmFrameReader::open(const std::string& name, size_t maxSize, uint32_t owner)
{
    close();
    int fd = ::shm_open(name.c_str(), O_RDONLY, 0);
    if (fd < 0) {
        lastError_ = std::string("shm_open: ") + std::strerror(errno);
        return false;
    }
    struct stat st {};
    if (::fstat(fd, &st) != 0) {
        lastError_ = std::string("fstat: ") + std::strerror(errno);
        ::close(fd);
        return false;
    }
    if (uint32_t(st.st_uid) != owner) {
        lastError_ = "段的属主是 uid " + std::to_string(st.st_uid) + "，不是本用户，拒绝映射";
        ::close(fd);
        return false;
    }
    if (st.st_size < off_t(sizeof(SharedRenderHeader))) {
        lastError_ = "段只有 " + std::to_string(st.st_size) + " 字节（服务端还没定长），下一帧再试";
        ::close(fd);
        return false;
    }
    const size_t size = std::min(size_t(st.st_size), maxSize);
    void* p = ::mmap(nullptr, size, PROT_READ, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED) {
        lastError_ = std::string("mmap: ") + std::strerror(errno);
        ::close(fd);
        return false;
    }
    fd_ = fd;
    ptr_ = p;
    size_ = size;
    lastError_.clear();
    return true;
}

void ShmFrameReader::close()
{
    if (ptr_) {
        ::munmap(ptr_, size_);
        ptr_ = nullptr;
    }
    if (fd_ >= 0) {
        ::close(fd_);
        fd_ = -1;
    }
}

bool ShmFrameReader::snapshot(SharedFrame& out) const
{
    if (!ptr_ || size_ < sizeof(SharedRenderHeader)) {
        return false;
    }
    SharedRenderHeader h;
    std::memcpy(&h, ptr_, sizeof(h)); // packed 结构，拷出来再读，免得未对齐访问
    if (h.magic != SHARED_RENDER_MAGIC || h.width > kMaxFrameDim || h.height > kMaxFrameDim) {
        return false;
    }
    out.sequence = h.sequence;
    out.flags = h.flags;
    out.screenX = h.x;
    out.screenY = h.y;
    out.width = h.width;
    out.height = h.height;
    out.stride = h.stride;
    out.bgra.clear();
    if (h.dataSize == 0) {
        return true;
    }
    // 头里的尺寸自相矛盾或越出映射范围：宁可丢这一帧，也别越界读。
    uint64_t need = uint64_t(h.stride) * h.height;
    if (h.stride < uint64_t(h.width) * 4 || need > h.dataSize
        || sizeof(SharedRenderHeader) + uint64_t(h.dataSize) > size_) {
        return false;
    }
    const auto* pix = static_cast<const uint8_t*>(ptr_) + sizeof(SharedRenderHeader);
    out.bgra.assign(pix, pix + need);
    return true;
}

Rect placePanel(int32_t x, int32_t y, int32_t w, int32_t h, const Rect& wa, bool absolute)
{
    constexpr int32_t kCaretHeight = 18; // 同 macOS：拿不到真实光标高时的估值
    Rect r{x, y, w, h};
    int32_t right = wa.x + wa.w;
    int32_t bottom = wa.y + wa.h;
    if (r.x + w > right) {
        r.x = right - w;
    }
    if (r.x < wa.x) {
        r.x = wa.x;
    }
    if (!absolute && r.y + h > bottom) {
        r.y = y - kCaretHeight - h; // 翻到光标上方
    }
    if (r.y + h > bottom) {
        r.y = bottom - h;
    }
    if (r.y < wa.y) {
        r.y = wa.y;
    }
    return r;
}

Rect placeCandidate(int32_t x, int32_t y, int32_t w, int32_t h, const Rect& wa, bool absolute,
                    const std::optional<std::pair<int32_t, int32_t>>& pin, int32_t contentX,
                    int32_t contentY)
{
    if (pin) {
        return placePanel(pin->first - contentX, pin->second - contentY, w, h, wa, true);
    }
    return placePanel(x, y, w, h, wa, absolute);
}

Rect placeOverlay(const OverlayFramePayload& p, const Rect& wa, int32_t shiftX, int32_t shiftY)
{
    const int32_t cw = p.contentW ? int32_t(p.contentW) : int32_t(p.width);
    const int32_t ch = p.contentH ? int32_t(p.contentH) : int32_t(p.height);
    const int32_t left = wa.x;
    const int32_t top = wa.y;
    const int32_t right = wa.x + wa.w;
    const int32_t bottom = wa.y + wa.h;
    int32_t x = p.x;
    int32_t y = p.y;
    if (p.place == OVERLAY_PLACE_EXACT) {
        // 菜单：落位已在服务端按本端报去的工作区做完，命中测试也按这个位置——一个像素都不能挪。
        return Rect{x - p.contentX, y - p.contentY, int32_t(p.width), int32_t(p.height)};
    }
    switch (p.place) {
    case OVERLAY_PLACE_FOLLOW_CANDIDATE:
    case OVERLAY_PLACE_FLIP: {
        const bool follow = p.place == OVERLAY_PLACE_FOLLOW_CANDIDATE;
        const int32_t dx = follow ? shiftX : 0;
        const int32_t dy = follow ? shiftY : 0;
        x = p.x + dx;
        y = p.y + dy;
        if (x + cw > right) {
            x = p.altX + dx;
        }
        if (y + ch > bottom) {
            int32_t alt = p.altY + dy;
            y = alt >= top ? alt : bottom - ch;
        }
        break;
    }
    case OVERLAY_PLACE_ANCHOR: {
        const int32_t m = p.margin;
        const int32_t cx = (left + right) / 2 - cw / 2;
        const int32_t cy = (top + bottom) / 2 - ch / 2;
        const int32_t l = left + m;
        const int32_t r = right - m - cw;
        const int32_t t = top + m;
        const int32_t b = bottom - m - ch;
        switch (p.anchor) {
        case OVERLAY_ANCHOR_TOP_LEFT: x = l; y = t; break;
        case OVERLAY_ANCHOR_TOP_RIGHT: x = r; y = t; break;
        case OVERLAY_ANCHOR_BOTTOM_LEFT: x = l; y = b; break;
        case OVERLAY_ANCHOR_BOTTOM_RIGHT: x = r; y = b; break;
        case OVERLAY_ANCHOR_TOP_CENTER: x = cx; y = t; break;
        case OVERLAY_ANCHOR_BOTTOM_CENTER: x = cx; y = b; break;
        case OVERLAY_ANCHOR_CENTER:
        default: x = cx; y = cy; break;
        }
        break;
    }
    case OVERLAY_PLACE_ABSOLUTE:
    default:
        break;
    }
    // 夹回：先右/下再左/上，内容比工作区还大时保住左上角可见。
    if (x + cw > right) {
        x = right - cw;
    }
    if (x < left) {
        x = left;
    }
    if (y + ch > bottom) {
        y = bottom - ch;
    }
    if (y < top) {
        y = top;
    }
    return Rect{x - p.contentX, y - p.contentY, int32_t(p.width), int32_t(p.height)};
}

int32_t hitTest(const std::vector<CandidateHitRect>& rects, int32_t px, int32_t py)
{
    for (const auto& r : rects) {
        if (r.contains(px, py)) {
            return r.index;
        }
    }
    return kNoHit;
}

std::optional<std::vector<Rect>> candidateInputRegion(const CandidateSkin& skin, uint32_t w,
                                                      uint32_t h)
{
    if ((skin.flags & SKIN_FLAG_ACTIVE) == 0 || (skin.flags & SKIN_FLAG_MASK) == 0) {
        return std::nullopt;
    }
    std::vector<Rect> out;
    out.reserve(skin.mask.size());
    for (const Rect& r : skin.mask) {
        const int64_t x0 = std::max<int64_t>(0, r.x), y0 = std::max<int64_t>(0, r.y);
        const int64_t x1 = std::min<int64_t>(w, int64_t(r.x) + r.w);
        const int64_t y1 = std::min<int64_t>(h, int64_t(r.y) + r.h);
        if (x1 > x0 && y1 > y0) {
            out.push_back(Rect{int32_t(x0), int32_t(y0), int32_t(x1 - x0), int32_t(y1 - y0)});
        }
    }
    return out;
}

int32_t decorAt(const CandidateSkin& skin, int32_t x, int32_t y)
{
    if ((skin.flags & SKIN_FLAG_ACTIVE) == 0) {
        return -1;
    }
    auto in = [x, y](const Rect& r) {
        return x >= r.x && y >= r.y && int64_t(x) < int64_t(r.x) + r.w
               && int64_t(y) < int64_t(r.y) + r.h;
    };
    const bool inFrame = in(skin.frame);
    for (auto it = skin.decor.rbegin(); it != skin.decor.rend(); ++it) {
        if (it->hit == SKIN_HIT_PASS || !in(it->rect)) {
            continue;
        }
        if ((it->flags & SKIN_DECOR_UNDER) != 0 && inFrame) {
            continue;
        }
        return int32_t(it->index);
    }
    return -1;
}

bool onNoDragDecor(const CandidateSkin& skin, int32_t x, int32_t y)
{
    if ((skin.flags & SKIN_FLAG_ACTIVE) == 0) {
        return false;
    }
    auto in = [x, y](const Rect& r) {
        return x >= r.x && y >= r.y && int64_t(x) < int64_t(r.x) + r.w
               && int64_t(y) < int64_t(r.y) + r.h;
    };
    if (in(skin.frame)) {
        return false;
    }
    for (const SkinDecor& d : skin.decor) {
        if (d.hit != SKIN_HIT_DRAG && in(d.rect)) {
            return true;
        }
    }
    return false;
}

Rect toLogicalCover(const Rect& r, double scale)
{
    if (!(scale > 0)) {
        scale = 1;
    }
    const auto x0 = int32_t(std::floor(r.x / scale)), y0 = int32_t(std::floor(r.y / scale));
    const auto x1 = int32_t(std::ceil((double(r.x) + r.w) / scale));
    const auto y1 = int32_t(std::ceil((double(r.y) + r.h) / scale));
    return Rect{x0, y0, std::max(1, x1 - x0), std::max(1, y1 - y0)};
}

bool animInPlace(uint32_t flags, uint32_t notifySeq, uint32_t shmSeq)
{
    return (flags & FRAME_FLAG_ANIM_FRAME) != 0 && notifySeq == shmSeq;
}

} // namespace windlinux
