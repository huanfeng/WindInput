//! 快捷输入格式表：候选「渲染成什么样、按什么顺序」的外置配置。
//!
//! 设计与约束见 `docs/design/quick-input-format-table.md`。三条要点：
//!
//! - 解析归代码、渲染归配置：`kind` 是白名单（对应五个已有解析器），配置不能新增；
//! - 组内顺序归 `position`，跨来源顺序仍归 `mix_modes.members`（不设第二真相源）；
//! - 坏掉也得能打字：文件缺失/整份解析失败一律回落 [`FormatTable::builtin`]，
//!   单条非法只剔除该条。

use std::path::Path;
use tracing::warn;

/// 解析器类别。恰好对应五个已有解析器，不可由配置新增。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    /// 完整日期 `2025.12.25`（用户明确给了年）
    Date,
    /// 月日 `12.25`（年由代码补当年）
    ///
    /// 与 [`Self::Date`] 分开：只打两段时想要的多半是不带年的短写法。变量集与 `Date` 相同
    /// （年取当前年），差别只在出厂条目与用户调整各自记账。
    MonthDay,
    /// 年月 `2025.12`
    YearMonth,
    /// 数字 / 金额（纯数字，或算式求值结果）
    Number,
    /// 算式求值
    Calc,
}

impl FormatKind {
    /// 全部类别，按 [`Self::group_order`] 升序。设置页与穷尽性测试共用这一份，别另写枚举列表。
    pub const ALL: &'static [Self] = &[
        Self::Date,
        Self::MonthDay,
        Self::YearMonth,
        Self::Number,
        Self::Calc,
    ];

    /// 配置文件里的写法 → 类别。也用于候选 id（`quick:{kind}:{格式 id}`）的反解析，
    /// 故与 [`Self::as_str`] 必须严格互逆。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "date" => Some(Self::Date),
            "month_day" => Some(Self::MonthDay),
            "year_month" => Some(Self::YearMonth),
            "number" => Some(Self::Number),
            "calc" => Some(Self::Calc),
            _ => None,
        }
    }

    /// 表内分组顺序。不是候选的呈现顺序（那由 `mix_modes.members` 决定），
    /// 只用来让 `entries()` 有个稳定的规范表示，好让「出厂文件 == 内置表」可比。
    fn group_order(self) -> u8 {
        match self {
            Self::Date => 0,
            Self::MonthDay => 1,
            Self::YearMonth => 2,
            Self::Number => 3,
            Self::Calc => 4,
        }
    }

    /// 配置文件里的写法（错误信息与测试用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::MonthDay => "month_day",
            Self::YearMonth => "year_month",
            Self::Number => "number",
            Self::Calc => "calc",
        }
    }

    /// 本类支持的变量名（校验用；取值实现见 `crate::vars`）。
    ///
    /// 农历变量只给 `Date` 与 `MonthDay`，不给 `YearMonth`：农历月与公历月不一一对应，
    /// `2026.12` 推不出唯一的农历月。
    pub fn supports_var(self, name: &str) -> bool {
        let common_year = matches!(name, "Y" | "YYYY" | "YY" | "YC");
        match self {
            // 月日与完整日期同一套变量：年份取当前年，故 `$Y`/农历照样可用
            // （出厂表借此把「2026年12月25日」留作月日组的次选）。
            Self::Date | Self::MonthDay => {
                common_year
                    || matches!(name, "M" | "MM" | "MC" | "D" | "DD" | "DC")
                    || crate::lunar::is_var(name)
            }
            Self::YearMonth => common_year || matches!(name, "M" | "MM" | "MC"),
            Self::Number => matches!(name, "N" | "THOU" | "CNL" | "CNU" | "DIG" | "AMT"),
            Self::Calc => matches!(name, "EXPR" | "RESULT" | "EXACT"),
        }
    }

    /// 本类可用变量的展示清单（名字 + 一句话，含示例值），供设置页的模板输入框提示。
    /// 放在 core 是因为白名单是 [`Self::supports_var`] 的知识，设置页硬编码会静默过时。
    ///
    /// 与 `supports_var` 是两份数据，测试钉住名字都能通过 `supports_var` 且每类条数写死：
    /// 加变量时那个数字会红，提醒同步这里与文档站的变量表。
    pub fn var_hints(self) -> &'static [(&'static str, &'static str)] {
        // 公历年月日（date / month_day / year_month 共用前几项）
        const YEAR: &[(&str, &str)] = &[
            ("Y", "年，如 2026"),
            ("YYYY", "四位年，2026"),
            ("YY", "两位年，26"),
            ("YC", "汉字年，二〇二六"),
        ];
        const MONTH: &[(&str, &str)] =
            &[("M", "月，3"), ("MM", "两位月，03"), ("MC", "汉字月，三")];
        const DAY: &[(&str, &str)] = &[("D", "日，5"), ("DD", "两位日，05"), ("DC", "汉字日，五")];
        const LUNAR: &[(&str, &str)] = &[
            ("LY", "农历干支年，丙午"),
            ("LYN", "农历年数字，2026（以正月初一为界，可能与公历差 1）"),
            ("LZ", "生肖，马"),
            ("LM", "农历月，四月"),
            ("LD", "农历日，廿九"),
            ("LMD", "农历月日，四月廿九"),
            ("LF", "农历节日，端午节（非节日为空）"),
        ];
        // 拼接结果必须是 `'static`，故各类各写一份完整的常量（拼接要分配）。
        const DATE_ALL: &[(&str, &str)] = &[
            YEAR[0], YEAR[1], YEAR[2], YEAR[3], MONTH[0], MONTH[1], MONTH[2], DAY[0], DAY[1],
            DAY[2], LUNAR[0], LUNAR[1], LUNAR[2], LUNAR[3], LUNAR[4], LUNAR[5], LUNAR[6],
        ];
        const YEAR_MONTH_ALL: &[(&str, &str)] = &[
            YEAR[0], YEAR[1], YEAR[2], YEAR[3], MONTH[0], MONTH[1], MONTH[2],
        ];
        match self {
            // 月日与完整日期同一套变量（年份取当前年），故共用一份清单。
            Self::Date | Self::MonthDay => DATE_ALL,
            // 无 $D 系列、无农历：农历月与公历月不是一一对应，`2026.12` 推不出唯一农历月。
            Self::YearMonth => YEAR_MONTH_ALL,
            Self::Number => &[
                ("N", "原数字，1234.5"),
                ("THOU", "千分位，1,234.5"),
                ("CNL", "中文小写，一千二百三十四点五"),
                ("CNU", "中文大写，壹仟贰佰叁拾肆点伍"),
                ("DIG", "逐位读，一二三四点五"),
                ("AMT", "金额大写，壹仟贰佰叁拾肆元伍角（超两位小数为空）"),
            ],
            Self::Calc => &[
                ("EXPR", "算式原文，1+2*3"),
                ("RESULT", "计算结果，7"),
                (
                    "EXACT",
                    "结果原始精度（未按小数位截断），供 {pct()} 等函数二次换算，如 1/3 → 0.3333333333333333",
                ),
            ],
        }
    }
}

