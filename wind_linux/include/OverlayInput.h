// 光栅浮层（状态气泡 / 悬停提示 / Toast）与候选窗悬停的鼠标交互纯逻辑（不依赖 Fcitx5 / X11）。
//
// 这三层浮层与候选窗同构：像素是服务端画的，本端只贴图。交互分两类：
//   - **本端就能定的**：悬停保持、拖动、Toast 点击关闭、自动隐藏的暂停——计时与落位本来就在
//     本端（同 macOS `.app`），服务端没有要跟着动的状态；
//   - **要服务端定的**：右键菜单（菜单项、勾选态、按段 / 按行命中都在服务端），经 `menu.open`
//     报上去，与候选右键菜单同一条路（见 Menu.h）。
// 每条规则对位 Windows 的哪个窗口 / 函数，写在各自的注释里。
#pragma once

#include "ShmFrame.h"

#include <cstdint>
#include <cstdlib>
#include <optional>
#include <utility>
#include <vector>

namespace windlinux {

/// 指针在某层浮层上按下时做什么。对位 Windows：
///   - 状态气泡 `StatusTipMouse`：左键拖动，右键请求菜单；
///   - 悬停提示 `TooltipMouse`：只认右键（请求菜单），左键无动作；
///   - Toast：Windows 没有鼠标交互，这里加的是「点一下提前关掉」（任意键）；
///   - 工具栏 / 软键盘：命中、高亮、按键都在服务端，按下一律 `Forward`（原样报 CMD_PANEL_POINTER）。
///     唯一例外是左键落在拖动区里——见下面带拖动区的重载。
enum class OverlayPress { None, Drag, Menu, Close, Forward };
OverlayPress overlayPressAction(uint32_t kind, uint8_t button);

/// 同上，工具栏 / 软键盘另看按下点 `(x, y)`（位图内）是否落在该帧的拖动区 `drag` 里：左键落在
/// 拖动区 → `Drag`，这次按下连同随后的移动、松开都由本端拖，松手只报一次 `pos.*`——按下**不能**
/// 转给服务端，它收到拖动区里的按下会自己再发一次「已移动」，与 `pos.*` 重复。其余 → `Forward`。
/// 别的 kind 忽略坐标，同两参版本。
OverlayPress overlayPressAction(uint32_t kind, uint8_t button, int32_t x, int32_t y,
                                const std::vector<Rect>& drag);

/// 工具栏 / 软键盘拖完后上报用的信封 kind（`pos.toolbar` / `pos.softkeyboard`）；别的 kind 返回空。
const char* panelPosKind(uint32_t kind);

/// `host.fullscreen` 的去重：只在值变化时报。初值「不全屏」与服务端默认相同，故首次报不全屏不发。
/// 服务重启后新服务回到默认，此时若仍全屏要补报一次（`resyncAfterRestart`）。
class FullscreenReporter {
public:
    /// 宿主全屏状态变为 `on`：返回要不要发。
    bool change(bool on)
    {
        if (on == fullscreen_) {
            return false;
        }
        fullscreen_ = on;
        return true;
    }
    /// 服务（重）启：返回要不要把「全屏」补报给新服务。
    bool resyncAfterRestart() const { return fullscreen_; }

private:
    bool fullscreen_ = false;
};

/// Wayland 面板发起的菜单的认领：在 layer-shell 面板上按下 / 松开（转给了服务端）后，随后
/// `kWindowUs` 内来的第一张菜单帧算面板的（工具栏右键、设置键），开在面板所在输出的底板上。
/// 期间本端若另有一处发起了菜单请求（X11 候选右键、气泡菜单…），那张菜单不是面板的：`clear`。
class PanelMenuGate {
public:
    static constexpr uint64_t kWindowUs = 1500 * 1000;
    void note(uint32_t output, uint64_t nowUs)
    {
        output_ = output;
        at_ = nowUs;
    }
    void clear()
    {
        at_ = 0;
        claimed_ = false;
    }
    bool pending(uint64_t nowUs) const { return at_ != 0 && nowUs - at_ < kWindowUs; }
    /// 认领：返回面板所在输出，此后不再 pending（一次按下只认一个菜单）。
    uint32_t take()
    {
        at_ = 0;
        claimed_ = true;
        return output_;
    }
    /// 服务端用隐藏帧收起了菜单。收的是认领到的那张时重新开窗、记回同一输出：分格菜单的「更多…」
    /// 是先收分格菜单、紧接着推主菜单，主菜单帧也要开回那块屏的底板上——落进候选 popup 的话，屏幕
    /// 坐标被当成 popup 内坐标，画布撑大、整块被合成器挤走。收的是本端请求的菜单则不动。
    void serverClosed(uint64_t nowUs)
    {
        if (claimed_) {
            claimed_ = false;
            at_ = nowUs;
        }
    }

private:
    uint32_t output_ = 0;
    uint64_t at_ = 0;
    bool claimed_ = false; // 当前（最近一张）屏幕菜单是认领来的
};

/// 宿主全屏状态该怎么报：Wayland 会话（工具栏摆在 layer-shell 上）里一律不全屏——X11 那套只看
/// XWayland 的 `_NET_ACTIVE_WINDOW`，焦点切到原生 Wayland 应用时它不变，全屏态复位不了，工具栏
/// 会一直藏着；Wayland 下靠 TOP 层被全屏应用盖住来近似。纯 X11 会话照报。
inline bool effectiveFullscreen(bool x11Fullscreen, bool waylandPanels)
{
    return x11Fullscreen && !waylandPanels;
}

/// 右键某层浮层时 `menu.open` 的 target（MENU_TARGET_STATUS / MENU_TARGET_TOOLTIP）；
/// 该层没有菜单返回 MENU_TARGET_MAIN（调用方不应走到这里）。
int32_t overlayMenuTarget(uint32_t kind);

/// 悬停门控：**「收到移动事件」不等于「用户动了鼠标」**。气泡弹在静止的指针下面、或状态刷新时
/// 挪到指针下面，X 一样会发进入 / 移动事件；把它当悬停，气泡就永远不会自动消失。故以窗口出现
/// 那一刻的指针位置为基线，位置真变了才算。对位 Windows `StatusTipMouse::accept_move`。
class HoverGate {
public:
    /// 窗口从隐藏变为显示时调用，基线取此刻的指针位置。已显示时不要调（会抹掉真实悬停）。
    void rebase(int32_t x, int32_t y)
    {
        x_ = x;
        y_ = y;
    }
    /// 一次指针事件：与基线不同才算真实移动（并推进基线）。
    bool accept(int32_t x, int32_t y)
    {
        if (x == x_ && y == y_) {
            return false;
        }
        x_ = x;
        y_ = y;
        return true;
    }

private:
    int32_t x_ = INT32_MIN;
    int32_t y_ = INT32_MIN;
};

/// 候选悬停变化的延后（毫秒；0 = 立即报）。对位 Windows `candidate_window::hover_move`：只有
/// `last` 的悬停提示正显示时才延后——去往「无」（空隙 / 离开候选窗）按 kTipGraceMs，去往另一个
/// 候选按 kTipSwitchMs。给「从候选行穿过空隙挪进提示」留时间：一离开就报，提示当场被收掉，
/// 用户永远够不着它。`raw` 为 -1 表示无候选。
constexpr uint32_t kTipGraceMs = 280;
constexpr uint32_t kTipSwitchMs = 150;
uint32_t hoverDeferMs(int32_t last, int32_t raw, bool tipShown);

/// 指针从 (px, py) 移到 (nx, ny) 这一步是不是朝着悬停提示 `tip` 去。是的话延后的悬停变化按
/// kTipSwitchMs 顺延（展开态的提示停靠在窗口外侧，过去要横穿好几格）。对位 Windows
/// `candidate_window::heading_to_tip`：沿提示所在那一侧在靠近，且偏离提示跨度不比靠近快。
bool headingToTip(int32_t px, int32_t py, int32_t nx, int32_t ny, const Rect& tip);

/// 点到提示的距离（沿提示所在那一侧的轴；在提示上为 0）。只有刷新到本次宽限的新低才顺延：
/// 指针停着抖动时一来一回刷不出新低，不会把切换无限拖住。对位 Windows `tip_gap`。
int32_t tipGap(int32_t x, int32_t y, const Rect& tip);

/// 「不改悬停」的哨兵（`tipRecheckHover` 的返回值）。
constexpr int32_t kKeepHover = INT32_MIN;

/// 离开悬停提示后宽限到期（或菜单关闭后）重新定悬停。对位 Windows `menu_step` 的
/// `Recheck` / `Closed`：指针回到提示上 → 不动；回到提示所属的候选行 → 不动（此后去留跟着
/// 候选悬停走）；在候选窗的别处 → 报那里（候选下标或 -1）；在外面 → 报 -1（提示随之收起）。
/// `held` 是本端最近报的悬停，`candidateHit` 为指针处的候选命中（kNoHit / 翻页按钮都算「无」）。
int32_t tipRecheckHover(bool onTip, bool onCandidate, int32_t candidateHit, int32_t held);

/// 拖动状态气泡：指针在 `(px, py)`、按下时指针相对窗口左上的偏移为 `(grabDx, grabDy)`，返回
/// 新的**窗口**矩形。内容盒夹进工作区，规则与 `placeOverlay` 的最后一步是同一条（阴影扩边
/// 允许出屏）——落点再经服务端存成固定位置、下一次按 ABSOLUTE 摆放时不会被挪动。
/// 对位 Windows `StatusTipMouse` 的 WM_MOUSEMOVE（那边按窗口矩形钳制，这里按内容盒，与本端
/// 其余浮层的落位一致）。`geo` 取该层最近一帧的尺寸与内容盒偏移。
Rect dragOverlay(int32_t px, int32_t py, int32_t grabDx, int32_t grabDy,
                 const OverlayFramePayload& geo, const Rect& workArea);

/// 候选窗左键点选：按下只记命中，**松开时仍落在同一项上**才选（候选与翻页按钮同理；移出后松开
/// 视为放弃，按钮的通行语义）。对位 macOS `CandidateContentView.mouseUp`（ab671477，同 Squirrel）。
/// 按下就选的话，上屏、收起候选窗在松开前就做完了，松开落到候选窗后面的应用上。
class ClickGate {
public:
    void press(int32_t hit) { pressed_ = hit; }
    /// 返回要选的命中（候选下标 / 翻页 -1 -2），不选返回 kNoHit。每次按下只交出一次。
    int32_t release(int32_t hit)
    {
        const int32_t p = pressed_;
        pressed_ = kNoHit;
        return p != kNoHit && p == hit ? p : kNoHit;
    }
    /// 候选窗收起 / 指针离开：按下作废。
    void cancel() { pressed_ = kNoHit; }

private:
    int32_t pressed_ = kNoHit;
};

/// 皮肤装饰点击（`skin.decor_click`）：按下与松开落在同一块装饰上（`decorAt` 的结果）、指针没挪过
/// 拖动阈值（各轴 ≤ `kDecorClickSlop` 像素，Windows `SM_CXDRAG` 出厂值），算点了它一下。对位
/// Windows `CandidateMouse::press_decor` / `release_decor`。坐标系由调用方定，前后一致即可。
constexpr int32_t kDecorClickSlop = 4;
class DecorClickGate {
public:
    void press(int32_t decor, int32_t x, int32_t y)
    {
        decor_ = decor;
        x_ = x;
        y_ = y;
    }
    /// 返回要报的装饰下标，不报返回 -1。每次按下只交出一次。
    int32_t release(int32_t decor, int32_t x, int32_t y)
    {
        const int32_t d = decor_;
        decor_ = -1;
        const bool still = std::abs(x - x_) <= kDecorClickSlop && std::abs(y - y_) <= kDecorClickSlop;
        return d >= 0 && d == decor && still ? d : -1;
    }
    void cancel() { decor_ = -1; }

private:
    int32_t decor_ = -1;
    int32_t x_ = 0;
    int32_t y_ = 0;
};

/// 候选窗本端拖动（X11 / XWayland；原生 Wayland 的 popup 由合成器摆位，不能拖，设计 §17.7）。按在
/// 空白处或 `drag` 装饰上时 `press`（候选 / 翻页按钮与 `onNoDragDecor` 由调用方先排除）；指针任一轴挪过
/// `kDecorClickSlop` 才开始跟随——没过阈值就松开是一次点击（`DecorClickGate` 判装饰点击），窗口不动、
/// 不报位置。过了阈值之后一直跟随，回到原处也不算点击。坐标一律是根窗口坐标。对位 Windows
/// `CandidateMouse` 的 `dragging` / `drag_anchor` / `drag_origin`。
class CandidateDragGate {
public:
    /// 按下：指针 `(px, py)`，窗口左上 `(wx, wy)`。
    void press(int32_t px, int32_t py, int32_t wx, int32_t wy)
    {
        armed_ = true;
        moving_ = false;
        px_ = px;
        py_ = py;
        wx_ = wx;
        wy_ = wy;
    }
    /// 指针移到 `(px, py)`：已在跟随（含这一步刚过阈值）返回窗口该去的左上（未夹工作区），否则空。
    std::optional<std::pair<int32_t, int32_t>> move(int32_t px, int32_t py)
    {
        if (!armed_) {
            return std::nullopt;
        }
        if (!moving_
            && std::abs(px - px_) <= kDecorClickSlop && std::abs(py - py_) <= kDecorClickSlop) {
            return std::nullopt;
        }
        moving_ = true;
        return std::make_pair(wx_ + px - px_, wy_ + py - py_);
    }
    /// 松开：返回这次是不是拖了（是的话调用方报落点、不再判点击）。每次按下只交一次。
    bool release()
    {
        const bool moved = armed_ && moving_;
        cancel();
        return moved;
    }
    /// 一次移动事件里左键是否还按着（`held`）。松开事件可能丢（按住期间窗口重建、别的抓取插进来），
    /// 之后指针悬停也带着闸门走；左键已经没按着就按松开收尾：返回这次是不是拖了（同 `release`），
    /// 没按下 / 左键仍按着返回空。对位状态气泡拖动的同一处兜底。
    std::optional<bool> releaseIfUp(bool held)
    {
        if (!armed_ || held) {
            return std::nullopt;
        }
        return release();
    }
    /// 候选窗收起 / 窗口重建 / 右键 / 菜单打开：按下作废。
    void cancel()
    {
        armed_ = false;
        moving_ = false;
    }
    bool moving() const { return armed_ && moving_; }

private:
    bool armed_ = false;
    bool moving_ = false;
    int32_t px_ = 0;
    int32_t py_ = 0;
    int32_t wx_ = 0;
    int32_t wy_ = 0;
};

/// 窗口矩形里是否包含屏幕点（半开区间）。
bool rectContains(const Rect& r, int32_t x, int32_t y);

/// 工具栏 / 软键盘的窗口夹进工作区（内容盒即整幅位图）：先右 / 下再左 / 上，比工作区还大时
/// 保住左上角可见。与 `placeOverlay` 的末步同一条规则。服务端记下的位置可能落在换过布局的
/// 显示器外面，首帧按 EXACT 摆、靠这里拉回来。
Rect clampToWorkArea(const Rect& r, const Rect& workArea);

/// `_NET_WM_STATE` 的原子列表里是否有 `_NET_WM_STATE_FULLSCREEN`（`fullscreenAtom` 为 0 表示
/// 没拿到该原子，一律不认）。
bool hasFullscreenState(const std::vector<uint32_t>& states, uint32_t fullscreenAtom);

/// 一块输出的工作区：输出矩形 ∩ `_NET_WORKAREA`（窗口管理器只给整个虚拟屏一块，常见做法是
/// 与各输出求交）。没有该属性、或交集为空（属性与输出布局对不上）时取整块输出。
Rect outputWorkArea(const Rect& output, const std::optional<Rect>& netWorkArea);

/// 屏幕点落在第几块输出（半开区间）；都不在返回 -1。
int outputIndexAt(const std::vector<Rect>& outputs, int32_t x, int32_t y);

/// 窗口中心落在第几块输出；都不在时取 `fallback`（越界取第一块），没有输出返回 -1。
/// 工具栏 / 软键盘按它决定摆在哪块屏、夹进哪块工作区。
int panelOutputIndex(const std::vector<Rect>& outputs, const Rect& window, int fallback);

/// 跨屏拖动：按未钳制的全局逻辑矩形选输出，再夹进目标工作区。
/// 调用方继续累计原始位置，不能把返回的钳制位置作为下一次移动的起点。
struct PanelDragPlacement {
    int output;
    Rect rect;
};
PanelDragPlacement placePanelDrag(const std::vector<Rect>& outputs, const Rect& desired, int fallback);

// ── Wayland 面板坐标 ──
// 工具栏 / 软键盘在 Wayland 下的「屏幕坐标」= 合成器全局逻辑坐标（xdg-output 的 logical_position
// 起算）× 服务端此刻的光栅化缩放，单位与位图像素一致——同状态气泡固定位置的约定，服务端拿位图
// 宽高与坐标做加减时不必再折算。layer surface 只能按「所在输出 + 锚定边 + 边距」摆，边距是逻辑
// 像素，故两头换算。缩放不是正数时按 1。

/// 逻辑矩形 → 面板屏幕坐标（四个量各自 × 缩放取整）。
Rect scaleRect(const Rect& logical, double scale);
/// 面板屏幕坐标里的窗口左上 → 该输出上 top|left 锚定的边距（逻辑像素，左、上）。
std::pair<int32_t, int32_t> layerMarginFor(int32_t x, int32_t y, const Rect& outputLogical,
                                           double scale);
/// 面板落位用的输出可用区（逻辑坐标）：原点取输出原点，宽高取探测 layer surface（四边锚定、
/// 独占区 0）configure 来的尺寸——合成器扣掉了任务栏这类独占区。top|left 锚定的边距本就相对
/// 可用区起算，原点不必挪；上 / 左边有任务栏时原点与真实可用区差一条任务栏，只影响越界判定。
/// 某一维没量到（≤ 0）或比输出还大（不可信）时那一维取整块输出。
Rect layerUsableArea(const Rect& outputLogical, int32_t availW, int32_t availH);
/// `layerMarginFor` 的反向：边距 → 面板屏幕坐标里的窗口左上。缩放 ≥ 1 时两者往返不漂。
std::pair<int32_t, int32_t> layerOriginFor(int32_t marginLeft, int32_t marginTop,
                                           const Rect& outputLogical, double scale);

} // namespace windlinux
