// 系统「减少动态效果」的读取：设置门户（org.freedesktop.portal.Settings，经 Fcitx5 自带的 DBus 封装、
// 挂在 Fcitx5 事件循环上，异步）+ kdeglobals 文件。取舍见 theme-skin-animation.md §17.1，挑哪一路见
// HostUi.h `pickReduceMotion`。
#pragma once

#include "HostUi.h"

#include <fcitx-utils/event.h>

#include <functional>
#include <memory>
#include <optional>
#include <string>

namespace windlinux {

class DesktopMotion {
public:
    /// `onChange(reduce)`：结论第一次得出、或之后变了时调（主线程）。
    DesktopMotion(fcitx::EventLoop& loop, std::function<void(bool)> onChange);
    ~DesktopMotion();
    DesktopMotion(const DesktopMotion&) = delete;
    DesktopMotion& operator=(const DesktopMotion&) = delete;

    /// 服务（重）启时调：重读 kdeglobals 文件（没有门户的 KDE 会话靠它），返回此刻的结论（不触发
    /// `onChange`，调用方自己报）；另向门户重发一轮读取（门户晚于本端起来时补上），回包与此刻
    /// 不同再经 `onChange` 报。
    std::optional<bool> refresh();
    /// 此刻的结论（没读到为空）。
    std::optional<bool> current() const { return last_; }

    /// 门户的一个值到了（读到的回包或 SettingChanged）：认得的键记下，结论变了调 `onChange`。
    void onPortalValue(const std::string& ns, const std::string& key, std::optional<double> value);

private:
    std::optional<bool> pick() const;
    void update();
    /// 向门户重发一轮读取（清掉上一轮还没回的）。**不得在 Read 回调里调用**：清 `reads` 会销毁正在
    /// 执行的那个回调的 slot。回调里要再读用 `readKey`（只追加）。
    void readPortal();
    /// 读一个键；回包 NoReply 时再试 `retries` 次。
    void readKey(const std::string& ns, const std::string& key, int retries);

    struct Portal;
    std::unique_ptr<Portal> portal_; // 连不上会话总线 / Fcitx5 没带 DBus 时为空
    std::function<void(bool)> onChange_;
    MotionSources sources_;
    std::optional<bool> last_;
};

} // namespace windlinux