/// 一条格式：解析出的量渲染成候选文本的模板。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatEntry {
    /// 稳定标识（同 kind 内唯一）。日志与将来的候选身份按它认人。
    pub id: String,
    pub kind: FormatKind,
    /// 模板。`$` 变量路径见 [`crate::template`]；含裸 `{` 时走表达式路径。
    pub text: String,
    /// 组内顺序（升序）。缺省时按文件出现序。
    pub position: i32,
}

impl FormatEntry {
    /// 是否走表达式路径（`{amt(unit='圆')}`）而非 `$` 变量替换。
    ///
    /// 分流与 `wind-phrase` 的短语 text 同构，判据收窄为「含裸 `{`」：格式表一条只产
    /// 一条候选，故 `$SS`/`$AA`（多候选）与 `$CC`（副作用动作）都不适用，
    /// 不能直接套用 `is_cmdbar_grammar`。
    pub fn is_expression(&self) -> bool {
        crate::template::has_bare_brace(&self.text)
    }
}

/// 内置默认表。必须与出厂 `data/system.quick.toml` 逐条一致（有测试钉住），是文件缺失/损坏
/// 时的兜底。顺序不要随手调整，首选变了就是用户可感的行为变更。
const BUILTIN: &[(&str, FormatKind, &str)] = &[
    // 日期：中文 → 全汉字 → ISO 扩展 → ISO 基本 → 斜杠
    ("date.cn", FormatKind::Date, "$Y年$M月$D日"),
    ("date.cn_hans", FormatKind::Date, "$YC年$MC月$DC日"),
    ("date.iso", FormatKind::Date, "$YYYY-$MM-$DD"),
    ("date.basic", FormatKind::Date, "$YYYY$MM$DD"),
    ("date.slash", FormatKind::Date, "$YYYY/$MM/$DD"),
    // 农历排在公历五条之后：首选不变，日期候选 5→7 条仍在一页内。
    // 超出 1900–2100 时这两条自动消失（变量取不到值 → 整条模板作废）。
    ("date.lunar", FormatKind::Date, "农历$LMD"),
    ("date.lunar_ganzhi", FormatKind::Date, "$LY年$LMD"),
    // 月日（只打两段）：不带年的短写法在前，农历其次，补年的两条垫底。
    // 改这里必须同步出厂 `data/system.quick.toml`（`factory_file_matches_builtin_table`）。
    ("month_day.cn", FormatKind::MonthDay, "$M月$D日"),
    ("month_day.cn_hans", FormatKind::MonthDay, "$MC月$DC日"),
    ("month_day.iso", FormatKind::MonthDay, "$MM-$DD"),
    ("month_day.slash", FormatKind::MonthDay, "$MM/$DD"),
    ("month_day.lunar", FormatKind::MonthDay, "农历$LMD"),
    (
        "month_day.with_year_cn",
        FormatKind::MonthDay,
        "$Y年$M月$D日",
    ),
    (
        "month_day.with_year_iso",
        FormatKind::MonthDay,
        "$YYYY-$MM-$DD",
    ),
    // 年月：与完整日期同构
    ("year_month.cn", FormatKind::YearMonth, "$Y年$M月"),
    ("year_month.cn_hans", FormatKind::YearMonth, "$YC年$MC月"),
    ("year_month.iso", FormatKind::YearMonth, "$YYYY-$MM"),
    ("year_month.slash", FormatKind::YearMonth, "$YYYY/$MM"),
    // 数字：金额 → 中文小写 → 中文大写 → 逐位 → 千分位
    ("number.amount", FormatKind::Number, "$AMT"),
    ("number.cn_lower", FormatKind::Number, "$CNL"),
    ("number.cn_upper", FormatKind::Number, "$CNU"),
    ("number.digits", FormatKind::Number, "$DIG"),
    ("number.thousands", FormatKind::Number, "$THOU"),
    // 计算：结果作首选，等式次之，百分比殿后（默认 ×100 保两位小数、去尾零）
    ("calc.result", FormatKind::Calc, "$RESULT"),
    ("calc.equation", FormatKind::Calc, "$EXPR=$RESULT"),
    ("calc.percent", FormatKind::Calc, "{pct()}"),
];

