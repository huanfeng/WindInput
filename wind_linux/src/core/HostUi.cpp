#include "HostUi.h"

#include "Bridge.h"
#include "GnomeCaretGeom.h"

#include "KeyMap.h"
#include "Protocol.h"

#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <locale>
#include <sstream>
#include <utility>

#include <sys/stat.h>

namespace windlinux {

namespace {

// 服务端 `wind_bridge::handler::COMPOSITION_PLACEHOLDER`。
constexpr const char* kCompositionPlaceholder = " ";

std::optional<uint32_t> leadingU32(const Bytes& p)
{
    if (p.size() < 4) {
        return std::nullopt;
    }
    uint32_t v = 0;
    std::memcpy(&v, p.data(), 4);
    return v;
}

} // namespace

std::optional<ModeStatus> modeStatusOf(const Frame& frame)
{
    switch (frame.cmd) {
    case CMD_MODE_PUSH: {
        // 只有 flags u32（中英 / 全角 / 标点），没有大写锁定位与标签。
        auto flags = leadingU32(frame.payload);
        if (!flags) {
            return std::nullopt;
        }
        ModeStatus m;
        m.chinese = (*flags & STATUS_CHINESE_MODE) != 0;
        return m;
    }
    case CMD_STATUS_UPDATE:
    case CMD_STATE_PUSH:
    case CMD_ACTIVATION_STATUS_PUSH: {
        // 状态头 flags + keyDown 数 + keyUp 数（各 u32），随后热键哈希，尾部到帧末是标签
        // （`encode_status_update_ex`：标签没有长度前缀）。
        auto flags = leadingU32(frame.payload);
        if (!flags) {
            return std::nullopt;
        }
        ModeStatus m;
        m.chinese = (*flags & STATUS_CHINESE_MODE) != 0;
        const Bytes& p = frame.payload;
        if (p.size() >= 12) {
            uint32_t down = 0;
            uint32_t up = 0;
            std::memcpy(&down, p.data() + 4, 4);
            std::memcpy(&up, p.data() + 8, 4);
            uint64_t labelAt = 12 + (uint64_t(down) + up) * 4;
            if (labelAt <= p.size()) {
                m.capsLock = (*flags & STATUS_CAPS_LOCK) != 0;
                m.label = std::string(p.begin() + labelAt, p.end());
            }
        }
        return m;
    }
    case CMD_COMMIT_TEXT:
        if (auto p = decodeCommitText(frame.payload); p && (p->flags & COMMIT_FLAG_MODE_CHANGED)) {
            ModeStatus m;
            m.chinese = (p->flags & COMMIT_FLAG_CHINESE_MODE) != 0;
            return m;
        }
        return std::nullopt;
    default:
        return std::nullopt;
    }
}

bool ModeIndicator::update(const Frame& frame)
{
    auto st = modeStatusOf(frame);
    if (!st) {
        return false;
    }
    const std::string icon = iconName();
    const std::string text = label();
    chinese_ = st->chinese;
    if (st->capsLock) {
        caps_ = *st->capsLock;
    }
    if (st->label && !st->label->empty()) {
        // 按帧自己报的状态归档（不是按本端镜像）：标签与状态位出自同一帧，天然一致。
        if (st->capsLock.value_or(false)) {
            capsLabel_ = *st->label;
        } else if (st->chinese) {
            schemaLabel_ = *st->label;
        } else {
            englishLabel_ = *st->label;
        }
    }
    return iconName() != icon || label() != text;
}

bool ModeIndicator::noteCapsLock(bool on)
{
    if (caps_ == on) {
        return false;
    }
    caps_ = on;
    return known();
}

ModeIndicator::Variant ModeIndicator::variant() const
{
    if (caps_) {
        return Variant::Caps;
    }
    return chinese() ? Variant::Chinese : Variant::English;
}

std::string ModeIndicator::dynamicIconName(Variant v, const std::string& label)
{
    return dynamicIconName(v, label, variantSuffix() == "Dev");
}

std::string ModeIndicator::dynamicIconName(Variant v, const std::string& label, bool dev)
{
    static const char* kHex = "0123456789abcdef";
    std::string s = dev ? "windinput-lbl-dev-" : "windinput-lbl-";
    s += v == Variant::Chinese ? "zh" : v == Variant::English ? "en" : "caps";
    s += '-';
    if (label.size() > 32) { // tray_icon::MAX_HEX_LABEL_BYTES
        uint64_t h = 0xcbf29ce484222325ull; // FNV-1a 64
        for (unsigned char c : label) {
            h ^= c;
            h *= 0x100000001b3ull;
        }
        s += 'h';
        for (int shift = 60; shift >= 0; shift -= 4) {
            s += kHex[(h >> shift) & 0xF];
        }
        return s;
    }
    for (unsigned char c : label) {
        s += kHex[c >> 4];
        s += kHex[c & 0xF];
    }
    return s;
}

std::string ModeIndicator::seedIconName(Variant v)
{
    switch (v) {
    case Variant::Chinese: return "windinput-zh";
    case Variant::English: return "windinput-en";
    case Variant::Caps: return "windinput-caps";
    }
    return "windinput-zh";
}

std::string ModeIndicator::iconName() const
{
    if (!known()) {
        return {};
    }
    const Variant v = variant();
    const std::string text = label();
    if (!iconRoot_.empty() && !text.empty()) {
        std::string name = dynamicIconName(v, text);
        // 服务端按尺寸从小到大写、64 最后（`tray_icon::LAST_SIZE`），它在 = 这一组写完了。
        struct stat st {};
        if (::stat((iconRoot_ + "/64x64/apps/" + name + ".png").c_str(), &st) == 0) {
            return name;
        }
    }
    return seedIconName(v);
}

std::string ModeIndicator::label() const
{
    if (!known()) {
        return {};
    }
    if (caps_) {
        return capsLabel_;
    }
    return chinese() ? schemaLabel_ : englishLabel_;
}

std::string ModeIndicator::subModeName() const
{
    if (!known()) {
        return {};
    }
    if (caps_) {
        return "大写锁定";
    }
    return chinese() ? "中文" : "英文";
}

std::string userHicolorDir()
{
    const char* data = std::getenv("XDG_DATA_HOME");
    if (data && data[0] == '/') {
        return std::string(data) + "/icons/hicolor";
    }
    const char* home = std::getenv("HOME");
    if (home && home[0]) {
        return std::string(home) + "/.local/share/icons/hicolor";
    }
    return {};
}

bool ensureIconDirs(const std::string& root)
{
    if (root.empty()) {
        return false;
    }
    std::error_code ec;
    for (const char* n : {"16x16", "22x22", "24x24", "32x32", "48x48", "64x64"}) {
        std::filesystem::create_directories(std::filesystem::path(root) / n / "apps", ec);
        if (ec) {
            return false;
        }
    }
    return true;
}

std::optional<bool> CapsLockTracker::onKey(uint32_t keysym, uint32_t states, bool release)
{
    constexpr uint32_t kCapsLock = 0xffe5; // XK_Caps_Lock
    const bool locked = (states & xstate::CapsLock) != 0;
    if (keysym != kCapsLock) {
        return locked;
    }
    if (!release) {
        // 自动重复的按下不覆盖：第一次按下时的状态才是「之前」。
        if (!pressLocked_) {
            pressLocked_ = locked;
        }
        return std::nullopt;
    }
    if (!pressLocked_) {
        return std::nullopt; // 按下落在别处（换焦点前按下）：判不了，等下一个键校准
    }
    bool now = !*pressLocked_;
    pressLocked_.reset();
    return now;
}

std::string clientPreeditText(const std::string& composition)
{
    return composition == kCompositionPlaceholder ? std::string() : composition;
}

namespace {

std::string trimmed(const std::string& s)
{
    const auto b = s.find_first_not_of(" \t\r");
    if (b == std::string::npos) {
        return std::string();
    }
    const auto e = s.find_last_not_of(" \t\r");
    return s.substr(b, e - b + 1);
}

/// 去掉组名 / 键名尾部的 KConfig 修饰（`[$i]` 锁定、`[$e]` 展开等，可以叠几个）。
std::string stripKConfigMarkers(std::string s)
{
    while (s.size() >= 4 && s.back() == ']') {
        const auto open = s.rfind("[$");
        if (open == std::string::npos || open == 0) {
            break;
        }
        s = trimmed(s.substr(0, open));
    }
    return s;
}

} // namespace

std::optional<double> parseDecimal(const std::string& s)
{
    std::istringstream in(s);
    in.imbue(std::locale::classic());
    double v = 0;
    if (!(in >> v)) {
        return std::nullopt;
    }
    in >> std::ws;
    return in.eof() ? std::optional<double>(v) : std::nullopt;
}

std::optional<double> kdeAnimationFactor(const std::string& kdeglobals)
{
    // 编辑器存出来的文件可能带 UTF-8 BOM：留着的话第一行的组名比不上。
    const bool bom = kdeglobals.compare(0, 3, "\xEF\xBB\xBF") == 0;
    std::istringstream in(bom ? kdeglobals.substr(3) : kdeglobals);
    std::string line;
    bool inKde = false;
    std::optional<double> out;
    while (std::getline(in, line)) {
        const std::string t = trimmed(line);
        if (t.empty() || t[0] == '#') {
            continue;
        }
        if (t[0] == '[') {
            inKde = stripKConfigMarkers(t) == "[KDE]"; // `[KDE][$i]`：整组锁定，仍是 KDE 组
            continue;
        }
        const auto eq = t.find('=');
        if (!inKde || eq == std::string::npos) {
            continue;
        }
        // 键可能带 KConfig 的修饰（`AnimationDurationFactor[$i]`），去掉再比。
        std::string key = trimmed(t.substr(0, eq));
        if (const auto br = key.find('['); br != std::string::npos) {
            key = trimmed(key.substr(0, br));
        }
        if (key == "AnimationDurationFactor") {
            out = parseDecimal(trimmed(t.substr(eq + 1))); // 同一键写了多次取最后一条（KConfig 同）
        }
    }
    return out;
}

std::string kdeglobalsPath()
{
    if (const char* x = std::getenv("XDG_CONFIG_HOME"); x && x[0] == '/') {
        return std::string(x) + "/kdeglobals";
    }
    if (const char* h = std::getenv("HOME"); h && h[0]) {
        return std::string(h) + "/.config/kdeglobals";
    }
    return std::string();
}

std::optional<bool> pickReduceMotion(const MotionSources& s, const std::string& xdgCurrentDesktop)
{
    if (s.reducedMotion) {
        return *s.reducedMotion != 0;
    }
    std::optional<bool> kde;
    if (const auto f = s.kdePortalFactor ? s.kdePortalFactor : s.kdeFileFactor) {
        kde = *f <= 0;
    }
    std::optional<bool> gnome;
    if (s.gnomeAnimations) {
        gnome = *s.gnomeAnimations == 0;
    }
    const bool onKde = xdgCurrentDesktopHas(xdgCurrentDesktop.c_str(), "KDE");
    return onKde ? (kde ? kde : gnome) : (gnome ? gnome : kde);
}

} // namespace windlinux
