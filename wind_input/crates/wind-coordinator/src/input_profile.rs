//! 输入行为的平台档位：桌面 / 移动端的差异集中在这一个值上。
//!
//! 协调器被桌面和 Android 共用。过去用编译期判断（按目标平台的编译期宏）区分，
//! 结果主机上永远只有桌面那一支，移动端分支测不到。现在由宿主在**构造时**传入
//! [`InputProfile`]，主机测试直接构造 `InputProfile::mobile()` 就能走到移动分支。
//!
//! ★ 平台差异全部落在配置文件的 `[mobile.*]` 段（经 [`ConfigOverlay`] 选层）和下面几个
//! 行为开关上。
//!
//! ⛔ 别把差异改成「值域哨兵」（让 `kind` / `mode` 取 `"auto"`、由代码按平台解释）：
//! 那会把平台知识塞进值域，设置界面被迫列一个语义空洞的「自动」选项，而代码里也要退化成
//! 一组 `platform_default_*` 函数——一处判断变成每个字段一处。
//!
//! 同一个协调器实例的 Profile 在构造后不变。

use wind_config::ConfigOverlay;

use crate::handle_candidate::MOBILE_PINYIN_SINGLE_LIMIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputProfile {
    /// 取配置时是否再叠 `[mobile.*]` 覆盖段。
    pub config_overlay: ConfigOverlay,
    /// 数字键在英文方案组码中是否进编码缓冲（而不是选候选）。
    ///
    /// 英文词里常带数字（mp3、h2o、win10），软键盘上数字要上滑或切层才打得出，没人会拿它
    /// 去选候选。桌面物理键盘上数字选词是长期习惯，故不进。中文方案任何档位都不进。
    pub digit_enters_english_buffer: bool,
    /// 分隔符 `auto` 在两个候选键都被占时，是否允许夺取 `'`。
    ///
    /// 手机是触屏，选词键没人按，而「分词」键需要一个出路；桌面不动选词键。
    pub separator_may_claim_quote: bool,
    /// 拼音单字母输入的首批候选上限；`None` = 不收紧。
    pub pinyin_single_letter_limit: Option<usize>,
}

impl InputProfile {
    /// 桌面档位：所有开关关闭，配置只看基线。
    pub const fn desktop() -> Self {
        Self {
            config_overlay: ConfigOverlay::Base,
            digit_enters_english_buffer: false,
            separator_may_claim_quote: false,
            pinyin_single_letter_limit: None,
        }
    }

    /// 移动端档位。
    pub const fn mobile() -> Self {
        Self {
            config_overlay: ConfigOverlay::Mobile,
            digit_enters_english_buffer: true,
            separator_may_claim_quote: true,
            pinyin_single_letter_limit: Some(MOBILE_PINYIN_SINGLE_LIMIT),
        }
    }
}