/// 用户对某一类格式的调整（右键菜单产生，存在用户数据目录的 `quick_format.toml`，不写回格式表文件）。
///
/// 本类型是「中立数据」：`wind-store` 有自己的可序列化记录，协调器负责转换。
/// 两处不合并是刻意的——store 不依赖业务类型，与 shadow 同一条纪律。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatAdjust {
    /// 被移动过的条目 `(格式 id, 组内目标下标)`。
    /// LIFO，index 0 = 最新，应用时逆序遍历。
    pub moved: Vec<(String, usize)>,
    /// 被停用的格式 id。
    pub disabled: Vec<String>,
    /// 用户自己加的条目，排在基表条目之后（组内按 `position` 稳定排序）。调序、停用与
    /// 出厂条目共用 `moved` / `disabled`。
    ///
    /// 它是用户的内容而不是对出厂的调整：[`Self::is_empty`] 要算上它，「恢复默认」不得清它。
    pub added: Vec<FormatEntry>,
}

impl FormatAdjust {
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.disabled.is_empty() && self.added.is_empty()
    }

    /// 这个 id 是不是用户自己加的条目。
    pub fn is_user(&self, id: &str) -> bool {
        self.added.iter().any(|e| e.id == id)
    }

    /// 这条格式是否被用户调整过（移动或停用）。
    ///
    /// 语义是「有没有用户规则」，不是「位置是否与出厂不同」：移回原位也算调整过（规则仍在库里）。
    /// 设置页据此决定「恢复此条」能不能点，与 `wind_store::QuickFormatRecord::has_rule` 同口径。
    pub fn has_rule(&self, id: &str) -> bool {
        self.moved.iter().any(|(i, _)| i == id) || self.disabled.iter().any(|d| d == id)
    }
}

