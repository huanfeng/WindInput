// ResponseRouter / DigitTracker / Utf 单测。用例语义对齐 wind_macos 的
// BridgeResponseRouterTests（同一套待定标点 / 定格前缀规则），宿主换成记录操作的桩。

#include "ExtProtocol.h"
#include "Protocol.h"
#include "ResponseRouter.h"
#include "TestHarness.h"
#include "Utf.h"

#include <functional>
#include <string>
#include <vector>

using namespace windlinux;

namespace {

struct FakeSink : TextSink {
    std::vector<std::string> ops;
    std::string preedit;
    size_t caret = 0;
    std::string committed;
    bool surrounding = true;

    void commitText(const std::string& t) override
    {
        ops.push_back("commit:" + t);
        committed += t;
    }
    void setPreedit(const std::string& t, size_t c) override
    {
        ops.push_back("preedit:" + t);
        preedit = t;
        caret = c;
    }
    bool deleteBeforeCursor(size_t n) override
    {
        if (!surrounding) {
            return false;
        }
        ops.push_back("delete:" + std::to_string(n));
        return true;
    }
    void moveCursor(int n) override { ops.push_back("move:" + std::to_string(n)); }
};

Bytes le32(uint32_t v)
{
    return {uint8_t(v), uint8_t(v >> 8), uint8_t(v >> 16), uint8_t(v >> 24)};
}

void put(Bytes& b, uint32_t v)
{
    Bytes x = le32(v);
    b.insert(b.end(), x.begin(), x.end());
}

void put(Bytes& b, const std::string& s)
{
    b.insert(b.end(), s.begin(), s.end());
}

Frame mk(uint16_t cmd, Bytes p = {})
{
    Frame f;
    f.cmd = cmd;
    f.payload = std::move(p);
    return f;
}

Frame commit(const std::string& text, uint32_t flags = 0, const std::string& comp = "")
{
    Bytes p;
    put(p, flags);
    put(p, uint32_t(text.size()));
    put(p, uint32_t(comp.size()));
    put(p, text);
    put(p, comp);
    return mk(CMD_COMMIT_TEXT, p);
}

Frame update(const std::string& text, uint32_t caret)
{
    Bytes p;
    put(p, caret);
    put(p, text);
    return mk(CMD_UPDATE_COMPOSITION, p);
}

Frame hold(const std::string& text, uint32_t ms)
{
    Bytes p;
    put(p, ms);
    put(p, uint32_t(text.size()));
    put(p, text);
    return mk(CMD_HOLD_COMPOSITION, p);
}

Frame commitAndHold(uint16_t cmd, const std::string& c, const std::string& h)
{
    Bytes p;
    put(p, 0u);
    put(p, uint32_t(c.size()));
    put(p, uint32_t(h.size()));
    put(p, c);
    put(p, h);
    return mk(cmd, p);
}

void TestBasicFlow()
{
    CASE("组字 → 上屏：预编辑随 UpdateComposition 走，CommitText 先上屏再清预编辑");
    ResponseRouter r;
    FakeSink s;
    CHECK(r.apply(update("ni", 2), &s));
    CHECK(s.preedit == "ni");
    CHECK_EQ(s.caret, size_t(2));
    CHECK(r.hasComposition());
    CHECK(r.apply(commit("你好"), &s));
    CHECK(s.committed == "你好");
    CHECK(s.preedit.empty());
    CHECK(!r.hasComposition());
    // 顺序：先上屏再清（Fcitx5 的 commitString 不会顺带清 client preedit；反过来 Electron 冷启动会丢字）
    CHECK(s.ops.size() == 3 && s.ops[1] == "commit:你好" && s.ops[2] == "preedit:");
}

void TestCommitBeforeClearingPreedit()
{
    CASE("凡是上屏顺带收组合的路径，都先 commit 后清预编辑");
    {
        ResponseRouter r;
        FakeSink s;
        r.apply(update("kh", 2), &s);
        Bytes w;
        put(w, uint32_t(std::string("（）").size()));
        put(w, 1u);
        put(w, std::string("（）"));
        r.apply(mk(CMD_COMMIT_TEXT_WITH_CURSOR, w), &s);
        CHECK(s.ops.size() == 4 && s.ops[1] == "commit:（）" && s.ops[2] == "preedit:"
              && s.ops[3] == "move:-1");
    }
    {
        ResponseRouter r;
        FakeSink s;
        r.apply(hold("、", 0), &s);
        r.applyClearComposition(&s);
        CHECK(s.ops.size() == 3 && s.ops[1] == "commit:、" && s.ops[2] == "preedit:");
    }
    {
        ResponseRouter r;
        FakeSink s;
        r.apply(hold("。", 0), &s);
        r.apply(mk(CMD_PASS_THROUGH), &s);
        CHECK(s.ops.size() == 3 && s.ops[1] == "commit:。" && s.ops[2] == "preedit:");
    }
    {
        ResponseRouter r;
        FakeSink s;
        r.apply(update("gg", 2), &s);
        r.apply(commitAndHold(CMD_COMMIT_THEN_DEFER, "工", "a"), &s);
        CHECK(s.ops.size() == 4 && s.ops[1] == "commit:工" && s.ops[2] == "preedit:"
              && s.ops[3] == "preedit:a");
    }
}

void TestCaretIsUtf16ToBytes()
{
    CASE("组合内光标：服务端 UTF-16 码元 → Fcitx5 UTF-8 字节");
    ResponseRouter r;
    FakeSink s;
    r.apply(update("你好a", 2), &s); // 光标在「好」之后
    CHECK_EQ(s.caret, size_t(6));
    r.apply(update("𠀀x", 2), &s); // 扩展 B 区：1 个字 = 2 个 UTF-16 码元 = 4 字节
    CHECK_EQ(s.caret, size_t(4));
    r.apply(update("ab", 99), &s); // 越界钳到串尾
    CHECK_EQ(s.caret, size_t(2));
}

void TestPassThroughAndClear()
{
    CASE("PassThrough → false；ClearComposition 遇宿主快捷键 → false；ClearThenPassThrough → false");
    ResponseRouter r;
    FakeSink s;
    CHECK(!r.apply(mk(CMD_PASS_THROUGH), &s));
    r.apply(update("n", 1), &s);
    CHECK(r.apply(mk(CMD_CLEAR_COMPOSITION), &s));
    CHECK(s.preedit.empty());
    r.apply(update("n", 1), &s);
    CHECK(!r.apply(mk(CMD_CLEAR_COMPOSITION), &s, /*hostShortcut=*/true)); // Ctrl+C 不被吞
    r.apply(update("n", 1), &s);
    CHECK(!r.apply(mk(CMD_CLEAR_THEN_PASS_THROUGH), &s));
    CHECK(s.preedit.empty());
    CHECK(r.apply(mk(CMD_CONSUMED), &s));
    CHECK(r.apply(mk(CMD_STATUS_UPDATE), &s)); // 未专门处理的 cmd：默认消费
}

void TestHeldSymbol()
{
    CASE("待定标点 + 新组字：并进同一段预编辑，不被覆盖（macOS 符号凭空消失那个坑）");
    ResponseRouter r;
    FakeSink s;
    r.apply(hold("，", 0), &s);
    CHECK(s.preedit == "，");
    r.apply(update("n", 1), &s);
    CHECK(s.preedit == "，n");
    CHECK_EQ(s.caret, std::string("，n").size()); // 光标右移前缀长度
    r.apply(commit("你"), &s);
    CHECK(s.committed == "，你"); // 前缀随本次上屏

    CASE("PassThrough 时待定标点先真上屏");
    ResponseRouter r2;
    FakeSink s2;
    r2.apply(hold("。", 0), &s2);
    CHECK(!r2.apply(mk(CMD_PASS_THROUGH), &s2));
    CHECK(s2.committed == "。");
    CHECK(s2.preedit.empty());

    CASE("replacingHeld：press2 丢弃待定标点");
    ResponseRouter r3;
    FakeSink s3;
    r3.apply(hold("，", 0), &s3);
    r3.apply(commit(",", COMMIT_FLAG_REPLACING_HELD), &s3);
    CHECK(s3.committed == ",");
    CHECK(s3.preedit.empty());

    CASE("失焦 ClearComposition：待定标点转为提交而非丢弃");
    ResponseRouter r4;
    FakeSink s4;
    r4.apply(hold("、", 0), &s4);
    r4.applyClearComposition(&s4);
    CHECK(s4.committed == "、");
}

void TestCommitAndHoldVsDefer()
{
    CASE("CommitThenDefer（码表顶码）：先上屏再开余码组合，余码不当待定标点");
    ResponseRouter r;
    FakeSink s;
    CHECK(r.apply(commitAndHold(CMD_COMMIT_THEN_DEFER, "工", "a"), &s));
    CHECK(s.committed == "工"); // ← macOS 早期漏接这一臂，顶码上屏的字被吞
    CHECK(s.preedit == "a");
    r.apply(update("ab", 2), &s);
    CHECK(s.preedit == "ab"); // 余码被新组合正常替换，不叠加

    CASE("CommitAndHold：holdText 是待定标点，下一次组字不能把它覆盖掉");
    ResponseRouter r2;
    FakeSink s2;
    r2.apply(commitAndHold(CMD_COMMIT_AND_HOLD, "你", "，"), &s2);
    CHECK(s2.committed == "你");
    r2.apply(update("h", 1), &s2);
    CHECK(s2.preedit == "，h");
}

void TestHoldTimer()
{
    CASE("hold 计时器：到点把待定标点定稿；期间有别的动作则作废");
    std::vector<std::function<void()>> timers;
    ResponseRouter r;
    FakeSink s;
    r.setTimerScheduler([&](uint32_t, std::function<void()> cb) { timers.push_back(cb); });
    r.setSinkProvider([&](const std::function<void(TextSink*)>& use) { use(&s); });
    r.apply(hold("。", 500), &s);
    CHECK_EQ(timers.size(), size_t(1));
    timers[0]();
    CHECK(s.committed == "。");
    CHECK(s.preedit.empty());

    FakeSink s2;
    r.setSinkProvider([&](const std::function<void(TextSink*)>& use) { use(&s2); });
    r.apply(hold("，", 500), &s2);
    r.apply(update("n", 1), &s2); // 定格进前缀 → 旧计时器作废
    timers.back()();
    CHECK(s2.committed.empty());
    CHECK(s2.preedit == "，n");
}

void TestReplaceBackwardAndCursor()
{
    CASE("ReplaceBackward：先删光标前 N 字再插入；宿主不支持删除时只插入不误删");
    ResponseRouter r;
    FakeSink s;
    Bytes p;
    put(p, 1u);
    put(p, 1u);
    put(p, std::string("."));
    r.apply(mk(CMD_REPLACE_BACKWARD, p), &s);
    CHECK(s.ops.size() == 2 && s.ops[0] == "delete:1" && s.ops[1] == "commit:.");
    FakeSink s2;
    s2.surrounding = false;
    r.apply(mk(CMD_REPLACE_BACKWARD, p), &s2);
    CHECK(s2.ops.size() == 1 && s2.ops[0] == "commit:.");

    CASE("CommitTextWithCursor：配对插入后光标左移；MoveCursor 右移");
    FakeSink s3;
    Bytes w;
    put(w, uint32_t(std::string("（）").size()));
    put(w, 1u);
    put(w, std::string("（）"));
    r.apply(mk(CMD_COMMIT_TEXT_WITH_CURSOR, w), &s3);
    CHECK(s3.committed == "（）");
    CHECK(s3.ops.back() == "move:-1");
    r.apply(mk(CMD_MOVE_CURSOR, le32(1)), &s3);
    CHECK(s3.ops.back() == "move:1");

    CASE("服务端给的计数巨大（> INT_MAX）：钳到上限再取负，不是未定义行为");
    FakeSink s5;
    Bytes big;
    put(big, 0xFFFFFFFFu);
    put(big, 1u);
    put(big, std::string("."));
    r.apply(mk(CMD_REPLACE_BACKWARD, big), &s5);
    CHECK(s5.ops.size() == 2 && s5.ops[0] == "delete:" + std::to_string(kMaxDeleteUnits));
    FakeSink s6;
    Bytes far;
    put(far, uint32_t(std::string("（）").size()));
    put(far, 0x80000000u);
    put(far, std::string("（）"));
    r.apply(mk(CMD_COMMIT_TEXT_WITH_CURSOR, far), &s6);
    CHECK(s6.ops.back() == "move:-" + std::to_string(kMaxCursorMove));

    CASE("KeyType：整段上屏");
    FakeSink s4;
    r.apply(mk(CMD_KEY_TYPE, Bytes{'o', 'k'}), &s4);
    CHECK(s4.committed == "ok");
}

void TestSeqTracker()
{
    CASE("seqChar 记账：上屏取末码点、透传报类别代表值、组合串不动");
    ResponseRouter r;
    FakeSink s;
    r.apply(commit("你好"), &s);
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t(0x597D));
    r.apply(update("wo", 2), &s);
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t(0x597D)); // 预编辑没写进文档
    r.apply(commit("\xF0\xA0\x80\x80"), &s);              // U+20000：取高代理
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t(0xD840));
    r.noteKeyPassthrough(0x48, 0);                          // H
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t('a'));
    r.noteKeyPassthrough(0x20, 0);
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t(' '));
    r.noteKeyPassthrough(0xBE, 0);                          // .
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t('?'));
    r.noteKeyPassthrough(0x56, KEYMOD_CTRL);                // Ctrl+V
    CHECK_EQ(r.seqTracker().seqChar(), SeqTracker::kUnknown);
    r.noteKeyPassthrough(0x37, 0);
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t('7'));  // 两份记账一起喂
    CHECK_EQ(r.seqTracker().seqChar(), uint16_t('7'));
    r.reset();
    CHECK_EQ(r.seqTracker().seqChar(), SeqTracker::kUnknown);
}

