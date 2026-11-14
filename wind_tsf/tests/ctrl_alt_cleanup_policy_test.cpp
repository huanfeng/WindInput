// g++ -std=c++17 -Wall -Wextra -Iwind_tsf/include wind_tsf/tests/ctrl_alt_cleanup_policy_test.cpp -o /tmp/ctrl_alt_cleanup_test
#include "CtrlAltCleanupPolicy.h"
#include <cstdio>
#include <initializer_list>

int main()
{
    using wind::ctrlalt::ShouldReturnToHost;
    int failures = 0;
    // 普通宿主快捷键：清理组合后重放，无组合时直接透传。
    for (auto type : {ResponseType::PassThrough, ResponseType::ClearCompositionThenPassThrough})
        if (!ShouldReturnToHost(type)) ++failures;
    // 加词 Ctrl+Enter 关闭面板，其他 Ctrl 组合可能只回 Consumed；二者均不可交还宿主。
    // 候选操作、整段上屏及错误兜底也必须保留响应原有的消费语义。
    for (auto type : {ResponseType::ClearComposition, ResponseType::Consumed, ResponseType::Ack,
                      ResponseType::UpdateComposition, ResponseType::CommitText,
                      ResponseType::InsertTextWithCursor, ResponseType::CommitThenDefer,
                      ResponseType::ReplaceBackward, ResponseType::StatusUpdate,
                      ResponseType::HoldComposition, ResponseType::CommitAndHold, ResponseType::Error})
        if (ShouldReturnToHost(type)) ++failures;
    std::printf("ctrl_alt_cleanup_policy_test: %d failure(s)\n", failures);
    return failures ? 1 : 0;
}
