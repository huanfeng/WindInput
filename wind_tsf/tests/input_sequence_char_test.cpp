// InputSequenceChar 单元测试
//
// 跑法（纯 C++17、不含 Win32 头）：
//   g++ -std=c++17 -I../include -o input_sequence_char_test input_sequence_char_test.cpp && ./input_sequence_char_test
//
// 守的是 seqChar 的取值规则：透传键只报类别代表值，上屏文本取末尾码点的首个 UTF-16 单元。
// 测不到的部分（记录点挂在哪、何时清零）是接线，见 KeyEventSink.cpp。

#include "InputSequenceChar.h"

#include <cstdio>
#include <cwchar>

namespace
{

using WindInputSeq::FromCommittedText;
using WindInputSeq::FromVk;

int g_failures = 0;

void Expect(const char* name, uint16_t got, uint16_t want)
{
    if (got != want)
    {
        std::printf("FAIL %s: got 0x%04X, want 0x%04X\n", name, got, want);
        ++g_failures;
    }
}

uint16_t Text(const wchar_t* s) { return FromCommittedText(s, std::wcslen(s)); }

} // namespace

int main()
{
    // 透传键：只报类别代表值。
    Expect("letter_reports_a", FromVk('H', false, false), 'a');
    Expect("shift_letter_reports_a", FromVk('H', true, false), 'a');
    Expect("digit_reports_itself", FromVk('7', false, false), '7');
    Expect("numpad_digit_reports_digit", FromVk(0x63, false, false), '3');
    Expect("shift_digit_is_symbol", FromVk('1', true, false), '?');
    Expect("space_reports_space", FromVk(0x20, false, false), ' ');
    Expect("oem_period_is_symbol", FromVk(0xBE, false, false), '?');
    Expect("oem_quote_is_symbol", FromVk(0xDE, false, false), '?');
    Expect("oem_102_is_symbol", FromVk(0xE2, false, false), '?');
    Expect("numpad_decimal_is_symbol", FromVk(0x6E, false, false), '?');

    // 编辑/导航/功能键与宿主快捷键：不可知。
    Expect("backspace_unknown", FromVk(0x08, false, false), WindInputSeq::kUnknown);
    Expect("enter_unknown", FromVk(0x0D, false, false), WindInputSeq::kUnknown);
    Expect("tab_unknown", FromVk(0x09, false, false), WindInputSeq::kUnknown);
    Expect("left_arrow_unknown", FromVk(0x25, false, false), WindInputSeq::kUnknown);
    Expect("f5_unknown", FromVk(0x74, false, false), WindInputSeq::kUnknown);
    Expect("ctrl_letter_unknown", FromVk('V', false, true), WindInputSeq::kUnknown);
    Expect("ctrl_digit_unknown", FromVk('1', false, true), WindInputSeq::kUnknown);

    // 上屏文本：末尾码点。
    Expect("cjk_tail", Text(L"你好"), 0x597D);
    Expect("latin_tail", Text(L"hello"), 'o');
    Expect("trailing_space", Text(L"hello "), ' ');
    Expect("cn_punct_tail", Text(L"你。"), 0x3002);
    // 扩展 B 汉字 U+20000 = D840 DC00：取高代理。显式写代理对——本测试也在 wchar_t 为 32 位的
    // Linux 上跑，那里 L"\U00020000" 只占一个单元；DLL 侧 wchar_t 恒为 UTF-16。
    const wchar_t extB[] = {L'\u4F60', (wchar_t)0xD840, (wchar_t)0xDC00, 0};
    Expect("supplementary_tail_reports_high_surrogate", Text(extB), 0xD840);
    // 孤立低代理不配对：原样报出（服务端归「其它」）。
    const wchar_t lone[] = {L'a', (wchar_t)0xDC00, 0};
    Expect("lone_low_surrogate", Text(lone), 0xDC00);
    Expect("empty_text", FromCommittedText(L"", 0), 0);
    Expect("null_text", FromCommittedText(nullptr, 3), 0);

    // 汉字判定与服务端 classify_unit 同表。
    Expect("cjk_basic", WindInputSeq::IsCjk(0x4F60), 1);
    Expect("cjk_ext_b_high_surrogate", WindInputSeq::IsCjk(0xD840), 1);
    Expect("kana", WindInputSeq::IsCjk(0x3042), 1);
    Expect("cn_punct_not_cjk", WindInputSeq::IsCjk(0x3002), 0);
    Expect("latin_not_cjk", WindInputSeq::IsCjk('a'), 0);
    Expect("unknown_not_cjk", WindInputSeq::IsCjk(WindInputSeq::kUnknown), 0);

    // 鼠标挪光标：按着鼠标、没改文本的选区变化才算；透传 / 自己上屏改了文本，不算。
    using WindInputSeq::CursorMovedByMouse;
    Expect("mouse_click_moves_cursor", CursorMovedByMouse(true, false, true, false), 1);
    Expect("typed_char_is_not_a_move", CursorMovedByMouse(true, false, true, true), 0);
    Expect("no_mouse_is_not_a_move", CursorMovedByMouse(true, false, false, false), 0);
    Expect("composition_is_not_a_move", CursorMovedByMouse(true, true, true, false), 0);
    Expect("no_sel_change_is_not_a_move", CursorMovedByMouse(false, false, true, false), 0);

    if (g_failures == 0)
        std::printf("input_sequence_char_test: all passed\n");
    return g_failures == 0 ? 0 : 1;
}
