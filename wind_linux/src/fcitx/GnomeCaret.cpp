#include "GnomeCaret.h"

#include <gio/gio.h>

namespace windlinux {
namespace {

constexpr const char* kBus = "org.windinput.Caret";
constexpr const char* kPath = "/org/windinput/Caret";
constexpr const char* kIface = "org.windinput.Caret";

std::optional<GnomeSpot> callSpot(const char* method, GVariant* params)
{
    GError* err = nullptr;
    GDBusConnection* conn = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &err);
    if (!conn) {
        if (err) {
            g_error_free(err);
        }
        if (params) {
            g_variant_unref(params);
        }
        return std::nullopt;
    }

    GVariant* reply = g_dbus_connection_call_sync(
        conn, kBus, kPath, kIface, method, params, G_VARIANT_TYPE("(iiiid)"),
        G_DBUS_CALL_FLAGS_NONE, 50, nullptr, &err);
    g_object_unref(conn);
    if (!reply) {
        if (err) {
            g_error_free(err);
        }
        return std::nullopt;
    }

    GnomeSpot s;
    g_variant_get(reply, "(iiiid)", &s.x, &s.y, &s.w, &s.h, &s.shellScale);
    g_variant_unref(reply);
    return s;
}

} // namespace

std::optional<GnomeSpot> translateRelativeSpot(int32_t x, int32_t y, int32_t w, int32_t h,
                                               double clientScale)
{
    return callSpot("TranslateRelativeSpot",
                    g_variant_new("(iiiid)", x, y, w, h, clientScale));
}

} // namespace windlinux