void TestSeqTrackerCursor()
{
    CASE("seqChar 记账：宿主报的光标与预期不符（鼠标挪过）就作废");
    ResponseRouter r;
    FakeSink s;
    auto& t = r.seqTracker();
    t.noteCursor(0);                 // 基准
    r.apply(commit("工工"), &s);     // 预期 2
    t.noteCursor(2);
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));
    r.noteKeyPassthrough(0x33, 0);   // 透传 '3'：预期 3
    t.noteCursor(3);
    CHECK_EQ(t.seqChar(), uint16_t('3'));
    t.noteCursor(0);                 // 点到行首
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);

    CASE("上屏时宿主先报「编码已清、新字未插」的中间态：当回声忽略，不误判");
    t.noteCursor(3);                 // 重新当基准：光标在 3
    r.apply(commit("工"), &s);       // 预期 4
    t.noteCursor(3);                 // 中间态回声
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));
    t.noteCursor(4);                 // 最终态
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));
    t.noteCursor(3);                 // 已收到最终态之后再报 3：是真被挪过
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);

    CASE("宿主还没报新位置就连打：预期累加，不误判");
    t.noteCursor(0);                 // 重新当基准
    r.apply(commit("你"), &s);       // 预期 1
    r.apply(commit("好"), &s);       // 预期 2
    t.noteCursor(2);
    CHECK_EQ(t.seqChar(), uint16_t(0x597D));

    CASE("删过字 / 导航键之后光标去向不明：下一次报的位置重新当基准，不判");
    r.noteKeyPassthrough(0x25, 0);   // ←
    r.apply(commit("工"), &s);
    t.noteCursor(9);
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));
    t.forgetCursor();
    t.noteCursor(4);
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));
    t.noteCursor(-1);                // 宿主给不出有效值
    t.noteCursor(7);
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));

    CASE("点回上屏前的位置：光标后紧跟刚上屏的字，不是回声");
    t.noteCursor(3, 'c', 0);         // abc|
    r.apply(commit("工"), &s);       // abc工|，预期 4
    t.noteCursor(3, 'c', 0x5DE5);    // 点到 c 与 工 之间
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);
    t.noteCursor(4, 0x5DE5, 0);
    r.apply(commit("工"), &s);       // 预期 5
    t.noteCursor(4, 0x5DE5, 0);      // 中间态回声：新字还没插进来
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));

    CASE("位置对得上但光标前的字不是记账那个（点到别的行同一列）：作废");
    t.noteCursor(5, 0x5DE5, 0);
    CHECK_EQ(t.seqChar(), uint16_t(0x5DE5));
    t.noteCursor(5, 'b', 'c');
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);
    t.noteCursor(0, 0, 0);
    r.apply(commit("中文"), &s);
    t.noteCursor(2, 0x6587, 0);
    CHECK_EQ(t.seqChar(), uint16_t(0x6587));
    t.noteCursor(2, 0, 'a');         // 点到行首（周边文本只给当前行）
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);

    CASE("透传键只记类别：光标前同类就认");
    t.noteCursor(2, 'x', 0);
    r.noteKeyPassthrough(0x42, 0);   // b → 代表值 'a'
    t.noteCursor(3, 'b', 0);
    CHECK_EQ(t.seqChar(), uint16_t('a'));
    r.noteKeyPassthrough(0xBC, 0);   // , → '?'
    t.noteCursor(4, ',', 0);
    CHECK_EQ(t.seqChar(), uint16_t('?'));
    r.noteKeyPassthrough(0x31, 0);   // 1
    t.noteCursor(5, '7', 0);         // 光标前不是 1
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);
    CASE("透传键没有中间态：回到写入前的位置就是被挪过");
    t.noteCursor(5, '1', 0);
    r.noteKeyPassthrough(0x32, 0);   // 2，预期 6
    t.noteCursor(5, '1', '2');
    CHECK_EQ(t.seqChar(), SeqTracker::kUnknown);
}

