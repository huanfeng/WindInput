#include "Codec.h"

#include <cmath>

#include "ExtProtocol.h"
#include "Protocol.h"

#include <cstring>

namespace windlinux {

namespace {

void putU16(Bytes& b, uint16_t v)
{
    b.push_back(uint8_t(v & 0xFF));
    b.push_back(uint8_t(v >> 8));
}

void putU32(Bytes& b, uint32_t v)
{
    for (int i = 0; i < 4; ++i) {
        b.push_back(uint8_t((v >> (8 * i)) & 0xFF));
    }
}

void putU64(Bytes& b, uint64_t v)
{
    for (int i = 0; i < 8; ++i) {
        b.push_back(uint8_t((v >> (8 * i)) & 0xFF));
    }
}

void putLenStr(Bytes& b, const std::string& s)
{
    putU32(b, uint32_t(s.size()));
    b.insert(b.end(), s.begin(), s.end());
}

uint32_t getU32(const uint8_t* p)
{
    return uint32_t(p[0]) | (uint32_t(p[1]) << 8) | (uint32_t(p[2]) << 16)
        | (uint32_t(p[3]) << 24);
}

int32_t getI32(const uint8_t* p)
{
    return static_cast<int32_t>(getU32(p));
}

/// 把 payload 包上头成完整帧。
Bytes frame(uint16_t cmd, const Bytes& payload, bool async = false)
{
    Bytes out = encodeHeader(cmd, uint32_t(payload.size()), async);
    out.insert(out.end(), payload.begin(), payload.end());
    return out;
}

Bytes i32Frame(uint16_t cmd, int32_t v)
{
    Bytes p;
    putU32(p, static_cast<uint32_t>(v));
    return frame(cmd, p);
}

/// JSON 字符串字面量（带引号）。服务端按 serde_json 解析，控制字符一律 \u 转义。
std::string jsonString(const std::string& s)
{
    std::string out = "\"";
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') {
            out += '\\';
            out += char(c);
        } else if (c < 0x20) {
            static const char kHex[] = "0123456789abcdef";
            out += "\\u00";
            out += kHex[c >> 4];
            out += kHex[c & 0xF];
        } else {
            out += char(c);
        }
    }
    return out + "\"";
}

/// 游标式读取器：越界即置 ok=false，后续读取全部返回空值。
struct Reader {
    const Bytes& b;
    size_t off = 0;
    bool ok = true;

    explicit Reader(const Bytes& bytes) : b(bytes) {}

    bool has(size_t n) const { return ok && off + n <= b.size(); }

    uint32_t u32()
    {
        if (!has(4)) {
            ok = false;
            return 0;
        }
        uint32_t v = getU32(b.data() + off);
        off += 4;
        return v;
    }

    std::string str(size_t n)
    {
        if (!has(n)) {
            ok = false;
            return {};
        }
        std::string s(reinterpret_cast<const char*>(b.data() + off), n);
        off += n;
        return s;
    }
};

} // namespace

Bytes encodeHeader(uint16_t cmd, uint32_t payloadLen, bool async)
{
    Bytes b;
    b.reserve(HEADER_SIZE + payloadLen);
    uint16_t ver = PROTOCOL_VERSION;
    if (async) {
        ver |= ASYNC_FLAG;
    }
    putU16(b, ver);
    putU16(b, cmd);
    putU32(b, payloadLen);
    return b;
}

HeaderError decodeHeader(const uint8_t* buf, HeaderInfo& out)
{
    uint16_t ver = uint16_t(buf[0] | (buf[1] << 8));
    out.cmd = uint16_t(buf[2] | (buf[3] << 8));
    out.length = getU32(buf + 4);
    out.isAsync = (ver & ASYNC_FLAG) != 0;
    uint16_t base = ver & uint16_t(~ASYNC_FLAG);
    if ((base >> 12) != (PROTOCOL_VERSION >> 12)) {
        return HeaderError::VersionMismatch;
    }
    if (out.length > MAX_PAYLOAD_SIZE) {
        return HeaderError::PayloadTooLarge;
    }
    return HeaderError::None;
}

