// 会话中的 Ctrl/Alt 组合按服务端的显式响应决定归属，不能根据操作后的组合状态推断。
// 加词编辑会关闭面板并返回 ClearComposition，但这一键已经由输入法消费。
#pragma once

#include "ResponseType.h"

namespace wind
{
namespace ctrlalt
{

/// 只有显式透传响应才把当前键交给宿主。
/// ClearCompositionThenPassThrough 还要求先收组合，再重放原键。
inline bool ShouldReturnToHost(ResponseType type)
{
    return type == ResponseType::PassThrough || type == ResponseType::ClearCompositionThenPassThrough;
}

} // namespace ctrlalt
} // namespace wind