/// 设置页列表里的一行：条目 + 它当前的状态。
///
/// 与候选生成的 [`FormatTable::entries_of_adjusted`] 刻意分开两种口径：候选里停用项消失，
/// 设置页里停用项必须还在，否则再也开不回来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatEntryView<'a> {
    pub entry: &'a FormatEntry,
    /// 是否启用（停用项照样出现在本列表里，只是这里为 false）。
    pub enabled: bool,
    /// 用户是否调整过它（见 [`FormatAdjust::has_rule`]）。
    pub adjusted: bool,
    /// 是不是用户自己加的条目（而非出厂表里的）。
    ///
    /// 别拿 `adjusted` 当它用：出厂条目不能删、能恢复默认；用户条目能删、能改模板，没有默认可回。
    pub user: bool,
}

/// 格式表。条目已按 kind 分组、组内按 position 稳定排序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatTable {
    entries: Vec<FormatEntry>,
}

impl Default for FormatTable {
    fn default() -> Self {
        Self::builtin()
    }
}

impl FormatTable {
    /// 代码内置的出厂表（兜底路径）。
    ///
    /// `position` 按组内 1-based 计数，与出厂 TOML 一致，两者要能直接比相等
    /// （见 `tests/factory_table.rs`）。
    pub fn builtin() -> Self {
        let mut entries: Vec<FormatEntry> = Vec::with_capacity(BUILTIN.len());
        for (id, kind, text) in BUILTIN {
            let position = entries.iter().filter(|e| e.kind == *kind).count() as i32 + 1;
            entries.push(FormatEntry {
                id: (*id).to_string(),
                kind: *kind,
                text: (*text).to_string(),
                position,
            });
        }
        entries.sort_by_key(|e| (e.kind.group_order(), e.position));
        Self { entries }
    }