Bytes encodeEmptyFrame(uint16_t cmd, bool async)
{
    return encodeHeader(cmd, 0, async);
}

Bytes encodeKeyEventFrame(const KeyEvent& e)
{
    Bytes p;
    putU32(p, e.keyCode);
    putU32(p, e.scanCode);
    putU32(p, e.modifiers);
    p.push_back(e.eventType);
    p.push_back(e.toggles);
    putU16(p, e.eventSeq);
    putU16(p, e.prevChar);
    putU16(p, e.seqChar);
    static_assert(sizeof(KeyPayload) == 20, "KeyPayload 布局变了，同步这里");
    return frame(CMD_KEY_EVENT, p);
}

Bytes encodeFocusGainedFrame(uint64_t clientToken, uint64_t inputScopeMask,
                             const std::string& hostName, const std::string& windowClass)
{
    Bytes p(20, 0); // caret 段全 0（height=0 → 服务端忽略）
    putU64(p, clientToken);
    putU64(p, inputScopeMask);
    p.push_back(0); // disabled
    p.push_back(0); // reason
    p.push_back(0); // caretSource = CARET_SRC_UNKNOWN（Linux 无 TSF 语义域）
    static_assert(sizeof(FocusGainedPayload) == 39, "FocusGainedPayload 布局变了，同步这里");
    putLenStr(p, hostName);
    putLenStr(p, windowClass);
    return frame(CMD_FOCUS_GAINED, p);
}

Bytes encodeFocusLostFrame(uint64_t clientToken, uint8_t reason)
{
    Bytes p;
    putU64(p, clientToken);
    p.push_back(reason);
    static_assert(sizeof(FocusLostPayload) == 9, "FocusLostPayload 布局变了，同步这里");
    return frame(CMD_FOCUS_LOST, p);
}

Bytes encodeClientTokenFrame(uint16_t cmd, uint64_t clientToken)
{
    Bytes p;
    putU64(p, clientToken);
    return frame(cmd, p, /*async=*/true);
}

Bytes encodeCaretUpdateFrame(int32_t x, int32_t y, int32_t height)
{
    Bytes p;
    putU32(p, static_cast<uint32_t>(x));
    putU32(p, static_cast<uint32_t>(y));
    putU32(p, static_cast<uint32_t>(height));
    return frame(CMD_CARET_UPDATE, p);
}

Bytes encodeCandidateSelectFrame(int32_t index)
{
    return i32Frame(CMD_CANDIDATE_SELECT, index);
}

Bytes encodeCandidateHoverFrame(int32_t index)
{
    // 本端内部用 -1 表示「无悬停」，但线上 -1/-2 在服务端非 Windows 分支是翻页按钮
    // （`message_handler` 的 `handle_candidate_hover`）：发 -1 会让上页按钮残留悬停高亮。
    // addon 从不上报翻页按钮的悬停，故负值一律折成 INT32_MIN（与 macOS `.app` 同一哨兵）。
    return i32Frame(CMD_CANDIDATE_HOVER, index < 0 ? INT32_MIN : index);
}

Bytes encodeCandidateScrollFrame(int32_t delta)
{
    return i32Frame(CMD_CANDIDATE_SCROLL, delta);
}

Bytes encodeMenuActionFrame(int32_t id)
{
    return i32Frame(CMD_MENU_ACTION, id);
}

Bytes encodeExtFrame(const std::string& kind, const std::string& body)
{
    Bytes p;
    putLenStr(p, kind);
    putLenStr(p, body);
    return frame(CMD_EXT, p);
}

Bytes encodeMenuPointerFrame(uint32_t event, uint32_t button, int32_t x, int32_t y)
{
    Bytes p;
    putU32(p, event);
    putU32(p, button);
    putU32(p, uint32_t(x));
    putU32(p, uint32_t(y));
    return frame(CMD_MENU_POINTER, p);
}