void TestDigitTracker()
{
    CASE("数字后智能标点记账：上屏数字记、汉字清零；透传键按产出记；快捷键不记");
    ResponseRouter r;
    FakeSink s;
    r.apply(commit("12"), &s);
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t('2'));
    r.apply(commit("你"), &s);
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t(0));
    r.digitTracker().noteKeyPassthrough(0x33, 0);
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t('3'));
    r.digitTracker().noteKeyPassthrough(0x33, KEYMOD_SHIFT); // '#'
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t(0));
    r.digitTracker().noteKeyPassthrough(0x65, 0); // VK_NUMPAD5
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t('5'));
    r.digitTracker().noteKeyPassthrough(0x32, KEYMOD_CTRL);
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t(0));
    r.apply(commit("7"), nullptr); // 没写进文档就不记账
    CHECK_EQ(r.digitTracker().prevChar(), uint16_t(0));

    CASE("数字后只认记账：宿主光标前的旧数字不算，非数字照报");
    DigitTracker t;
    CHECK_EQ(t.resolve(uint16_t('3')), uint16_t(0));          // 光标停在旧数字后
    CHECK_EQ(t.resolve(uint16_t(0x3002)), uint16_t(0x3002)); // 「。」照报给 press2 守卫
    CHECK_EQ(t.resolve(0), uint16_t(0));
    t.noteKeyPassthrough(0x35, 0);
    CHECK_EQ(t.resolve(0), uint16_t('5'));                   // 读不到宿主
    CHECK_EQ(t.resolve(uint16_t('5')), uint16_t('5'));       // 宿主读到的正是它
    CHECK_EQ(t.resolve(uint16_t(0x5BF9)), uint16_t('5'));    // surrounding 异步滞后的旧字「对」
}