    /// 解析 TOML 文本。整份语法错误返回 `Err`（调用方回落 [`Self::builtin`]）；
    /// 单条非法只剔除该条并告警，其余照常——一条写错不该让整个快捷输入退回出厂。
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        let raw: RawFile = toml::from_str(toml_text)?;
        let mut entries: Vec<FormatEntry> = Vec::with_capacity(raw.formats.len());
        for (i, r) in raw.formats.into_iter().enumerate() {
            let Some(kind) = FormatKind::parse(&r.kind) else {
                warn!(
                    "快捷输入格式表: 跳过 id={} —— 未知 kind {:?}（可用: date/month_day/year_month/number/calc）",
                    r.id, r.kind
                );
                continue;
            };
            if let Err(e) = validate_format_text(kind, &r.text) {
                warn!("快捷输入格式表: 跳过 id={} —— {}", r.id, e);
                continue;
            }
            // `{kind}.u{数字}` 是设置页里用户条目的保留形态。基表占用它就会与用户条目
            // 同 id，行为取决于遍历顺序——把约定变成加载期检查（见 `is_user_format_id`）。
            if is_user_format_id(&r.id) {
                warn!(
                    "快捷输入格式表: 跳过 id={} —— `.u数字` 结尾是设置页自定义条目的保留形态，请换个 id",
                    r.id
                );
                continue;
            }
            if entries.iter().any(|e| e.kind == kind && e.id == r.id) {
                warn!(
                    "快捷输入格式表: 跳过重复 id={}（kind={}）",
                    r.id,
                    kind.as_str()
                );
                continue;
            }
            entries.push(FormatEntry {
                id: r.id,
                kind,
                text: r.text,
                // 缺省时用文件出现序。它与显式 position 混排时可能重叠，但同值保持出现序，
                // 结果仍与「作者看到的文件顺序」一致。
                position: r.position.unwrap_or(i as i32),
            });
        }
        // 先按 kind 分组、组内按 position。稳定排序，故同 position 保持文件出现序。
        // position 是组内序号（出厂文件里每类都从 1 起），跨类比较没有意义，
        // 分组是为了让 `entries()` 有个规范表示。
        entries.sort_by_key(|e| (e.kind.group_order(), e.position));
        Ok(Self { entries })
    }

    /// 从解析好的路径加载；`None`（两处都没有）或读取/解析失败一律回落
    /// [`Self::builtin`] 并告警——配置坏掉不能导致「打不出字」。
    ///
    /// 路径解析本身是调用方的事（`Config::resolve_data_file`，含用户覆盖日志）。
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            warn!("快捷输入格式表: system.quick.toml 两处均不存在，回落内置默认表（部署可能损坏）");
            return Self::builtin();
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "快捷输入格式表: 读取失败，回落内置默认表 {}: {}",
                    path.display(),
                    e
                );
                return Self::builtin();
            }
        };
        match Self::parse(&text) {
            Ok(t) if t.is_empty() => {
                warn!(
                    "快捷输入格式表: {} 未产出任何有效条目，回落内置默认表",
                    path.display()
                );
                Self::builtin()
            }
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "快捷输入格式表: 解析失败，回落内置默认表 {}: {}",
                    path.display(),
                    e
                );
                Self::builtin()
            }
        }
    }

    /// 某类的条目（已按 position 排序）。
    pub fn entries_of(&self, kind: FormatKind) -> impl Iterator<Item = &FormatEntry> {
        self.entries.iter().filter(move |e| e.kind == kind)
    }

    /// 某类的条目，已应用用户调整：取基表该类条目 → 剔除 `disabled` → 逆序（LIFO）遍历
    /// `moved`，把该 id 插到目标下标。没被碰过的条目保持基表顺序，出厂新增的格式自然落在
    /// 出厂位置；找不到的 id 静默忽略。见 docs/design/quick-input-format-table.md §11。
    ///
    /// `adjust.added` 也在结果中（接在基表之后，再由 `moved` 定位），故返回的引用可能指向
    /// 表也可能指向调整，两个入参共用生命周期 `'a`。
    pub fn entries_of_adjusted<'a>(
        &'a self,
        kind: FormatKind,
        adjust: &'a FormatAdjust,
    ) -> Vec<&'a FormatEntry> {
        self.entries_with(kind, &adjust.moved, &adjust.disabled, &adjust.added)
    }

    /// [`Self::entries_of_adjusted`] 的实现，三段规则分开收，以便部分借用：
    /// [`Self::entries_of_view`] 要算一趟「`disabled` 当作空」的结果，若收整个 `&'a FormatAdjust`
    /// 就得现造临时值，其 `added` 活不到 `'a`。
    fn entries_with<'a>(
        &'a self,
        kind: FormatKind,
        moved: &[(String, usize)],
        disabled: &[String],
        added: &'a [FormatEntry],
    ) -> Vec<&'a FormatEntry> {
        let alive = |e: &FormatEntry| !disabled.iter().any(|d| d == &e.id);
        let mut list: Vec<&FormatEntry> = self.entries_of(kind).filter(|e| alive(e)).collect();
        // 用户条目接在基表之后（组内按 position 稳定排序），于是「新加的排末尾」，
        // 而出厂日后新增的条目仍落在它的出厂位置——两类互不挤位。
        let mut mine: Vec<&FormatEntry> = added
            .iter()
            .filter(|e| e.kind == kind && alive(e))
            .collect();
        mine.sort_by_key(|e| e.position);
        list.extend(mine);
        for (id, position) in moved.iter().rev() {
            let Some(from) = list.iter().position(|e| &e.id == id) else {
                continue; // 孤儿规则：基表与用户条目里都没有这个 id
            };
            let entry = list.remove(from);
            // 下标可能越界：条目被停用、或基表条目数变少（用户改了文件）
            let to = (*position).min(list.len());
            list.insert(to, entry);
        }
        list
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 全部条目（设置页/调试用）。
    pub fn entries(&self) -> &[FormatEntry] {
        &self.entries
    }

    /// 某类的条目，设置页口径：启用项按候选实际顺序，停用项留在原位。
    ///
    /// 启用项顺序直接取 [`Self::entries_of_adjusted`] 的结果，不重算，否则两处会漂移。
    /// 每个停用项插在锚点（它前面最近的启用条目）之后，锚点查的是「`disabled` 清空后重算」
    /// 的顺序而非基表，这样先调序再停用的条目也不跳。不能改成「不剔除停用项、直接对全表套
    /// `moved`」：`moved` 的下标是剔除停用项之后的位置。推导与反例见
    /// docs/design/quick-input-format-table.md §11.1。
    ///
    /// 用户条目与出厂条目同等对待，只由 [`FormatEntryView::user`] 标记出身。
    pub fn entries_of_view<'a>(
        &'a self,
        kind: FormatKind,
        adjust: &'a FormatAdjust,
    ) -> Vec<FormatEntryView<'a>> {
        let is_disabled = |id: &str| adjust.disabled.iter().any(|d| d == id);
        let mut out: Vec<FormatEntryView<'a>> = self
            .entries_of_adjusted(kind, adjust)
            .into_iter()
            .map(|entry| FormatEntryView {
                enabled: true,
                adjusted: adjust.has_rule(&entry.id),
                user: adjust.is_user(&entry.id),
                entry,
            })
            .collect();

        // 「假设全部启用」的顺序：停用项的位置从这里读，它们的 moved 规则在此生效
        // （见上方 doc）。`moved` 与 `added` 照常，只把 `disabled` 当作空——三段规则
        // 分开收正是为了这一趟（见 `entries_with`：造临时 FormatAdjust 会让 added 悬垂）。
        let as_if_all_enabled = self.entries_with(kind, &adjust.moved, &[], &adjust.added);

        // 停用项按该顺序归组，每组挂在同一个锚点后面（连续几条停用的保持彼此相对顺序）。
        let mut groups: Vec<(Option<&str>, Vec<&'a FormatEntry>)> = Vec::new();
        let mut anchor: Option<&str> = None;
        for e in as_if_all_enabled {
            if is_disabled(&e.id) {
                match groups.last_mut() {
                    Some((a, v)) if *a == anchor => v.push(e),
                    _ => groups.push((anchor, vec![e])),
                }
            } else {
                anchor = Some(&e.id);
            }
        }
        // 从后往前插入：插入点每次按锚点 id 重新定位，故靠后的插入不会挪动靠前锚点的下标。
        for (anchor, items) in groups.iter().rev() {
            let at = match anchor {
                None => 0,
                Some(id) => out
                    .iter()
                    .position(|v| v.entry.id == *id)
                    .map(|i| i + 1)
                    .unwrap_or(out.len()),
            };
            for (k, entry) in items.iter().enumerate() {
                out.insert(
                    at + k,
                    FormatEntryView {
                        entry,
                        enabled: false,
                        // 在 disabled 里即已被调整过，无需再查。
                        adjusted: true,
                        user: adjust.is_user(&entry.id),
                    },
                );
            }
        }
        out
    }

    /// 本类里是否已有逐字相同的模板（基表与用户条目一起查），返回撞上的那条 id。
    ///
    /// 新增/改写时据此拒绝重复并指出撞的是哪一条。只比逐字相同：渲染等价随输入而变，
    /// 不是稳定判据。
    pub fn duplicate_text_of(
        &self,
        kind: FormatKind,
        text: &str,
        adjust: &FormatAdjust,
        except: Option<&str>,
    ) -> Option<String> {
        let keep = |id: &str| except != Some(id);
        self.entries_of(kind)
            .chain(adjust.added.iter().filter(|e| e.kind == kind))
            .find(|e| e.text == text && keep(&e.id))
            .map(|e| e.id.clone())
    }
}