Bytes encodeMenuOpenFrame(int32_t target, int32_t x, int32_t y, int32_t left, int32_t top,
                          int32_t right, int32_t bottom)
{
    std::string body = "{\"target\":" + std::to_string(target) + ",\"x\":" + std::to_string(x)
        + ",\"y\":" + std::to_string(y) + ",\"work\":[" + std::to_string(left) + ","
        + std::to_string(top) + "," + std::to_string(right) + "," + std::to_string(bottom) + "]}";
    return encodeExtFrame(EXT_KIND_MENU_OPEN, body);
}

Bytes encodeMenuOpenFrame(int32_t target, int32_t x, int32_t y, int32_t left, int32_t top,
                          int32_t right, int32_t bottom, int32_t lx, int32_t ly)
{
    std::string body = "{\"target\":" + std::to_string(target) + ",\"x\":" + std::to_string(x)
        + ",\"y\":" + std::to_string(y) + ",\"work\":[" + std::to_string(left) + ","
        + std::to_string(top) + "," + std::to_string(right) + "," + std::to_string(bottom)
        + "],\"lx\":" + std::to_string(lx) + ",\"ly\":" + std::to_string(ly) + "}";
    return encodeExtFrame(EXT_KIND_MENU_OPEN, body);
}

Bytes encodeHostDisplayFrame(bool caretFree, bool caretTrusted, bool inlinePreedit, double scale)
{
    return encodeHostDisplayFrame(caretFree, caretTrusted, inlinePreedit, scale, HostOutput{});
}

Bytes encodeHostDisplayFrame(bool caretFree, bool caretTrusted, bool inlinePreedit, double scale,
                             const HostOutput& output)
{
    std::string body = std::string("{\"caret_free\":") + (caretFree ? "true" : "false")
                       + ",\"caret_trusted\":" + (caretTrusted ? "true" : "false")
                       + ",\"inline_preedit\":" + (inlinePreedit ? "true" : "false")
                       // 能力声明：本 addon 能贴工具栏 / 软键盘浮层。旧 addon 不报，服务端据此不开
                       // 软键盘（否则主键区被接管而屏上什么都没有）。
                       + ",\"panels\":true";
    if (scale > 0.0) {
        // 两位小数足够表达 125% / 150% / 175% 这类档位；不用 std::to_string（带 locale 的小数点）。
        const long hundredths = std::lround(scale * 100.0);
        body += ",\"scale\":" + std::to_string(hundredths / 100) + "."
                + (hundredths % 100 < 10 ? "0" : "") + std::to_string(hundredths % 100);
    }
    if (!output.name.empty()) {
        body += ",\"output\":" + jsonString(output.name);
    }
    if (const auto& w = output.work) {
        body += ",\"work\":[" + std::to_string(w->x) + "," + std::to_string(w->y) + ","
                + std::to_string(w->x + w->w) + "," + std::to_string(w->y + w->h) + "]";
    }
    body += "}";
    return encodeExtFrame(EXT_KIND_HOST_DISPLAY, body);
}

Bytes encodeMenuDismissFrame(const std::string& reason)
{
    return encodeExtFrame(EXT_KIND_MENU_DISMISS, "{\"reason\":\"" + reason + "\"}");
}

Bytes encodePosFrame(const std::string& kind, int32_t x, int32_t y)
{
    return encodePosFrame(kind, x, y, std::string());
}

Bytes encodePosFrame(const std::string& kind, int32_t x, int32_t y, const std::string& output)
{
    std::string body = "{\"x\":" + std::to_string(x) + ",\"y\":" + std::to_string(y);
    if (!output.empty()) {
        body += ",\"output\":" + jsonString(output);
    }
    return encodeExtFrame(kind, body + "}");
}

Bytes encodeHostFullscreenFrame(bool fullscreen)
{
    return encodeExtFrame(EXT_KIND_HOST_FULLSCREEN,
                          std::string("{\"fullscreen\":") + (fullscreen ? "true" : "false") + "}");
}

Bytes encodePanelPointer(uint32_t kind, uint32_t event, uint32_t button, int32_t x, int32_t y)
{
    Bytes p;
    putU32(p, kind);
    putU32(p, event);
    putU32(p, button);
    putU32(p, uint32_t(x));
    putU32(p, uint32_t(y));
    return frame(CMD_PANEL_POINTER, p);
}

