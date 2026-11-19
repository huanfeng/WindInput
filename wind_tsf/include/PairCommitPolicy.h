// 配对插入提交方式（pair_via_comp）的纯逻辑判据。Win32 无关，测试可直接编译
// （pair_commit_policy_test.cpp）。
//
// 背景：InsertTextWithCursor 的提交方式二选一——TSF 直写（ITfInsertAtSelection，长期现状）
// 或按键注入（VK_PACKET）。AutoCAD 表格在位编辑器丢弃直写的 ASCII 却谎报 S_OK，字符静默
// 丢失（2026-11-18 靶机实测）。
#ifndef WIND_PAIR_COMMIT_POLICY_H
#define WIND_PAIR_COMMIT_POLICY_H

#include <cstddef>
#include <cstdint>

namespace wind::paircommit
{
// 服务端 CONFIG_KEY_PAIR_VIA_COMP 的出厂默认（每次重连从它起步；握手时服务端必推）。
inline constexpr bool kDefaultPairViaComp = false;

// 解码开关值：u8 0/1（对齐 Rust encode_pair_via_comp_value）。
// 空值/空指针保持现值——服务端推的值不该因格式意外而翻转默认。
inline bool DecodePairViaComp(const std::uint8_t* data, std::size_t len, bool current)
{
    if (data == nullptr || len == 0)
        return current;
    return data[0] != 0;
}
} // namespace wind::paircommit

#endif // WIND_PAIR_COMMIT_POLICY_H
