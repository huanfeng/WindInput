#include "GnomeCaret.h"

#include <gio/gio.h>

#include <chrono>
#include <cstdint>

namespace windlinux {
namespace {

constexpr const char* kBus = "org.windinput.Caret";
constexpr const char* kPath = "/org/windinput/Caret";
constexpr const char* kIface = "org.windinput.Caret";
/// 扩展没装 / 名字没人持有 / 总线暂时不可用：这段时间内不再做同步往返。
constexpr uint64_t kBackoffMs = 3000;

uint64_t nowMs()
{
    using namespace std::chrono;
    return uint64_t(duration_cast<milliseconds>(steady_clock::now().time_since_epoch()).count());
}

/// 只在 fcitx 主线程用。成功结果不在这里按矩形复用：相对矩形相同的另一个窗口会得到错误的
/// 绝对坐标。那份复用在调用方，按输入上下文记住。这里只缓存连接，并在失败后退避。
struct Client {
    GDBusConnection* conn = nullptr;
    uint64_t backoffUntil = 0;
};

Client& client()
{
    static Client c;
    return c;
}

void armBackoff(Client& c)
{
    c.backoffUntil = nowMs() + kBackoffMs;
}

GDBusConnection* connection(Client& c)
{
    if (c.conn && g_dbus_connection_is_closed(c.conn)) {
        g_object_unref(c.conn);
        c.conn = nullptr;
    }
    if (c.conn) {
        return c.conn;
    }
    GError* err = nullptr;
    c.conn = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &err);
    if (!c.conn && err) {
        g_error_free(err);
    }
    return c.conn;
}

} // namespace

std::optional<GnomeSpot> translateRelativeSpot(int32_t x, int32_t y, int32_t w, int32_t h,
                                               double clientScale)
{
    Client& c = client();
    // 退避窗口内不再 g_bus_get_sync / g_dbus_connection_call_sync。过了才重试，
    // 这样用户中途启用扩展后，下一次光标更新还能问到。
    if (nowMs() < c.backoffUntil) {
        return std::nullopt;
    }

    GDBusConnection* conn = connection(c);
    if (!conn) {
        armBackoff(c);
        return std::nullopt;
    }

    GError* err = nullptr;
    GVariant* reply = g_dbus_connection_call_sync(
        conn, kBus, kPath, kIface, "TranslateRelativeSpot",
        g_variant_new("(iiiid)", x, y, w, h, clientScale), G_VARIANT_TYPE("(iiiid)"),
        G_DBUS_CALL_FLAGS_NONE, 50, nullptr, &err);
    if (!reply) {
        if (err) {
            g_error_free(err);
        }
        armBackoff(c);
        return std::nullopt;
    }

    GnomeSpot s;
    g_variant_get(reply, "(iiiid)", &s.x, &s.y, &s.w, &s.h, &s.shellScale);
    g_variant_unref(reply);
    c.backoffUntil = 0;
    return s;
}

} // namespace windlinux