Bytes encodeFrontContextFrame(const std::string& app, const std::string& title,
                              const std::string& sel)
{
    Bytes p;
    putLenStr(p, app);
    putLenStr(p, title);
    putLenStr(p, sel);
    return frame(CMD_FRONT_CONTEXT, p);
}

std::optional<CommitTextPayload> decodeCommitText(const Bytes& p)
{
    Reader r(p);
    CommitTextPayload out;
    out.flags = r.u32();
    uint32_t textLen = r.u32();
    uint32_t compLen = r.u32();
    out.text = r.str(textLen);
    out.newComposition = r.str(compLen);
    if (!r.ok) {
        return std::nullopt;
    }
    return out;
}

std::optional<UpdateCompositionPayload> decodeUpdateComposition(const Bytes& p)
{
    Reader r(p);
    UpdateCompositionPayload out;
    out.caretPos = r.u32();
    if (!r.ok) {
        return std::nullopt;
    }
    out.text = r.str(p.size() - r.off);
    return out;
}

std::optional<CommitTextWithCursorPayload> decodeCommitTextWithCursor(const Bytes& p)
{
    Reader r(p);
    CommitTextWithCursorPayload out;
    uint32_t textLen = r.u32();
    out.cursorOffset = r.u32();
    out.text = r.str(textLen);
    if (!r.ok) {
        return std::nullopt;
    }
    return out;
}

std::optional<uint32_t> decodeMoveCursor(const Bytes& p)
{
    Reader r(p);
    uint32_t dir = r.u32();
    if (!r.ok) {
        return std::nullopt;
    }
    return dir;
}

std::optional<ReplaceBackwardPayload> decodeReplaceBackward(const Bytes& p)
{
    Reader r(p);
    ReplaceBackwardPayload out;
    out.count = r.u32();
    uint32_t textLen = r.u32();
    out.text = r.str(textLen);
    if (!r.ok) {
        return std::nullopt;
    }
    return out;
}

std::optional<DeferredCompositionPayload> decodeHoldComposition(const Bytes& p)
{
    Reader r(p);
    DeferredCompositionPayload out;
    out.timeoutMs = r.u32();
    uint32_t textLen = r.u32();
    out.holdText = r.str(textLen);
    if (!r.ok) {
        return std::nullopt;
    }
    return out;
}

std::optional<DeferredCompositionPayload> decodeCommitAndHold(const Bytes& p)
{
    Reader r(p);
    DeferredCompositionPayload out;
    out.timeoutMs = r.u32();
    uint32_t commitLen = r.u32();
    uint32_t holdLen = r.u32();
    out.commitText = r.str(commitLen);
    out.holdText = r.str(holdLen);
    if (!r.ok) {
        return std::nullopt;
    }
    return out;
}

std::string decodeKeyType(const Bytes& p)
{
    return std::string(p.begin(), p.end());
}

namespace {

/// 读一个 combo；计数先与剩余字节比对（每项至少 4 字节长度前缀），免得坏帧里的巨大计数
/// 先把内存撑爆再发现读不下去。
bool readKeyCombo(Reader& r, KeyComboPayload& out)
{
    out.key = r.str(r.u32());
    uint32_t n = r.u32();
    if (!r.has(size_t(n) * 4)) {
        return false;
    }
    out.mods.reserve(n);
    for (uint32_t i = 0; i < n && r.ok; ++i) {
        out.mods.push_back(r.str(r.u32()));
    }
    return r.ok;
}

} // namespace

std::optional<KeyComboPayload> decodeKeyCombo(const Bytes& p)
{
    Reader r(p);
    KeyComboPayload out;
    if (!readKeyCombo(r, out)) {
        return std::nullopt;
    }
    return out;
}

std::optional<std::vector<KeyComboPayload>> decodeKeySeq(const Bytes& p)
{
    Reader r(p);
    uint32_t n = r.u32();
    if (!r.has(size_t(n) * 8)) {
        return std::nullopt;
    }
    std::vector<KeyComboPayload> out(n);
    for (auto& c : out) {
        if (!readKeyCombo(r, c)) {
            return std::nullopt;
        }
    }
    return out;
}

