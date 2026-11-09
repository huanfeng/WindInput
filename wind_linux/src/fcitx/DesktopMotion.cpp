#include "DesktopMotion.h"

#include <fcitx-utils/log.h>

#include <cstdint>
#include <exception>
#include <fstream>
#include <sstream>
#include <utility>
#include <vector>

// Fcitx5 的 DBus 封装随 fcitx5-utils 发（编 Fcitx5 时关了 DBus 的发行版没有这几个头文件）：没有就只读
// kdeglobals 文件。
#if __has_include(<fcitx-utils/dbus/bus.h>)
#include <fcitx-utils/dbus/bus.h>
#include <fcitx-utils/dbus/matchrule.h>
#include <fcitx-utils/dbus/message.h>
#define WIND_HAVE_FCITX_DBUS 1
#endif

FCITX_DECLARE_LOG_CATEGORY(windinput_log);
#define WIND_DEBUG() FCITX_LOGC(windinput_log, Debug)
#define WIND_INFO() FCITX_LOGC(windinput_log, Info)

namespace windlinux {
namespace {

constexpr const char* kNsAppearance = "org.freedesktop.appearance";
constexpr const char* kNsGnome = "org.gnome.desktop.interface";
constexpr const char* kNsKde = "org.kde.kdeglobals.KDE";
constexpr const char* kKeyReduced = "reduced-motion";
constexpr const char* kKeyGnome = "enable-animations";
constexpr const char* kKeyKde = "AnimationDurationFactor";

std::optional<double> readKdeglobalsFile()
{
    const std::string path = kdeglobalsPath();
    if (path.empty()) {
        return std::nullopt;
    }
    std::ifstream f(path);
    if (!f) {
        return std::nullopt;
    }
    std::ostringstream text;
    text << f.rdbuf();
    return kdeAnimationFactor(text.str());
}

#ifdef WIND_HAVE_FCITX_DBUS
constexpr const char* kService = "org.freedesktop.portal.Desktop";
constexpr const char* kPath = "/org/freedesktop/portal/desktop";
constexpr const char* kIface = "org.freedesktop.portal.Settings";
constexpr uint64_t kReadTimeoutUs = 2 * 1000 * 1000;

/// 门户的值：`Read` 回包套两层 variant（`ReadOne` 一层，SettingChanged 一层），一律剥到底再按类型折成数。
/// 只对 variant 调 `peekType`、内层类型取它报的内容签名：Fcitx5 5.1 的 sd-bus 版对基本类型 peek 时
/// 拿空指针构造 std::string，直接抛异常（在 DBus 回调里就是 abort）。
std::optional<double> readNumber(fcitx::dbus::Message& m)
{
    using fcitx::dbus::Container;
    using fcitx::dbus::Signature;
    std::string inner;
    for (int depth = 0; depth < 4; ++depth) {
        const auto [type, contents] = m.peekType();
        if (type != 'v' || contents.empty()) {
            return std::nullopt;
        }
        if (!(m >> Container(Container::Type::Variant, Signature(contents)))) {
            return std::nullopt;
        }
        if (contents != "v") {
            inner = contents;
            break;
        }
    }
    if (inner == "b") {
        bool v = false;
        return (m >> v) ? std::optional<double>(v ? 1 : 0) : std::nullopt;
    }
    if (inner == "u") {
        uint32_t v = 0;
        return (m >> v) ? std::optional<double>(v) : std::nullopt;
    }
    if (inner == "i") {
        int32_t v = 0;
        return (m >> v) ? std::optional<double>(v) : std::nullopt;
    }
    if (inner == "d") {
        double v = 0;
        return (m >> v) ? std::optional<double>(v) : std::nullopt;
    }
    if (inner == "s") {
        std::string v;
        return (m >> v) ? parseDecimal(v) : std::nullopt;
    }
    return std::nullopt;
}
#endif

} // namespace

#ifdef WIND_HAVE_FCITX_DBUS
struct DesktopMotion::Portal {
    fcitx::dbus::Bus bus{fcitx::dbus::BusType::Session};
    std::unique_ptr<fcitx::dbus::Slot> changed;
    std::vector<std::unique_ptr<fcitx::dbus::Slot>> reads;
};
#else
struct DesktopMotion::Portal {};
#endif

DesktopMotion::DesktopMotion(fcitx::EventLoop& loop, std::function<void(bool)> onChange)
    : onChange_(std::move(onChange))
{
    sources_.kdeFileFactor = readKdeglobalsFile();
#ifdef WIND_HAVE_FCITX_DBUS
    try {
        portal_ = std::make_unique<Portal>();
    } catch (const std::exception& e) {
        WIND_INFO() << "连不上会话总线，减少动态效果只看 kdeglobals：" << e.what();
    }
    if (portal_ && !portal_->bus.isOpen()) {
        portal_.reset();
    }
    if (portal_) {
        auto& p = *portal_;
        p.bus.attachEventLoop(&loop);
        // 门户后起（或重启）时也收得到：变化一律走这个信号，不必等首读成功。
        p.changed = p.bus.addMatch(
            fcitx::dbus::MatchRule(kService, kPath, kIface, "SettingChanged"),
            [this](fcitx::dbus::Message& m) {
                std::string ns, key;
                if (m >> ns >> key) {
                    onPortalValue(ns, key, readNumber(m));
                }
                return true;
            });
    }
#else
    (void)loop;
#endif
    readPortal();
    update();
}

DesktopMotion::~DesktopMotion() = default;

void DesktopMotion::readPortal()
{
#ifdef WIND_HAVE_FCITX_DBUS
    if (!portal_) {
        return;
    }
    auto& p = *portal_;
    p.reads.clear(); // 上一轮还没回的作废
    const std::pair<const char*, const char*> keys[] = {
        {kNsAppearance, kKeyReduced}, {kNsGnome, kKeyGnome}, {kNsKde, kKeyKde}};
    for (const auto& [ns, key] : keys) {
        auto call = p.bus.createMethodCall(kService, kPath, kIface, "Read");
        call << ns << key;
        p.reads.push_back(call.callAsync(
            kReadTimeoutUs,
            [this, ns = std::string(ns), key = std::string(key)](fcitx::dbus::Message& reply) {
                if (reply.isError()) {
                    // 没有门户 / 门户后端不认这个键：正常，各桌面只实现其中一部分。
                    WIND_DEBUG() << "门户没有 " << ns << " " << key << "：" << reply.errorName();
                } else {
                    onPortalValue(ns, key, readNumber(reply));
                }
                return true;
            }));
    }
#endif
}

std::optional<bool> DesktopMotion::refresh()
{
    sources_.kdeFileFactor = readKdeglobalsFile();
    readPortal();
    update();
    return last_;
}

void DesktopMotion::onPortalValue(const std::string& ns, const std::string& key,
                                  std::optional<double> value)
{
    if (ns == kNsAppearance && key == kKeyReduced) {
        sources_.reducedMotion = value;
    } else if (ns == kNsGnome && key == kKeyGnome) {
        sources_.gnomeAnimations = value;
    } else if (ns == kNsKde && key == kKeyKde) {
        sources_.kdePortalFactor = value;
    } else {
        return;
    }
    update();
}

void DesktopMotion::update()
{
    const char* desktop = std::getenv("XDG_CURRENT_DESKTOP");
    const auto now = pickReduceMotion(sources_, desktop ? desktop : "");
    if (!now || now == last_) {
        return;
    }
    last_ = now;
    WIND_INFO() << "系统减少动态效果：" << (*now ? "开" : "关");
    if (onChange_) {
        onChange_(*now);
    }
}

} // namespace windlinux
