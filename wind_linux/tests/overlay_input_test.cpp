// 光栅浮层的鼠标交互纯逻辑：按键 → 动作、菜单 target、悬停门控、候选悬停的延后、离开提示后
// 的重定、拖动落位（与 placeOverlay 同一条夹回规则），以及两条新增的上行信封编码。

#include "Bridge.h"
#include "Codec.h"
#include "ExtProtocol.h"
#include "OverlayInput.h"
#include "ShmFrame.h"
#include "TestHarness.h"

#include <cstring>

using namespace windlinux;

namespace {

uint32_t u32At(const Bytes& b, size_t off)
{
    uint32_t v = 0;
    std::memcpy(&v, b.data() + off, 4);
    return v;
}

std::string extBody(const Bytes& frame, std::string& kind)
{
    uint32_t kl = u32At(frame, HEADER_SIZE);
    kind.assign(reinterpret_cast<const char*>(frame.data() + HEADER_SIZE + 4), kl);
    uint32_t bl = u32At(frame, HEADER_SIZE + 4 + kl);
    return std::string(reinterpret_cast<const char*>(frame.data() + HEADER_SIZE + 8 + kl), bl);
}

/// 一帧状态气泡：位图 120x40，内容盒 100x24、左上扩边 (8, 6)。
OverlayFramePayload statusGeo()
{
    OverlayFramePayload p;
    p.kind = OVERLAY_KIND_STATUS;
    p.width = 120;
    p.height = 40;
    p.contentX = 8;
    p.contentY = 6;
    p.contentW = 100;
    p.contentH = 24;
    return p;
}

} // namespace