/// 用户条目 id 的保留形态：`{kind}.u{序号}`（如 `date.u1`）。
///
/// 命名空间靠加载期检查守住：[`FormatTable::parse`] 拒绝匹配本形态的出厂 id，
/// 否则出厂条目会与用户条目静默撞 id。
pub fn is_user_format_id(id: &str) -> bool {
    match id.rsplit_once(".u") {
        Some((head, n)) => {
            !head.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// 下一个可用的用户条目 id。
///
/// 取「已有最大序号 + 1」而不是「条数 + 1」：删掉中间一条后条数回退，会撞上仍存在的 id。
pub fn next_user_format_id(kind: FormatKind, adjust: &FormatAdjust) -> String {
    let prefix = format!("{}.u", kind.as_str());
    let max = adjust
        .added
        .iter()
        .filter(|e| e.kind == kind)
        .filter_map(|e| e.id.strip_prefix(&prefix))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", max + 1)
}

/// 模板静态校验：变量名必须在本 kind 的白名单内。
///
/// 在加载期做而不是渲染期（热路径上告警会刷屏）。加载遇坏条目剔除并 warn，设置页则在保存前
/// 拒绝；两处共用同一份判据，否则设置页放行的模板会在下次启动时被静默剔除。
pub fn validate_format_text(kind: FormatKind, text: &str) -> Result<(), String> {
    // 用 trim 判空而不是 `is_empty`：纯空白模板会渲染出一条「看起来是空的」候选，
    // 选中它则上屏几个空格。设置页的输入框里这是最容易误提交的内容。
    if text.trim().is_empty() {
        return Err("模板为空".into());
    }
    if crate::template::has_bare_brace(text) {
        // 表达式路径：函数名/参数由 cmdbar 在求值期校验（本 crate 不依赖它）。
        // 这里只拒绝混用：变量路径不认识 `{month()}`，会把它原样上屏。
        if crate::template::has_variable(text) {
            return Err("不能在同一条模板里混用 $变量 与 {表达式}，二选一".into());
        }
        return Ok(());
    }
    // 纯字面模板（既无 `$` 变量也无 `{}` 表达式）恒定输出，该配成短语而非格式。
    // 拦它主要是防漏花括号：照文档函数表抄 `amt()` 而不是 `{amt()}`，否则会一路通过、
    // 候选出的是源码本身。
    if !crate::template::has_variable(text) {
        return Err(if text.contains('(') && text.contains(')') {
            // 把用户写的原样包一层回显，比讲语法快
            format!("函数要包在花括号里，写成 {{{}}}", text.trim())
        } else {
            "模板里没有任何变量或函数，这条候选会恒定输出这段文字".into()
        });
    }
    // RefCell 而非 &mut：`expand` 收的是 `Fn`（渲染期要能重复调用），闭包不能可变捕获。
    let bad: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let ok = crate::template::expand(text, |name| {
        if kind.supports_var(name) {
            Some(String::new())
        } else {
            let mut b = bad.borrow_mut();
            if b.is_none() {
                *b = Some(name.to_string());
            }
            None
        }
    });
    match (ok, bad.into_inner()) {
        (Some(_), _) => Ok(()),
        (None, Some(name)) => Err(format!("kind={} 不支持变量 ${}", kind.as_str(), name)),
        (None, None) => Err("模板语法错误（`${` 未闭合）".into()),
    }
}

#[derive(serde::Deserialize)]
struct RawFile {
    #[serde(default)]
    formats: Vec<RawEntry>,
}

#[derive(serde::Deserialize)]
struct RawEntry {
    id: String,
    kind: String,
    text: String,
    #[serde(default)]
    position: Option<i32>,
}

#[cfg(test)]
#[path = "format_table_tests.rs"]
mod tests;
