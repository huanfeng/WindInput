// 交给 Fcitx5 呈现的宿主 UI 纯逻辑：中英模式镜像（托盘图标）与应用内预编辑的占位过滤。

#include "Codec.h"
#include "HostUi.h"
#include "KeyMap.h"
#include "Protocol.h"
#include "TestHarness.h"

#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <vector>

#include <unistd.h>

using namespace windlinux;

namespace {

void putU32(Bytes& b, uint32_t v)
{
    uint8_t x[4];
    std::memcpy(x, &v, 4);
    b.insert(b.end(), x, x + 4);
}

Frame flagsFrame(uint16_t cmd, uint32_t flags)
{
    Frame f;
    f.cmd = cmd;
    putU32(f.payload, flags);
    // STATUS_UPDATE 的真实载荷在 flags 之后还有热键计数与标签，模式只看开头。
    putU32(f.payload, 0);
    putU32(f.payload, 0);
    return f;
}

/// 完整状态帧（STATUS_UPDATE / STATE_PUSH / ACTIVATION_STATUS_PUSH 的真实布局）：
/// flags + keyDown 数 + keyUp 数 + 哈希 + 尾部标签。
Frame statusFrame(uint16_t cmd, uint32_t flags, const std::string& label, uint32_t hashes = 0)
{
    Frame f;
    f.cmd = cmd;
    putU32(f.payload, flags);
    putU32(f.payload, hashes);
    putU32(f.payload, 0);
    for (uint32_t i = 0; i < hashes; ++i) {
        putU32(f.payload, 0x10000u + i);
    }
    f.payload.insert(f.payload.end(), label.begin(), label.end());
    return f;
}

/// MODE_PUSH 的真实布局：只有 4 字节 flags。
Frame modePush(uint32_t flags)
{
    Frame f;
    f.cmd = CMD_MODE_PUSH;
    putU32(f.payload, flags);
    return f;
}

Frame commitFrame(uint32_t flags, const std::string& text)
{
    Frame f;
    f.cmd = CMD_COMMIT_TEXT;
    putU32(f.payload, flags);
    putU32(f.payload, uint32_t(text.size()));
    putU32(f.payload, 0);
    f.payload.insert(f.payload.end(), text.begin(), text.end());
    return f;
}

std::string readFile(const std::string& path)
{
    std::ifstream in(path);
    std::stringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

} // namespace

int main()
{
    CASE("四种状态帧都以 flags 开头，按 STATUS_CHINESE_MODE 取模式");
    for (uint16_t cmd : {CMD_MODE_PUSH, CMD_STATUS_UPDATE, CMD_STATE_PUSH, CMD_ACTIVATION_STATUS_PUSH}) {
        CHECK(modeStatusOf(flagsFrame(cmd, STATUS_CHINESE_MODE | STATUS_FULL_WIDTH))->chinese == true);
        CHECK(modeStatusOf(flagsFrame(cmd, STATUS_FULL_WIDTH))->chinese == false);
    }

    CASE("完整状态帧带大写锁定位与标签（跳过热键哈希取尾部）；MODE_PUSH 两样都不带");
    for (uint16_t cmd : {CMD_STATUS_UPDATE, CMD_STATE_PUSH, CMD_ACTIVATION_STATUS_PUSH}) {
        auto st = modeStatusOf(statusFrame(cmd, STATUS_CHINESE_MODE | STATUS_CAPS_LOCK, "A", 3));
        CHECK(st.has_value());
        CHECK(st->chinese);
        CHECK(st->capsLock == true);
        CHECK(st->label == std::string("A"));
        auto st2 = modeStatusOf(statusFrame(cmd, STATUS_CHINESE_MODE, "拼"));
        CHECK(st2->capsLock == false);
        CHECK(st2->label == std::string("拼"));
    }
    {
        auto st = modeStatusOf(modePush(STATUS_CHINESE_MODE | STATUS_CAPS_LOCK));
        CHECK(st.has_value());
        CHECK(!st->capsLock.has_value()); // 没有这一位，不能当「没开」
        CHECK(!st->label.has_value());
    }

    CASE("哈希计数超出载荷的坏帧：只认模式，不认大写锁定与标签");
    {
        Frame f = statusFrame(CMD_STATUS_UPDATE, STATUS_CAPS_LOCK, "A");
        uint32_t bogus = 1000;
        std::memcpy(f.payload.data() + 4, &bogus, 4);
        auto st = modeStatusOf(f);
        CHECK(st.has_value());
        CHECK(!st->capsLock.has_value());
        CHECK(!st->label.has_value());
    }

    CASE("载荷不足 4 字节的状态帧不算数（不当英文）");
    {
        Frame f;
        f.cmd = CMD_STATE_PUSH; // push_unix 的测试就发过零载荷的 STATE_PUSH
        CHECK(!modeStatusOf(f).has_value());
    }

    CASE("上屏帧只有带 MODE_CHANGED 时才携带模式");
    CHECK(!modeStatusOf(commitFrame(COMMIT_FLAG_CHINESE_MODE, "你好")).has_value());
    CHECK(modeStatusOf(commitFrame(COMMIT_FLAG_MODE_CHANGED, "Hello"))->chinese == false);
    CHECK(modeStatusOf(commitFrame(COMMIT_FLAG_MODE_CHANGED | COMMIT_FLAG_CHINESE_MODE, ""))->chinese == true);

    CASE("与模式无关的帧不动镜像");
    {
        Frame f;
        f.cmd = CMD_UPDATE_COMPOSITION;
        putU32(f.payload, STATUS_CHINESE_MODE); // 碰巧同值的 caretPos 不能被当成 flags
        CHECK(!modeStatusOf(f).has_value());
    }

    CASE("镜像：未知时不给图标（退回条目图标），学到后只在变化时报变");
    {
        ModeIndicator m;
        m.setIconRoot(""); // 不看真实用户目录：只验种子回落
        CHECK(!m.known());
        CHECK_EQ(m.iconName(), std::string());
        CHECK_EQ(m.label(), std::string());
        CHECK(m.update(modePush(STATUS_CHINESE_MODE)));
        CHECK_EQ(m.iconName(), std::string("windinput-zh"));
        CHECK_EQ(m.label(), std::string("中"));
        CHECK_EQ(m.subModeName(), std::string("中文"));
        CHECK(!m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE, "中"))); // 没变
        CHECK(!m.update(commitFrame(0, "你好")));                                   // 不带模式
        CHECK(m.update(statusFrame(CMD_STATE_PUSH, 0, "英")));
        CHECK_EQ(m.iconName(), std::string("windinput-en"));
        CHECK_EQ(m.label(), std::string("英"));
        CHECK_EQ(m.subModeName(), std::string("英文"));
    }

    CASE("大写锁定：无论中英都是「A」（对齐 Windows effective_chinese），关掉回到原态");
    {
        ModeIndicator m;
        m.setIconRoot(""); // 不看真实用户目录：只验种子回落
        m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE, "中"));
        CHECK(m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE | STATUS_CAPS_LOCK, "A")));
        CHECK_EQ(m.iconName(), std::string("windinput-caps"));
        CHECK_EQ(m.label(), std::string("A"));
        CHECK_EQ(m.subModeName(), std::string("大写锁定"));
        CHECK(m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE, "中")));
        CHECK_EQ(m.iconName(), std::string("windinput-zh"));

        m.update(statusFrame(CMD_STATUS_UPDATE, 0, "英"));
        CHECK(m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CAPS_LOCK, "A")));
        CHECK_EQ(m.iconName(), std::string("windinput-caps"));
        CHECK(m.update(statusFrame(CMD_STATUS_UPDATE, 0, "英")));
        CHECK_EQ(m.iconName(), std::string("windinput-en"));
    }

    CASE("焦点进入的 MODE_PUSH 不带大写锁定位：沿用已知状态，不回退");
    {
        ModeIndicator m;
        m.setIconRoot(""); // 不看真实用户目录：只验种子回落
        m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE | STATUS_CAPS_LOCK, "A"));
        CHECK(!m.update(modePush(STATUS_CHINESE_MODE)));
        CHECK_EQ(m.iconName(), std::string("windinput-caps"));
        // 换到一个英文态的 IC：中英变了，但大写锁定是全局的，图标仍是「A」。
        CHECK(!m.update(modePush(0)));
        CHECK_EQ(m.iconName(), std::string("windinput-caps"));
        CHECK(!m.chinese());
    }

    CASE("本端大写锁定判定：立即换图标；关掉后方案标签不丢");
    {
        ModeIndicator u;
        u.setIconRoot(""); // 不看真实用户目录：只验种子回落
        CHECK(!u.noteCapsLock(true)); // 模式未知：不给图标，也不报变
        u.update(modePush(STATUS_CHINESE_MODE));
        CHECK_EQ(u.iconName(), std::string("windinput-caps")); // 刚才那次判定记着

        ModeIndicator m;

        m.setIconRoot(""); // 不看真实用户目录：只验种子回落
        m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE, "拼"));
        CHECK_EQ(m.iconName(), std::string("windinput-zh"));
        CHECK(m.noteCapsLock(true));
        CHECK_EQ(m.iconName(), std::string("windinput-caps"));
        CHECK_EQ(m.label(), std::string("A"));
        CHECK(!m.noteCapsLock(true));
        // 大写锁定态下服务端发来的是 caps 标签，不能覆盖方案标签。
        m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE | STATUS_CAPS_LOCK, "A"));
        CHECK(m.noteCapsLock(false));
        CHECK_EQ(m.iconName(), std::string("windinput-zh"));
        CHECK_EQ(m.label(), std::string("拼"));
    }

    CASE("运行时图标名：状态段 + 主字 UTF-8 十六进制（与 wind_ui::tray_icon::icon_name 同一组样例）");
    {
        using V = ModeIndicator::Variant;
        CHECK_EQ(ModeIndicator::dynamicIconName(V::Chinese, "拼"), std::string("windinput-lbl-zh-e68bbc"));
        CHECK_EQ(ModeIndicator::dynamicIconName(V::English, "英"), std::string("windinput-lbl-en-e88bb1"));
        CHECK_EQ(ModeIndicator::dynamicIconName(V::Caps, "A"), std::string("windinput-lbl-caps-41"));
        CHECK_EQ(ModeIndicator::dynamicIconName(V::English, "En"), std::string("windinput-lbl-en-456e"));
    }

    CASE("运行时图标名分变体、超长标签走散列；样例与 tray_icon.rs 逐字相同（读源码核对）");
    {
        using V = ModeIndicator::Variant;
        const std::string devPin = ModeIndicator::dynamicIconName(V::Chinese, "拼", true);
        const std::string relPin = ModeIndicator::dynamicIconName(V::Chinese, "拼", false);
        std::string longLabel;
        for (int i = 0; i < 40; ++i) {
            longLabel += "虎";
        }
        const std::string hashed = ModeIndicator::dynamicIconName(V::Chinese, longLabel, false);
        CHECK_EQ(devPin, std::string("windinput-lbl-dev-zh-e68bbc"));
        CHECK_EQ(relPin, std::string("windinput-lbl-zh-e68bbc"));
        CHECK_EQ(hashed, std::string("windinput-lbl-zh-h8b27ed0fc2ac336d"));
        CHECK_EQ(ModeIndicator::dynamicIconName(V::Caps, std::string(32, 'a'), false),
                 "windinput-lbl-caps-" + [] {
                     std::string h;
                     for (int i = 0; i < 32; ++i) {
                         h += "61";
                     }
                     return h;
                 }());
        CHECK(ModeIndicator::dynamicIconName(V::Caps, std::string(33, 'a'), false)
                  .rfind("windinput-lbl-caps-h", 0) == 0);
        std::ifstream in(std::string(WIND_REPO_DIR) + "/wind_input/crates/wind-ui/src/tray_icon.rs");
        std::stringstream ss;
        ss << in.rdbuf();
        const std::string rs = ss.str();
        for (const std::string& sample : {devPin, relPin, hashed}) {
            if (rs.find("\"" + sample + "\"") == std::string::npos) {
                std::printf("  tray_icon.rs 里没有样例 %s\n", sample.c_str());
                CHECK(false);
            }
        }
        // 缺省按本 addon 的变体：WIND_VARIANT=dev 时带 dev 段。
        setenv("WIND_VARIANT", "dev", 1);
        CHECK_EQ(ModeIndicator::dynamicIconName(V::Chinese, "拼"), devPin);
        setenv("WIND_VARIANT", "release", 1);
        CHECK_EQ(ModeIndicator::dynamicIconName(V::Chinese, "拼"), relPin);
        unsetenv("WIND_VARIANT");
    }

    CASE("运行时图标在（64px 那张写完）就用它，否则退回种子；文字标签如实");
    {
        namespace fs = std::filesystem;
        fs::path root = fs::temp_directory_path() / ("wi-host-ui-" + std::to_string(::getpid()));
        fs::remove_all(root);
        auto touch = [&](const std::string& name, const char* size) {
            fs::create_directories(root / size / "apps");
            std::ofstream(root / size / "apps" / (name + ".png")) << "png";
        };
        ModeIndicator m;
        m.setIconRoot(root.string());
        m.update(statusFrame(CMD_STATUS_UPDATE, STATUS_CHINESE_MODE, "虎"));
        CHECK_EQ(m.iconName(), std::string("windinput-zh")); // 还没渲染：种子
        CHECK_EQ(m.label(), std::string("虎"));
        touch("windinput-lbl-zh-e8998e", "16x16");
        CHECK_EQ(m.iconName(), std::string("windinput-zh")); // 只写了一部分：仍是种子
        touch("windinput-lbl-zh-e8998e", "64x64");
        CHECK_EQ(m.iconName(), std::string("windinput-lbl-zh-e8998e"));
        // 切方案：标签变、中英不变，图标名跟着变，要报变。
        touch("windinput-lbl-zh-e4ba94", "64x64");
        CHECK(m.update(statusFrame(CMD_STATE_PUSH, STATUS_CHINESE_MODE, "五")));
        CHECK_EQ(m.iconName(), std::string("windinput-lbl-zh-e4ba94"));
        // 本端判定大写锁定：caps 标签那组在就用它（服务端每次构建状态都会把 caps / 英文两组一起备好）。
        touch("windinput-lbl-caps-41", "64x64");
        CHECK(m.noteCapsLock(true));
        CHECK_EQ(m.iconName(), std::string("windinput-lbl-caps-41"));
        // [ui.labels] 改过的英文标签：图标与文字都跟配置。
        m.noteCapsLock(false);
        touch("windinput-lbl-en-456e", "64x64");
        m.update(statusFrame(CMD_STATUS_UPDATE, 0, "En"));
        CHECK_EQ(m.iconName(), std::string("windinput-lbl-en-456e"));
        CHECK_EQ(m.label(), std::string("En"));
        fs::remove_all(root);
    }

    CASE("预建图标目录：六个尺寸档的 apps 都建出来；空根失败");
    {
        namespace fs = std::filesystem;
        fs::path root = fs::temp_directory_path() / ("wi-host-dirs-" + std::to_string(::getpid()));
        fs::remove_all(root);
        CHECK(ensureIconDirs((root / "icons/hicolor").string()));
        for (const char* n : {"16x16", "22x22", "24x24", "32x32", "48x48", "64x64"}) {
            CHECK(fs::is_directory(root / "icons/hicolor" / n / "apps"));
        }
        CHECK(ensureIconDirs((root / "icons/hicolor").string())); // 已存在也算成功
        CHECK(!ensureIconDirs(""));
        fs::remove_all(root);
    }

    CASE("用户图标目录：XDG_DATA_HOME 为绝对路径时用它，否则 HOME/.local/share");
    {
        ::setenv("XDG_DATA_HOME", "/x/data", 1);
        CHECK_EQ(userHicolorDir(), std::string("/x/data/icons/hicolor"));
        ::setenv("XDG_DATA_HOME", "relative", 1);
        ::setenv("HOME", "/home/u", 1);
        CHECK_EQ(userHicolorDir(), std::string("/home/u/.local/share/icons/hicolor"));
        ::unsetenv("XDG_DATA_HOME");
        CHECK_EQ(userHicolorDir(), std::string("/home/u/.local/share/icons/hicolor"));
    }

    CASE("种子图标：16…64 与 scalable 都随包");
    {
        std::string rs = WIND_PROTOCOL_RS;
        const std::string ipc = "wind_input/crates/wind-ipc/src/protocol.rs";
        std::string root = rs.substr(0, rs.rfind(ipc)) + "wind_linux/data/icons/hicolor/";
        using V = ModeIndicator::Variant;
        for (V v : {V::Chinese, V::English, V::Caps}) {
            const std::string name = ModeIndicator::seedIconName(v);
            for (const char* sz : {"16x16", "22x22", "24x24", "32x32", "48x48", "64x64"}) {
                std::string path = root + sz + "/apps/" + name + ".png";
                if (!std::filesystem::exists(path)) {
                    std::printf("缺图标 %s\n", path.c_str());
                }
                CHECK(std::filesystem::exists(path));
            }
            CHECK(std::filesystem::exists(root + "scalable/apps/" + name + ".svg"));
        }
    }

    CASE("CapsLock 判定：X11 事件的 state 是事件之前的（关的那次松开仍带 Lock）");
    {
        constexpr uint32_t Caps = 0xffe5;
        CapsLockTracker t;
        // 开：按下 state=0，松开 state=Lock（xev 实测）。
        CHECK(!t.onKey(Caps, 0, false).has_value());
        CHECK(t.onKey(Caps, xstate::CapsLock, true) == true);
        // 关：按下 state=Lock，松开 state 仍是 Lock。只看松开的 Lock 位会判成「还开着」。
        CHECK(!t.onKey(Caps, xstate::CapsLock, false).has_value());
        CHECK(t.onKey(Caps, xstate::CapsLock, true) == false);
        // DBus 客户端若在松开时报事件之后的状态（关 → 松开 state=0），结论一样。
        CHECK(!t.onKey(Caps, 0, false).has_value());
        CHECK(t.onKey(Caps, 0, true) == true);
    }
    {
        constexpr uint32_t Caps = 0xffe5;
        CapsLockTracker t;
        CASE("CapsLock 判定：别的键的 Lock 位就是当前锁定态（在别处切过大写，第一个键就校准）");
        CHECK(t.onKey('a', xstate::CapsLock, false) == true);
        CHECK(t.onKey('a', xstate::CapsLock | xstate::Shift, true) == true);
        CHECK(t.onKey('a', 0, false) == false);
        CHECK(t.onKey(0xffe1 /* Shift_L */, xstate::NumLock, true) == false);

        CASE("CapsLock 判定：没见到按下的松开判不了；焦点切换作废半截的按下");
        CHECK(!t.onKey(Caps, xstate::CapsLock, true).has_value());
        CHECK(!t.onKey(Caps, 0, false).has_value());
        t.reset();
        CHECK(!t.onKey(Caps, xstate::CapsLock, true).has_value());

        CASE("CapsLock 判定：按住时的自动重复不覆盖「按下之前」的状态");
        CHECK(!t.onKey(Caps, 0, false).has_value());
        CHECK(!t.onKey(Caps, xstate::CapsLock, false).has_value()); // 重复，已上锁
        CHECK(t.onKey(Caps, xstate::CapsLock, true) == true);
    }

    CASE("预编辑：单个空格的占位组合不显示，其余原样");
    CHECK_EQ(clientPreeditText(" "), std::string());
    CHECK_EQ(clientPreeditText("ni'hao"), std::string("ni'hao"));
    CHECK_EQ(clientPreeditText(""), std::string());
    CHECK_EQ(clientPreeditText("  "), std::string("  ")); // 不是占位：只认服务端那一个常量
    CHECK_EQ(clientPreeditText("hello world"), std::string("hello world"));

    CASE("占位常量与服务端 COMPOSITION_PLACEHOLDER 同值（改了那边这里当场红）");
    {
        std::string rs = WIND_PROTOCOL_RS;
        const std::string ipc = "wind-ipc/src/protocol.rs";
        rs.replace(rs.rfind(ipc), ipc.size(), "wind-bridge/src/handler.rs");
        std::string src = readFile(rs);
        CHECK(!src.empty());
        CHECK(src.find("pub const COMPOSITION_PLACEHOLDER: &str = \" \";") != std::string::npos);
    }

    CASE("kdeglobals：只认 [KDE] 组的 AnimationDurationFactor，带修饰的键、空格、注释都照认");
    CHECK(!kdeAnimationFactor("").has_value());
    CHECK(!kdeAnimationFactor("[General]\nAnimationDurationFactor=0\n").has_value());
    CHECK(kdeAnimationFactor("[General]\nx=1\n[KDE]\nAnimationDurationFactor=0\n") == 0.0);
    CHECK(kdeAnimationFactor("[KDE]\n# c\n AnimationDurationFactor = 0.5\r\n") == 0.5);
    CHECK(kdeAnimationFactor("[KDE]\nAnimationDurationFactor[$i]=0\n") == 0.0);
    CHECK(!kdeAnimationFactor("[KDE]\nAnimationDurationFactor=fast\n").has_value());
    CHECK(!kdeAnimationFactor("[KDE]\nAnimationDurationFactor=0x\n").has_value());
    CHECK(kdeAnimationFactor("[KDE]\nAnimationDurationFactor=1\nAnimationDurationFactor=0\n") == 0.0);
    CHECK(!kdeAnimationFactor("[KDE][Other]\nAnimationDurationFactor=0\n").has_value());
    CHECK(kdeAnimationFactor("[KDE][$i]\nAnimationDurationFactor=0\n") == 0.0); // 整组锁定
    CHECK(kdeAnimationFactor("[KDE][$i][$e]\nAnimationDurationFactor=0\n") == 0.0);
    CHECK(!kdeAnimationFactor("[KDE][$i]x\nAnimationDurationFactor=0\n").has_value());
    CHECK(kdeAnimationFactor("\xEF\xBB\xBF[KDE]\nAnimationDurationFactor=0\n") == 0.0); // BOM

    CASE("减少动态效果：reduced-motion 优先；其余按桌面挑 KDE / GNOME，都没有不报");
    {
        MotionSources s;
        CHECK(!pickReduceMotion(s, "GNOME").has_value());
        s.gnomeAnimations = 0;
        CHECK(pickReduceMotion(s, "GNOME") == true);
        CHECK(pickReduceMotion(s, "KDE") == true); // KDE 会话没有 KDE 的值：退回 GNOME 的
        s.kdeFileFactor = 1;
        CHECK(pickReduceMotion(s, "KDE") == false);      // KDE 会话先看 KDE
        CHECK(pickReduceMotion(s, "ubuntu:GNOME") == true); // GNOME 会话先看 GNOME
        s.kdePortalFactor = 0;
        CHECK(pickReduceMotion(s, "KDE") == true); // 门户优先于文件
        CHECK(pickReduceMotion(s, "X-Cinnamon:KDE") == true);
        CHECK(pickReduceMotion(s, "KDEX") == true); // 不是 KDE：看 GNOME（同样是关）
        s.kdePortalFactor = 1;
        CHECK(pickReduceMotion(s, "kde") == false);       // 桌面名不分大小写
        CHECK(pickReduceMotion(s, "ubuntu:Kde") == false);
        s.kdePortalFactor = 0;
        s.gnomeAnimations = 1;
        CHECK(pickReduceMotion(s, "KDEX") == false);
        s.reducedMotion = 0;
        CHECK(pickReduceMotion(s, "KDE") == false); // 跨桌面的新键说了算
        s.reducedMotion = 1;
        CHECK(pickReduceMotion(s, "GNOME") == true);
        MotionSources k;
        k.kdeFileFactor = 0;
        CHECK(pickReduceMotion(k, "") == true); // 别的桌面只有 KDE 的值：用它
        k.kdeFileFactor = 0.25;
        CHECK(pickReduceMotion(k, "") == false);
    }

    TEST_MAIN_END();
}