int main()
{
    CASE("按下动作：气泡左键拖动 / 右键菜单；提示只认右键；Toast 任意键（不含滚轮）关闭");
    CHECK(overlayPressAction(OVERLAY_KIND_STATUS, 1) == OverlayPress::Drag);
    CHECK(overlayPressAction(OVERLAY_KIND_STATUS, 3) == OverlayPress::Menu);
    CHECK(overlayPressAction(OVERLAY_KIND_STATUS, 2) == OverlayPress::None);
    CHECK(overlayPressAction(OVERLAY_KIND_TOOLTIP, 3) == OverlayPress::Menu);
    CHECK(overlayPressAction(OVERLAY_KIND_TOOLTIP, 1) == OverlayPress::None);
    CHECK(overlayPressAction(OVERLAY_KIND_TOAST, 1) == OverlayPress::Close);
    CHECK(overlayPressAction(OVERLAY_KIND_TOAST, 3) == OverlayPress::Close);
    CHECK(overlayPressAction(OVERLAY_KIND_TOAST, 4) == OverlayPress::None);
    CHECK(overlayPressAction(OVERLAY_KIND_TOAST, 5) == OverlayPress::None);
    CHECK(overlayPressAction(OVERLAY_KIND_MENU, 1) == OverlayPress::None);

    CASE("菜单 target：气泡 / 提示各一档，与候选下标（≥0）和主菜单（-1）不撞");
    CHECK_EQ(overlayMenuTarget(OVERLAY_KIND_STATUS), MENU_TARGET_STATUS);
    CHECK_EQ(overlayMenuTarget(OVERLAY_KIND_TOOLTIP), MENU_TARGET_TOOLTIP);
    CHECK_EQ(overlayMenuTarget(OVERLAY_KIND_TOAST), MENU_TARGET_MAIN);
    CHECK(MENU_TARGET_STATUS < 0 && MENU_TARGET_TOOLTIP < 0);
    CHECK(MENU_TARGET_STATUS != MENU_TARGET_MAIN && MENU_TARGET_TOOLTIP != MENU_TARGET_MAIN
          && MENU_TARGET_STATUS != MENU_TARGET_TOOLTIP);

    CASE("悬停门控：窗口出现在静止指针下不算悬停，指针真动了才算");
    HoverGate g;
    g.rebase(300, 400);
    CHECK(!g.accept(300, 400)); // 映射引发的进入 / 移动：位置没变
    CHECK(g.accept(301, 400));
    CHECK(!g.accept(301, 400)); // 同一点重复事件
    CHECK(g.accept(305, 402));
    HoverGate fresh;
    CHECK(fresh.accept(0, 0)); // 没定过基线：任何位置都是真实移动

    CASE("候选悬停延后：只在所离开候选的提示正显示时延后（同 Windows hover_move）");
    CHECK_EQ(hoverDeferMs(-1, 2, true), 0u);             // 从「无」进候选：立即
    CHECK_EQ(hoverDeferMs(1, 2, false), 0u);             // 提示没显示：立即
    CHECK_EQ(hoverDeferMs(1, -1, true), kTipGraceMs);    // 去往「无」：宽限
    CHECK_EQ(hoverDeferMs(1, 2, true), kTipSwitchMs);    // 去往另一个候选：短一些
    CHECK_EQ(hoverDeferMs(1, 1, true), 0u);              // 没变
    CHECK(kTipSwitchMs < kTipGraceMs);
    CHECK_EQ(kTipGraceMs, 280u); // 与 tooltip.rs LEAVE_GRACE_MS / candidate_window TIP_GRACE 同值

    CASE("朝向提示：同 Windows heading_to_tip 那张表");
    {
        const Rect rightTop{100, 0, 100, 40};
        CHECK(headingToTip(10, 120, 14, 120, rightTop));  // 沿行水平右移
        CHECK(headingToTip(10, 120, 14, 116, rightTop));  // 斜着往上
        CHECK(!headingToTip(10, 120, 14, 126, rightTop)); // 往下偏得比靠近快
        CHECK(!headingToTip(10, 120, 6, 120, rightTop));  // 往回走
        CHECK(!headingToTip(10, 120, 10, 120, rightTop)); // 没动
        CHECK(headingToTip(10, 20, 12, 26, rightTop));    // 跨度内上下随意
        CHECK(headingToTip(10, 20, 6, 20, Rect{-200, 0, 100, 40}));  // 提示在左侧
        CHECK(headingToTip(40, 10, 40, 14, Rect{0, 50, 80, 70}));    // 提示在下方
        CHECK(!headingToTip(40, 10, 40, 6, Rect{0, 50, 80, 70}));
        CHECK(!headingToTip(150, 60, 151, 60, Rect{100, 0, 100, 100})); // 已在提示上
        CHECK_EQ(tipGap(25, 5, rightTop), 75);  // 同 Windows tip_gap
        CHECK_EQ(tipGap(40, 10, Rect{0, 50, 80, 70}), 40);
        CHECK_EQ(tipGap(150, 60, Rect{100, 0, 100, 100}), 0);
    }

    CASE("离开提示后重定悬停：回到提示 / 回到所属候选不动；候选窗别处报那里；外面报 -1");
    CHECK_EQ(tipRecheckHover(true, false, kNoHit, 2), kKeepHover);
    CHECK_EQ(tipRecheckHover(false, true, 2, 2), kKeepHover);
    CHECK_EQ(tipRecheckHover(false, true, 3, 2), 3);
    CHECK_EQ(tipRecheckHover(false, true, -1, 2), -1); // 翻页按钮算「无」
    CHECK_EQ(tipRecheckHover(false, true, kNoHit, 2), -1);
    CHECK_EQ(tipRecheckHover(false, false, kNoHit, 2), -1);
    CHECK_EQ(tipRecheckHover(false, false, kNoHit, -1), kKeepHover); // 本就是「无」

    CASE("拖动：窗口跟着指针走（抓取偏移不变）");
    const Rect wa{0, 0, 1280, 800};
    OverlayFramePayload geo = statusGeo();
    // 在窗口内 (30, 10) 处按下，指针拖到 (500, 300)：窗口左上 = (470, 290)。
    Rect r = dragOverlay(500, 300, 30, 10, geo, wa);
    CHECK_EQ(r.x, 470);
    CHECK_EQ(r.y, 290);
    CHECK_EQ(r.w, 120);
    CHECK_EQ(r.h, 40);

    CASE("拖动：拖出屏幕时内容盒夹回工作区（阴影扩边可出屏），与 placeOverlay 同一规则");
    r = dragOverlay(5, 3, 30, 10, geo, wa); // 窗口想到 (-25, -7)
    CHECK_EQ(r.x + geo.contentX, 0);        // 内容盒左上贴工作区
    CHECK_EQ(r.y + geo.contentY, 0);
    r = dragOverlay(1279, 799, 30, 10, geo, wa);
    CHECK_EQ(r.x + geo.contentX + int32_t(geo.contentW), 1280);
    CHECK_EQ(r.y + geo.contentY + int32_t(geo.contentH), 800);
    // 左侧副屏（工作区从负坐标起）：负坐标是合法落点。
    const Rect left{-1920, 0, 1920, 1080};
    r = dragOverlay(-1000, 500, 30, 10, geo, left);
    CHECK_EQ(r.x, -1030);
    CHECK_EQ(r.y, 490);
    // 夹回后的落点按 ABSOLUTE 重新摆放，位置不变（落盘 → 下次显示不跳）。
    OverlayFramePayload again = geo;
    again.place = OVERLAY_PLACE_ABSOLUTE;
    Rect d = dragOverlay(1279, 799, 30, 10, geo, wa);
    again.x = d.x + geo.contentX;
    again.y = d.y + geo.contentY;
    Rect back = placeOverlay(again, wa, 0, 0);
    CHECK_EQ(back.x, d.x);
    CHECK_EQ(back.y, d.y);

    CASE("ClickGate：松开仍在同一项才选，且只选一次");
    {
        ClickGate g;
        g.press(2);
        CHECK_EQ(g.release(2), 2);
        CHECK_EQ(g.release(2), kNoHit);
    }
    CASE("ClickGate：按下后移到别项 / 空白再松开 = 放弃");
    {
        ClickGate g;
        g.press(2);
        CHECK_EQ(g.release(3), kNoHit);
        g.press(2);
        CHECK_EQ(g.release(kNoHit), kNoHit);
    }
    CASE("ClickGate：翻页按钮（负下标）同理；没按下就松开不选");
    {
        ClickGate g;
        g.press(-2);
        CHECK_EQ(g.release(-2), -2);
        CHECK_EQ(g.release(0), kNoHit);
        g.press(kNoHit);
        CHECK_EQ(g.release(kNoHit), kNoHit);
    }
    CASE("DecorClickGate：同一块装饰、没挪过阈值才算点击，且只交一次");
    {
        DecorClickGate g;
        g.press(3, 100, 100);
        CHECK_EQ(g.release(3, 104, 96), 3);
        CHECK_EQ(g.release(3, 104, 96), -1);
        g.press(3, 100, 100);
        CHECK_EQ(g.release(3, 105, 100), -1); // 挪过阈值：是在拖
        g.press(3, 100, 100);
        CHECK_EQ(g.release(3, 100, 95), -1);
        g.press(3, 100, 100);
        CHECK_EQ(g.release(4, 100, 100), -1); // 松开在别的装饰上
        g.press(-1, 0, 0);
        CHECK_EQ(g.release(-1, 0, 0), -1); // 没按在装饰上
        g.press(0, 0, 0);
        g.cancel();
        CHECK_EQ(g.release(0, 0, 0), -1); // 收起后的松开
        g.press(0, 0, 0);
        CHECK_EQ(g.release(0, 0, 0), 0); // 下标 0 是合法装饰
    }
    CASE("CandidateDragGate：挪过阈值才跟随，过了就一直跟；没过阈值的松开不算拖");
    {
        CandidateDragGate g;
        CHECK(!g.move(10, 10).has_value()); // 没按下
        g.press(100, 100, 50, 60);
        CHECK(!g.move(104, 96).has_value()); // 阈值内
        CHECK(!g.moving());
        using P = std::optional<std::pair<int32_t, int32_t>>;
        CHECK(g.move(105, 100) == P(std::make_pair(55, 60)));
        CHECK(g.move(100, 101) == P(std::make_pair(50, 61))); // 回到阈值内也接着跟
        CHECK(g.release());
        CHECK(!g.release()); // 只交一次
        CHECK(!g.move(200, 200).has_value());
        g.press(100, 100, 50, 60);
        g.move(103, 103);
        CHECK(!g.release()); // 阈值内松开：是点击
        g.press(100, 100, 50, 60);
        g.move(100, 120);
        g.cancel();
        CHECK(!g.moving());
        CHECK(!g.release()); // 收起后的松开
        // 松开事件丢了：之后来的移动里左键已没按着，按松开收尾，闸门放掉、不再跟随。
        CHECK(!g.releaseIfUp(false).has_value()); // 没按下
        g.press(100, 100, 50, 60);
        g.move(120, 100);
        CHECK(!g.releaseIfUp(true).has_value()); // 左键还按着：照常
        CHECK(g.moving());
        CHECK(g.releaseIfUp(false) == std::optional<bool>(true)); // 拖过：要报落点
        CHECK(!g.moving());
        CHECK(!g.move(130, 130).has_value()); // 之后的悬停不带着窗口走
        g.press(100, 100, 50, 60);
        CHECK(g.releaseIfUp(false) == std::optional<bool>(false)); // 没拖过：不报
        CHECK(!g.move(130, 130).has_value());
    }
    CASE("host.reduce_motion：{reduce}");
    {
        std::string kind;
        CHECK_EQ(extBody(encodeReduceMotionFrame(true), kind), std::string(R"({"reduce":true})"));
        CHECK_EQ(kind, std::string("host.reduce_motion"));
        CHECK_EQ(extBody(encodeReduceMotionFrame(false), kind), std::string(R"({"reduce":false})"));
    }
    CASE("skin.decor_click：{index}");
    {
        std::string kind;
        const std::string body = extBody(encodeDecorClickFrame(12), kind);
        CHECK_EQ(kind, std::string("skin.decor_click"));
        CHECK_EQ(body, std::string(R"({"index":12})"));
    }
    CASE("ClickGate：候选窗收起后的松开不选");
    {
        ClickGate g;
        g.press(1);
        g.cancel();
        CHECK_EQ(g.release(1), kNoHit);
    }

    CASE("rectContains：半开区间");
    CHECK(rectContains(Rect{10, 20, 5, 5}, 10, 20));
    CHECK(rectContains(Rect{10, 20, 5, 5}, 14, 24));
    CHECK(!rectContains(Rect{10, 20, 5, 5}, 15, 24));
    CHECK(!rectContains(Rect{10, 20, 5, 5}, 9, 20));

    CASE("menu.open 带提示位图内坐标：lx/ly 追加在 work 之后（对位 Rust decode_menu_open）");
    std::string kind;
    std::string body = extBody(
        encodeMenuOpenFrame(MENU_TARGET_TOOLTIP, 300, 420, 0, 0, 1280, 800, 17, -3), kind);
    CHECK_EQ(kind, std::string("menu.open"));
    CHECK_EQ(body,
             std::string(R"({"target":-3,"x":300,"y":420,"work":[0,0,1280,800],"lx":17,"ly":-3})"));

    CASE("pos.status_tip：{x,y}（对位 Rust decode_ext_point），负坐标原样");
    body = extBody(encodePosFrame(EXT_KIND_POS_STATUS_TIP, -800, 12), kind);
    CHECK_EQ(kind, std::string("pos.status_tip"));
    CHECK_EQ(body, std::string(R"({"x":-800,"y":12})"));

    CASE("panel_press_in_drag_region_drags_else_forwards");
    {
        const std::vector<Rect> drag{Rect{0, 0, 12, 40}, Rect{100, 0, 20, 10}};
        // 工具栏：左键落在任一拖动区 → 本端拖，不报服务端（报了它会另发一次 Moved）。
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 1, 0, 0, drag) == OverlayPress::Drag);
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 1, 11, 39, drag) == OverlayPress::Drag);
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 1, 105, 5, drag) == OverlayPress::Drag);
        // 拖动区外（半开区间的右 / 下边界也算外）、或不是左键 → 原样转给服务端。
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 1, 12, 5, drag) == OverlayPress::Forward);
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 1, 50, 5, drag) == OverlayPress::Forward);
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 3, 5, 5, drag) == OverlayPress::Forward);
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLBAR, 2, 5, 5, drag) == OverlayPress::Forward);
        CHECK(overlayPressAction(OVERLAY_KIND_SOFTKEYBOARD, 4, 5, 5, drag) == OverlayPress::Forward);
        // 软键盘同理；没有拖动区时左键一律转发。
        CHECK(overlayPressAction(OVERLAY_KIND_SOFTKEYBOARD, 1, 5, 5, drag) == OverlayPress::Drag);
        CHECK(overlayPressAction(OVERLAY_KIND_SOFTKEYBOARD, 1, 5, 5, {}) == OverlayPress::Forward);
        // 非交互层：拖动区参数不起作用，与两参版本一致。
        CHECK(overlayPressAction(OVERLAY_KIND_STATUS, 1, 5, 5, {}) == OverlayPress::Drag);
        CHECK(overlayPressAction(OVERLAY_KIND_TOOLTIP, 1, 5, 5, drag) == OverlayPress::None);
        CHECK(overlayPressAction(OVERLAY_KIND_TOAST, 3, 500, 500, drag) == OverlayPress::Close);
    }

    CASE("pos.toolbar / pos.softkeyboard：{x,y[,output]}；拿不到输出名就不带该字段");
    CHECK_EQ(std::string(panelPosKind(OVERLAY_KIND_TOOLBAR)), std::string("pos.toolbar"));
    CHECK_EQ(std::string(panelPosKind(OVERLAY_KIND_SOFTKEYBOARD)), std::string("pos.softkeyboard"));
    CHECK(panelPosKind(OVERLAY_KIND_STATUS) == nullptr);
    body = extBody(encodePosFrame(EXT_KIND_POS_TOOLBAR, 1600, -20, "DP-1"), kind);
    CHECK_EQ(kind, std::string("pos.toolbar"));
    CHECK_EQ(body, std::string(R"({"x":1600,"y":-20,"output":"DP-1"})"));
    body = extBody(encodePosFrame(EXT_KIND_POS_SOFTKEYBOARD, 3, 4, ""), kind);
    CHECK_EQ(body, std::string(R"({"x":3,"y":4})"));
    // 输出名来自显示服务器，按 JSON 转义（服务端 serde_json 解析）。
    body = extBody(encodePosFrame(EXT_KIND_POS_TOOLBAR, 0, 0, "a\"b\\c\x01"), kind);
    CHECK_EQ(body, std::string(R"({"x":0,"y":0,"output":"a\"b\\c\u0001"})"));

    CASE("host.fullscreen：{fullscreen:bool}");
    body = extBody(encodeHostFullscreenFrame(true), kind);
    CHECK_EQ(kind, std::string("host.fullscreen"));
    CHECK_EQ(body, std::string(R"({"fullscreen":true})"));
    body = extBody(encodeHostFullscreenFrame(false), kind);
    CHECK_EQ(body, std::string(R"({"fullscreen":false})"));

    CASE("FullscreenReporter：只在值变化时发；服务重启后若仍全屏补报一次");
    {
        FullscreenReporter r;
        CHECK(!r.change(false)); // 初值即「不全屏」（服务端同此默认），不发
        CHECK(r.change(true));
        CHECK(!r.change(true));
        CHECK(r.change(false));
        CHECK(!r.resyncAfterRestart()); // 不全屏：新服务的默认就对
        CHECK(r.change(true));
        CHECK(r.resyncAfterRestart());
        CHECK(!r.change(true)); // 补报后仍是同值
    }

    CASE("clampToWorkArea：四边越界各自夹回，比工作区还大时保住左上");
    {
        const Rect wa{100, 50, 800, 600}; // [100, 900) × [50, 650)
        auto same = [](const Rect& a, const Rect& b) {
            return a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h;
        };
        CHECK(same(clampToWorkArea(Rect{200, 100, 50, 20}, wa), Rect{200, 100, 50, 20}));
        CHECK(same(clampToWorkArea(Rect{60, 100, 50, 20}, wa), Rect{100, 100, 50, 20}));  // 左
        CHECK(same(clampToWorkArea(Rect{880, 100, 50, 20}, wa), Rect{850, 100, 50, 20})); // 右
        CHECK(same(clampToWorkArea(Rect{200, 10, 50, 20}, wa), Rect{200, 50, 50, 20}));   // 上
        CHECK(same(clampToWorkArea(Rect{200, 640, 50, 20}, wa), Rect{200, 630, 50, 20})); // 下
        CHECK(same(clampToWorkArea(Rect{-5000, 9000, 50, 20}, wa), Rect{100, 630, 50, 20}));
        CHECK(same(clampToWorkArea(Rect{500, 500, 1000, 700}, wa), Rect{100, 50, 1000, 700}));
    }

    CASE("hasFullscreenState：_NET_WM_STATE 原子列表里有全屏原子才算");
    CHECK(hasFullscreenState({301, 302, 377}, 377));
    CHECK(!hasFullscreenState({301, 302}, 377));
    CHECK(!hasFullscreenState({}, 377));
    CHECK(!hasFullscreenState({0, 377}, 0)); // 原子没拿到（0）时不认

    CASE("outputWorkArea：输出矩形 ∩ _NET_WORKAREA；没有或不相交时取整块输出");
    {
        const Rect left{0, 0, 1920, 1080};
        const Rect right{1920, 0, 2560, 1440};
        const Rect net{0, 32, 4480, 1408}; // 顶部 32px 面板横跨两屏
        Rect w = outputWorkArea(left, net);
        CHECK(w.x == 0 && w.y == 32 && w.w == 1920 && w.h == 1048);
        w = outputWorkArea(right, net);
        CHECK(w.x == 1920 && w.y == 32 && w.w == 2560 && w.h == 1408);
        w = outputWorkArea(right, std::nullopt);
        CHECK(w.x == 1920 && w.y == 0 && w.w == 2560 && w.h == 1440);
        w = outputWorkArea(right, Rect{0, 0, 1920, 1080}); // 不相交：工作区属性不可信
        CHECK(w.x == 1920 && w.y == 0 && w.w == 2560 && w.h == 1440);
    }

    CASE("outputIndexAt：点落在哪块输出（半开区间），都不在返回 -1");
    {
        const std::vector<Rect> outs{{0, 0, 1920, 1080}, {1920, 0, 2560, 1440}};
        CHECK_EQ(outputIndexAt(outs, 10, 10), 0);
        CHECK_EQ(outputIndexAt(outs, 1919, 1079), 0);
        CHECK_EQ(outputIndexAt(outs, 1920, 0), 1);
        CHECK_EQ(outputIndexAt(outs, 100, 1200), -1); // 左屏下方的空洞
        CHECK_EQ(outputIndexAt({}, 0, 0), -1);
    }

    CASE("scaleRect：Wayland 输出的逻辑矩形折成面板屏幕坐标（位图像素 = 逻辑 × 缩放）");
    {
        Rect r = scaleRect(Rect{1920, 0, 1280, 720}, 1.5);
        CHECK(r.x == 2880 && r.y == 0 && r.w == 1920 && r.h == 1080);
        r = scaleRect(Rect{-1536, 100, 1536, 864}, 1.25);
        CHECK(r.x == -1920 && r.y == 125 && r.w == 1920 && r.h == 1080);
        r = scaleRect(Rect{10, 20, 30, 40}, 0); // 缩放无效按 1
        CHECK(r.x == 10 && r.y == 20 && r.w == 30 && r.h == 40);
    }

    CASE("layerMarginFor：面板屏幕坐标的窗口左上 → 所在输出上 top|left 锚定的边距（逻辑像素）");
    {
        const Rect right{1920, 0, 1280, 720}; // 右屏，逻辑坐标
        auto m = layerMarginFor(3000, 300, right, 1.5); // 3000/1.5=2000 → 2000-1920
        CHECK_EQ(m.first, 80);
        CHECK_EQ(m.second, 200);
        m = layerMarginFor(100, 50, Rect{0, 0, 1920, 1080}, 1.0);
        CHECK_EQ(m.first, 100);
        CHECK_EQ(m.second, 50);
        m = layerMarginFor(-1900, 125, Rect{-1536, 100, 1536, 864}, 1.25); // 左上方的屏
        CHECK_EQ(m.first, 16);
        CHECK_EQ(m.second, 0);
    }

    CASE("layerOriginFor：边距 → 面板屏幕坐标的窗口左上；与 layerMarginFor 往返不漂");
    {
        const Rect right{1920, 0, 1280, 720};
        auto o = layerOriginFor(80, 200, right, 1.5);
        CHECK_EQ(o.first, 3000);
        CHECK_EQ(o.second, 300);
        // 拖完报上去的坐标会原样回到下一帧：边距 → 坐标 → 边距必须回到原值，否则每次重绘挪一像素
        bool stable = true;
        for (double s : {1.0, 1.25, 1.5, 1.75, 2.0, 3.0}) {
            for (const Rect& out : {Rect{0, 0, 1920, 1080}, Rect{1920, 0, 1280, 720},
                                    Rect{-1536, -333, 1536, 864}}) {
                for (int32_t ml = -7; ml < 40; ml += 3) {
                    const auto px = layerOriginFor(ml, ml * 2 + 1, out, s);
                    const auto back = layerMarginFor(px.first, px.second, out, s);
                    stable = stable && back.first == ml && back.second == ml * 2 + 1;
                }
            }
        }
        CHECK(stable);
    }

    CASE("layerUsableArea：原点取输出原点、尺寸取探测到的可用区；没量到 / 量歪了取整块输出");
    {
        const Rect out{1920, 0, 1280, 720};
        Rect r = layerUsableArea(out, 1280, 680); // 底部 40 的任务栏
        CHECK(r.x == 1920 && r.y == 0 && r.w == 1280 && r.h == 680);
        r = layerUsableArea(out, 0, 0); // 还没 configure / 没有 layer-shell
        CHECK(r.x == 1920 && r.y == 0 && r.w == 1280 && r.h == 720);
        r = layerUsableArea(out, 1300, 700); // 比输出还宽：不可信，宽取输出、高照用
        CHECK(r.w == 1280 && r.h == 700);
        r = layerUsableArea(out, 1240, -5);
        CHECK(r.w == 1240 && r.h == 720);
        // 折成面板坐标后的默认右下角落在任务栏上方
        const Rect px = scaleRect(layerUsableArea(Rect{0, 0, 1536, 864}, 1536, 816), 1.25);
        CHECK(px.x == 0 && px.y == 0 && px.w == 1920 && px.h == 1020);
    }

    CASE("跨屏拖动：先选屏再钳制，不能在源屏边缘截断累计位移");
    {
        const std::vector<Rect> outs{{-1920, 200, 1920, 1080}, {0, 0, 2560, 1440}};
        auto p = placePanelDrag(outs, Rect{-210, 400, 200, 40}, 0);
        CHECK_EQ(p.output, 0);
        CHECK_EQ(p.rect.x, -210); // 原始位置仍在源屏内
        p = placePanelDrag(outs, Rect{-90, 400, 200, 40}, 0);
        CHECK_EQ(p.output, 1);
        CHECK_EQ(p.rect.x, 0); // 中心越界，落到目标屏左边
        p = placePanelDrag(outs, Rect{-300, 400, 200, 40}, 1);
        CHECK_EQ(p.output, 0); // 可以拖回负坐标的屏
        CHECK_EQ(p.rect.x, -300);
        p = placePanelDrag(outs, Rect{-500, 0, 200, 40}, 0);
        CHECK_EQ(p.output, 0); // 布局空洞保持原屏，夹到该屏顶边
        CHECK_EQ(p.rect.y, 200);
        p = placePanelDrag({}, Rect{20, 30, 200, 40}, 0);
        CHECK_EQ(p.output, -1);
        CHECK_EQ(p.rect.x, 20);
    }

    CASE("panelOutputIndex：窗口中心落在哪块输出；都不在时用回退下标");
    {
        const std::vector<Rect> outs{{0, 0, 1920, 1080}, {1920, 0, 2560, 1440}};
        CHECK_EQ(panelOutputIndex(outs, Rect{100, 100, 200, 40}, 1), 0);
        CHECK_EQ(panelOutputIndex(outs, Rect{1800, 100, 200, 40}, 0), 0); // 中心 1900 仍在左屏
        CHECK_EQ(panelOutputIndex(outs, Rect{1850, 100, 200, 40}, 0), 1); // 中心 1950 在右屏
        CHECK_EQ(panelOutputIndex(outs, Rect{100, 1200, 200, 40}, 1), 1); // 空洞：回退
        CHECK_EQ(panelOutputIndex(outs, Rect{100, 1200, 200, 40}, 7), 0); // 回退越界取第一块
        CHECK_EQ(panelOutputIndex({}, Rect{0, 0, 10, 10}, 0), -1);
    }

    CASE("PanelMenuGate：面板按下后 1.5 秒内认领一次；本端另发起菜单请求后不再认领");
    {
        PanelMenuGate g;
        CHECK(!g.pending(5'000'000));
        g.note(42, 1'000'000);
        CHECK(g.pending(1'000'000));
        CHECK(g.pending(2'499'999));
        CHECK(!g.pending(2'500'000)); // 过了窗口
        CHECK(g.pending(1'200'000));
        CHECK_EQ(g.take(), 42u);
        CHECK(!g.pending(1'200'001)); // 一次按下只认一个菜单
        g.note(7, 3'000'000);
        g.clear(); // X11 候选右键等本端菜单请求
        CHECK(!g.pending(3'100'000));
    }

    CASE("PanelMenuGate：服务端收起认领到的菜单后再认领一次（「更多…」→ 主菜单），同一输出");
    {
        PanelMenuGate g;
        g.note(42, 1'000'000);
        CHECK_EQ(g.take(), 42u);
        g.serverClosed(5'000'000); // 菜单开了几秒后点「更多…」
        CHECK(g.pending(5'000'000));
        CHECK(g.pending(6'499'999));
        CHECK(!g.pending(6'500'000));
        CHECK_EQ(g.take(), 42u);
        g.serverClosed(7'000'000); // 主菜单里点了普通项：仍重开窗口，只是不会有菜单帧来
        CHECK(g.pending(7'000'000));
        g.clear();
        g.serverClosed(8'000'000); // 收的是本端请求的菜单（clear 过）：不认领
        CHECK(!g.pending(8'000'000));
        g.serverClosed(9'000'000); // 收过一次就不再续
        CHECK(!g.pending(9'000'000));
    }

    CASE("effectiveFullscreen：Wayland 会话（layer-shell 面板）里一律不全屏，纯 X11 照报");
    CHECK(effectiveFullscreen(true, false));
    CHECK(!effectiveFullscreen(false, false));
    CHECK(!effectiveFullscreen(true, true));
    CHECK(!effectiveFullscreen(false, true));

    TEST_MAIN_END();
}
