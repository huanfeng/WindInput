// SettingsLauncher：`settings.open` body 解析与 argv 组装（程序路径不受信封内容影响）。
#include "SettingsLauncher.h"
#include "TestHarness.h"

#include <cstdlib>

// 设置程序的默认路径是编译期宏（打包到非 /usr 前缀时由构建系统注入）。断言按**可失败的性质**
// 写，别跟宏逐字比：`argv[0]` 与宏同源，`CHECK_EQ(argv[0], WIND_SETTING_PATH)` 恒真。
// 性质 = 绝对路径 + 末段是 wind_setting —— 注入 store 路径后仍成立，而指向服务、写成相对路径、
// 丢了文件名都会失败。
#ifdef WIND_SETTING_PATH
#define WIND_SETTING_PATH_INJECTED 1
#else
#define WIND_SETTING_PATH_INJECTED 0
#endif

using namespace windlinux;

using Args = std::vector<std::string>;

int main()
{
    CASE("服务端的典型 body：页 + 附加参数，顺序保留");
    auto a = parseSettingsOpenArgs(R"({"args":["--page=dict","--schema=wubi86"]})");
    CHECK(a.has_value());
    CHECK_EQ(*a, (Args{"--page=dict", "--schema=wubi86"}));

    CASE("空数组 / 缺 args 键 = 打开默认页");
    CHECK_EQ(*parseSettingsOpenArgs(R"({"args":[]})"), Args{});
    CHECK_EQ(*parseSettingsOpenArgs(R"({})"), Args{});
    CHECK_EQ(*parseSettingsOpenArgs(R"( { "future" : {"x":[1,true,null]} , "args" : [ "--page=ui" ] } )"),
             (Args{"--page=ui"}));

    CASE("含空白与中文的值仍是一个参数（加词页 --text=你 好）");
    CHECK_EQ(*parseSettingsOpenArgs(R"({"args":["--page=add-word","--text=你 好"]})"),
             (Args{"--page=add-word", "--text=你 好"}));

    CASE("JSON 转义全解：引号、反斜杠、\\u 与代理对");
    CHECK_EQ(*parseSettingsOpenArgs(R"({"args":["a\"b\\c","你好","😀","x\ty"]})"),
             (Args{"a\"b\\c", "你好", "\xF0\x9F\x98\x80", "x\ty"}));

    CASE("畸形或越界：整条作废，不带着残缺参数启动");
    CHECK(!parseSettingsOpenArgs(""));
    CHECK(!parseSettingsOpenArgs("[]"));
    CHECK(!parseSettingsOpenArgs(R"({"args":"--page=ui"})"));
    CHECK(!parseSettingsOpenArgs(R"({"args":["--page=ui",1]})"));
    CHECK(!parseSettingsOpenArgs(R"({"args":["--page=ui")"));
    CHECK(!parseSettingsOpenArgs(R"({"args":["a\u0000b"]})"));
    CHECK(!parseSettingsOpenArgs(R"({"args":["\ud83d"]})"));
    CHECK(!parseSettingsOpenArgs(R"({"args":[]} trailing)"));
    std::string many = R"({"args":[)";
    for (int i = 0; i < 65; ++i) {
        many += std::string(i ? "," : "") + "\"--x\"";
    }
    CHECK(!parseSettingsOpenArgs(many + "]}"));

    CASE("argv[0] 恒为设置程序路径；信封内容只进参数位");
    unsetenv("WIND_INPUT_SETTING");
    auto argv = settingsArgv({"/bin/sh", "-c", "evil"});
    const std::string prog = argv[0];
    CHECK(!prog.empty() && prog[0] == '/');
    const std::string tail = "/wind_setting";
    CHECK(prog.size() >= tail.size() &&
          prog.compare(prog.size() - tail.size(), tail.size(), tail) == 0);
#if !WIND_SETTING_PATH_INJECTED
    // 没注入 = deb / 源码构建：默认值必须是发行布局里那一份（打包脚本按这个路径安装）。
    CHECK_EQ(prog, std::string("/usr/lib/windinput/wind_setting"));
#endif
    CHECK_EQ(argv.size(), size_t(4));
    CHECK_EQ(argv[1], std::string("/bin/sh"));

    CASE("环境变量覆盖设置程序路径");
    setenv("WIND_INPUT_SETTING", "/opt/x/wind_setting", 1);
    CHECK_EQ(settingsArgv({})[0], std::string("/opt/x/wind_setting"));
    unsetenv("WIND_INPUT_SETTING");

    TEST_MAIN_END();
}