std::optional<HostRenderFramePayload> decodeHostRenderFrame(const Bytes& p)
{
    if (p.size() < 24) {
        return std::nullopt;
    }
    HostRenderFramePayload out;
    out.seq = getU32(p.data());
    out.x = getI32(p.data() + 4);
    out.y = getI32(p.data() + 8);
    out.width = getU32(p.data() + 12);
    out.height = getU32(p.data() + 16);
    out.flags = getU32(p.data() + 20);
    // scale 是 28 字节版的扩展字段；旧 24 字节帧默认 1。
    out.scale = p.size() >= 28 ? getU32(p.data() + 24) : 1;
    if (out.scale == 0) {
        out.scale = 1;
    }
    return out;
}

std::optional<OverlayFramePayload> decodeOverlayFrame(const Bytes& p)
{
    if (p.size() < 68) {
        return std::nullopt;
    }
    const uint8_t* d = p.data();
    OverlayFramePayload o;
    o.kind = getU32(d);
    o.seq = getU32(d + 4);
    o.width = getU32(d + 8);
    o.height = getU32(d + 12);
    o.flags = getU32(d + 16);
    o.place = getU32(d + 20);
    o.x = getI32(d + 24);
    o.y = getI32(d + 28);
    o.altX = getI32(d + 32);
    o.altY = getI32(d + 36);
    o.anchor = getU32(d + 40);
    o.margin = getI32(d + 44);
    o.contentX = getI32(d + 48);
    o.contentY = getI32(d + 52);
    o.contentW = getU32(d + 56);
    o.contentH = getU32(d + 60);
    o.durationMs = getI32(d + 64);
    // 拖动区尾段在 scale（68..72）之后，只有工具栏 / 软键盘的帧才编码。没有尾段（旧服务）= 无拖动区。
    if (isPanelOverlayKind(o.kind) && p.size() >= 76) {
        const uint32_t n = getU32(d + 72);
        if ((p.size() - 76) / 16 < n) {
            return std::nullopt;
        }
        o.drag.reserve(n);
        for (uint32_t i = 0; i < n; ++i) {
            const uint8_t* r = d + 76 + size_t(i) * 16;
            o.drag.push_back(Rect{getI32(r), getI32(r + 4), getI32(r + 8), getI32(r + 12)});
        }
    }
    return o;
}

bool isPanelOverlayKind(uint32_t kind)
{
    return kind == OVERLAY_KIND_TOOLBAR || kind == OVERLAY_KIND_SOFTKEYBOARD;
}

std::optional<std::vector<CandidateHitRect>> decodeCandidateRects(const Bytes& p)
{
    Reader r(p);
    uint32_t n = r.u32();
    if (!r.ok || !r.has(size_t(n) * 20)) {
        return std::nullopt;
    }
    std::vector<CandidateHitRect> out;
    out.reserve(n);
    for (uint32_t i = 0; i < n; ++i) {
        CandidateHitRect h;
        h.index = static_cast<int32_t>(r.u32());
        h.x = static_cast<int32_t>(r.u32());
        h.y = static_cast<int32_t>(r.u32());
        h.w = static_cast<int32_t>(r.u32());
        h.h = static_cast<int32_t>(r.u32());
        out.push_back(h);
    }
    return out;
}

std::optional<ExtEnvelope> decodeExt(const Bytes& p)
{
    Reader r(p);
    ExtEnvelope out;
    uint32_t kindLen = r.u32();
    out.kind = r.str(kindLen);
    uint32_t bodyLen = r.u32();
    if (!r.ok || !r.has(bodyLen)) {
        return std::nullopt;
    }
    out.body.assign(p.begin() + long(r.off), p.begin() + long(r.off + bodyLen));
    return out;
}

uint32_t stableHash(const std::string& s)
{
    if (s.empty()) {
        return 0;
    }
    uint32_t h = 2166136261u;
    for (unsigned char c : s) {
        h = (h ^ c) * 16777619u;
    }
    return h == 0 ? 1 : h;
}

} // namespace windlinux