void TestUtf()
{
    CASE("Utf：UTF-16 长度 / 偏移换算 / 码点数 / 末位码点");
    CHECK_EQ(utf16Length("a你𠀀"), size_t(4));
    CHECK_EQ(utf16OffsetToUtf8("a你𠀀", 2), size_t(4));
    CHECK_EQ(utf16OffsetToUtf8("a你𠀀", 3), size_t(8)); // 代理对中间 → 向后取整
    CHECK_EQ(utf16OffsetToUtf8("a你𠀀", 4), size_t(8));
    CHECK_EQ(utf8CharCount("a你𠀀"), size_t(3));
    CHECK_EQ(lastCodepoint("x9"), uint32_t('9'));
    CHECK_EQ(lastCodepoint(""), 0u);
    CHECK_EQ(utf16Length(std::string("\xff", 1)), size_t(1)); // 非法字节不崩

    CASE("删光标前 N 个 UTF-16 码元 → 码点数（Fcitx5 的 deleteSurroundingText 按码点）");
    CHECK_EQ(codepointsForUtf16Back("a你𠀀", 2), size_t(1));      // 𠀀 是代理对：2 码元 = 1 字
    CHECK_EQ(codepointsForUtf16Back("a你𠀀", 3), size_t(2));
    CHECK_EQ(codepointsForUtf16Back("a你𠀀", 4), size_t(3));
    CHECK_EQ(codepointsForUtf16Back("a你𠀀", 1), size_t(0));      // 落在代理对中间：不删半个字
    CHECK_EQ(codepointsForUtf16Back("你好", 2), size_t(2));       // BMP：码元 = 码点
    CHECK_EQ(codepointsForUtf16Back("😀", 6), size_t(1 + 4));    // 宿主只给了一段：余下按 1 计
    CHECK_EQ(codepointsForUtf16Back("", 3), size_t(3));
}

} // namespace

int main()
{
    TestBasicFlow();
    TestCommitBeforeClearingPreedit();
    TestCaretIsUtf16ToBytes();
    TestPassThroughAndClear();
    TestHeldSymbol();
    TestCommitAndHoldVsDefer();
    TestHoldTimer();
    TestReplaceBackwardAndCursor();
    TestDigitTracker();
    TestSeqTracker();
    TestSeqTrackerCursor();
    TestUtf();
    TEST_MAIN_END();
}
