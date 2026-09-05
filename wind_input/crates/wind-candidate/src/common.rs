//! 通用规范汉字表（常用字判定）
//!
//! 与 Go 版本 `wind_input/internal/dict/common_chars.go` 对齐。
//! 用于"检索范围"过滤：判定候选是否为常用字/词。

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// **用户覆盖**层，叠加在字符类 registry 的默认判定之上。
///
/// # ★ 判定的两层分工
///
/// | 层 | 谁 | 来源 |
/// |---|---|---|
/// | 默认判定 | [`crate::CharsetRegistry`]（**按参数传入**） | `charsets/*.yaml` 三层合并 |
/// | 用户覆盖 | 本结构的 `overrides` | 候选右键 / 词库管理，落在 redb |
///
/// 合成只发生在查询那一刻（覆盖优先），**不预先 merge 成一个集合**——merge 之后就分不清
/// 「这个字出厂就常用」还是「用户把它设成了常用」，而界面要显示的正是这个差别
/// （「出厂：生僻 → 现在：常用」），「恢复出厂」也需要知道回退到哪一边。
///
/// # ⚠️ registry 为什么是参数而不是字段
///
/// registry 由配置派生，配置一变就要重装。持有一份拷贝就多出一处「记得同步」的地方，
/// 而本仓已经因为这个吃过亏（`include_blocks` 漏在 schema_dirty 门控里，改完要切方案
/// 才生效）。**按参数传入让编译器保证每个调用点拿到的都是当前那一份**。
///
/// 这也与既有的 [`crate::rare_admits`] 同形——那个函数一直是「cc + registry 两份判定
/// 数据都作参数」。
pub struct CommonChars {
    /// 出厂字表的成员集。
    ///
    /// ⚠️ **不再参与判定**（那归 registry）。只剩两个职责：
    /// [`Self::is_empty`] 的「表没装进来」信号，以及 [`Self::list_all`] 里
    /// 「这个覆盖键在不在出厂表内」的分类。
    base: HashSet<char>,
    /// 出厂基表的**原始顺序**（列举用）。
    ///
    /// 单独留一份 Vec 而不是从 `base` 迭代：`common_chars.txt` 是按级别顺序拼接的
    /// （一级 3500 → 二级 3000 → 三级其余），而 HashSet 的迭代序是随机的。设置页要按
    /// 字表原序列出全表，从 HashSet 取会得到一份每次启动都不一样的乱序清单——分页更是
    /// 直接失效（第 2 页的内容会随机变）。8104 个 char 约 32KB，代价可以忽略。
    base_order: Vec<char>,
    /// 用户覆盖：`true` = 强制判为常用，`false` = 强制判为生僻。只含被碰过的字。
    ///
    /// 键是**一个字素簇**而不是一个 `char`：用户看到的「一个图形」可能由多个码位拼成
    /// （`⚽️`=2、`👨‍👩‍👧`=5、`🇨🇳`=2、`1️⃣`=3），而屏幕上分辨不出差别的东西，能不能设置
    /// 却不一样，是说不通的。存储层的键本就是 `&str`，故不涉及数据迁移。
    overrides: HashMap<String, bool>,
    /// 覆盖里**存在多码位键**。
    ///
    /// ★ 热路径闸门：[`Self::is_string_common`] 对**每个候选**都要跑一遍，而字素簇分割
    /// 比逐 `char` 遍历贵得多。绝大多数用户一个 emoji 序列都不会登记，此位为假时直接走
    /// 原来的逐 `char` 路径，开销不变。只有真的设过多码位覆盖，才付那份钱。
    has_multi_char_keys: bool,
}

impl CommonChars {
    /// 从字符类 registry 建：全表 = 那些**表态**类的成员，顺序取自配置。
    ///
    /// ★ 取代原先的 [`Self::load`]：常用字表已搬进 `charsets/common_han.yaml`，判定与
    /// 列表因此同源。两处各读一份文件的话，用户换了字表却只有一半生效——而那半是哪半，
    /// 取决于他改的是哪个文件。
    ///
    /// 多码位簇会被跳过：本结构的 `base` 是 `char` 集合（判定已归 registry，它只剩
    /// 「列表顺序 + 表空不空」两个职责），而表态类的成员本就全是单字符。
    pub fn from_registry(cs: &crate::CharsetRegistry) -> Self {
        Self::from_base(cs.ordered_members().filter_map(|m| {
            let mut it = m.chars();
            let c = it.next()?;
            it.next().is_none().then_some(c)
        }))
    }

    /// 从文件加载（一字一行，`#` 注释行跳过；空白与控制字符外全收，见 [`is_markable`]）。
    ///
    /// ⚠️ **出厂已不走这条路**（常用字表在 `charsets/common_han.yaml`）。保留它是给
    /// 「用户自己的类引用了一个外部字表」那种情形，以及本模块的测试。
    /// 失败（文件缺失）返回空集；上层应在空集时退化为"不过滤"。
    pub fn load(path: &Path) -> Self {
        let mut base = HashSet::new();
        let mut base_order = Vec::new();
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                for ch in line.chars() {
                    // 收录判据是 [`is_markable`] 而非 `is_han`：这份文件支持用户目录整份
                    // 覆盖，按汉字筛会让手写进去的符号（字根、注音…）**静默消失**，
                    // 而写端已放开到全字符，两头一紧一松没有道理。
                    // `insert` 返回 false = 重复字，不再追加进顺序表：字表里若有重复
                    // （历史数据难免），列表会出现两行一模一样的字。
                    if is_markable(ch) && base.insert(ch) {
                        base_order.push(ch);
                    }
                }
            }
        }
        Self {
            base,
            base_order,
            overrides: HashMap::new(),
            has_multi_char_keys: false,
        }
    }

    /// 由一批字直接构造基表（测试与内存态装配用）。
    pub fn from_base(chars: impl IntoIterator<Item = char>) -> Self {
        let mut base = HashSet::new();
        let mut base_order = Vec::new();
        for ch in chars {
            if base.insert(ch) {
                base_order.push(ch);
            }
        }
        Self {
            base,
            base_order,
            overrides: HashMap::new(),
            has_multi_char_keys: false,
        }
    }

    /// 全表列举：出厂字（按字表原序）+ 用户加的、**不在出厂表里**的字（追加在后）。
    ///
    /// 返回 `(字, 出厂是否常用, 现在是否常用)`。设置页靠它列全表并搜索——只列「改过的」
    /// 那几条不足以回答用户最常问的那个问题：「这个字现在算不算常用」。
    ///
    /// 追加的那批默认判定取 [`Self::is_base_common`]，**不是硬编码的 `false`**：默认字表
    /// 管不着的字符（`、` `ㄅ` `⿰`）事实上是被放行的，默认判定就得是「常用」。写死 false
    /// 会让用户把顿号设成生僻后，界面显示「默认：生僻 → 现在：生僻」——看上去什么都没改，
    /// 而写端那边判的是「与默认相反、要留记录」，两处对同一条记录给出相反的说法。
    /// 返回的第一项是**字素簇文本**而不是 `char`：覆盖表里可以有 `👨‍👩‍👧` 这样的多码位条目，
    /// 用 `char` 表示不了。默认字表那 8104 条全是单字符，转成 `String` 只是形态统一。
    pub fn list_all(&self, cs: &crate::CharsetRegistry) -> Vec<(String, bool, bool)> {
        let mut out: Vec<(String, bool, bool)> = self
            .base_order
            .iter()
            .map(|&c| {
                (
                    c.to_string(),
                    self.is_base_common(c, cs),
                    self.is_char_common(c, cs),
                )
            })
            .collect();
        // 覆盖里那些不在出厂表的：排序后追加，保证顺序稳定（HashMap 迭代序随机，
        // 直接追加会让这批每次刷新都换位置）。
        let mut extra: Vec<&String> = self
            .overrides
            .keys()
            .filter(|k| match single_char_of(k) {
                Some(c) => !self.base.contains(&c),
                None => true, // 多码位簇必不在默认字表里
            })
            .collect();
        extra.sort_unstable();
        // ⚠️ 默认判定走 `is_cluster_common_by_default`，**不能对多码位簇写死 `true`**：
        // `鬱`+VS16 这样的簇按默认判据是「生僻」，写死 true 会让界面显示「默认：常用 →
        // 现在：常用」（看着什么都没改），而写端判的是「与默认相反、要留记录」——
        // 两处对同一条记录给出相反的说法，正是本函数文档开头要避免的那件事。
        // 对单字符两者恒等（`!(in_scope && !base)` ≡ `!in_scope || base`），故是纯收敛。
        out.extend(extra.into_iter().map(|k| {
            (
                k.clone(),
                self.is_cluster_common_by_default(k, cs),
                self.is_cluster_common(k, cs),
            )
        }));
        out
    }

    /// **整体替换**用户覆盖（调用方从 store 全量读出后灌进来）。
    ///
    /// 刻意不做增量合并：增量语义下「撤销某字的覆盖」这个操作没有落点——被删掉的那条
    /// 在旧集合里仍然存在，用户会看到「点了恢复出厂、重启前毫无变化」。
    pub fn set_overrides(&mut self, it: impl IntoIterator<Item = (String, bool)>) {
        // ⚠️ **装载时滤掉不是「一个字素簇」的键**。库里的记录未必都经过写端校验——备份包
        // 里的 `common_chars.jsonl` 可能被手工改过、也可能来自更早的格式。一条
        // `{"ch":"我们"}` 进来有两重危害：读端逐簇查永远命中不了（死记录），以及它会把
        // `has_multi_char_keys` 顶成真、**永久**把热路径切到字素簇分割。
        //
        // 滤在这里而不是 store 层：判据只此一份（`single_markable_char`），且不管脏数据
        // 从哪条路径进的库都拦得住；让 wind-store 反过来依赖本 crate 才是架构倒置。
        self.overrides = it
            .into_iter()
            .filter(|(k, _)| single_markable_char(k).is_some())
            .collect();
        // 热路径闸门只在装载时算一次，查询时零成本（见字段注释）。
        self.has_multi_char_keys = self.overrides.keys().any(|k| k.chars().count() > 1);
    }

    /// 是否未加载到任何**出厂**字（数据缺失）。
    ///
    /// ⚠️ 判定本身已交给 registry，本方法只剩「上层据此退化为不过滤」这一个用途。
    /// registry 那边有一条同源的防线（`disarm_empty_scoped_classes`：名单为空的补集类
    /// 撤销表态），两者失效方向一致——都是**退化为不过滤**，而不是拿空表去过滤。
    ///
    /// ⚠️ 只看 `base`，不看覆盖：上层拿它决定「退化为不过滤」。若用户覆盖也算数，那么
    /// 出厂表缺失、而用户恰好设过一两个字时本函数返回 false，过滤照常进行——此刻几乎
    /// 所有字都查不到表、全被判成生僻，智能档会把候选滤得只剩那一两个字。
    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }

    /// 单字是否常用：**用户覆盖优先**，其次落到 [`Self::is_base_common`]。
    ///
    /// ★ 与 [`Self::is_string_common`] 对单字符串**必须给出同一个答案**
    /// （`char_and_string_judgements_agree` 钉着）。两者一处按字判、一处按串判，
    /// 判据分叉过就会出现「列表里显示常用、候选里却被滤掉」这种对不上账的现象。
    pub fn is_char_common(&self, ch: char, cs: &crate::CharsetRegistry) -> bool {
        self.is_cluster_common(ch.encode_utf8(&mut [0u8; 4]), cs)
    }

    /// 一个**字素簇**是否常用：覆盖优先，其次落到 [`Self::is_base_common`]。
    ///
    /// 多码位簇（`⚽️` `👨‍👩‍👧` `🇨🇳`）没有覆盖时恒判常用——它们整体落在默认字表管辖域外，
    /// 与单个域外字符同一待遇。
    pub fn is_cluster_common(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        match self.overrides.get(cluster) {
            Some(&v) => v,
            None => self.fallback_verdict_of(cluster, cs),
        }
    }

    /// 簇本身没有覆盖时的回落判定：**逐 `char` 走一遍完整判据**（先查覆盖、再查默认字表）。
    ///
    /// ★★ 两处都不能省，各挡一类闸门两侧不一致（`gate_consistency` 钉着）：
    ///
    /// - **不能写成「多码位簇一律忽略」**。`鬱`+`U+FE0F`（变体选择符）是一个簇，整簇忽略
    ///   就判成常用，而按 `char` 走时 `鬱` 查表失败应判非常用。
    /// - **逐 `char` 时必须也查覆盖**。用户对 `鬱` 登记过覆盖，但簇的键是 `鬱\u{FE0F}`
    ///   ——只查基表的话，那条覆盖在闸门开着时就查不到了。
    ///
    /// 症状都是「登记了一条 emoji 之后，某个字的判定忽然变了」，没人会把两件事联系起来。
    fn fallback_verdict_of(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        // ⛔ **不要在这里加「整簇先问一次 registry」**。那个写法从 `no_freq` / `in_rare`
        // 搬过来看着很自然，实际会破坏上面第二条不变量：`鬱`+VS16 整簇不在名单里、却在
        // Han 作用域内 ⇒ registry 直接判生僻并返回，**用户对 `鬱` 那条覆盖再也走不到**。
        //
        // ★ 根因是两种语义不能混：`no_freq` / `in_rare` 是**存在性**（任一命中即真），
        // 整簇查询在那里是补充；本方法是**全称**（每个 char 都常用才算常用），整簇查询
        // 在这里变成了短路。`gate_consistency` 抓住过一次。
        cluster.chars().all(|ch| self.char_verdict(ch, cs))
    }

    /// 单个 `char` 的判定：覆盖优先，其次受管辖的查默认字表，域外一律放行。
    /// 这就是闸门关着时逐 `char` 走的那条路径，抽出来供簇路径回落，两边因此不会分叉。
    fn char_verdict(&self, ch: char, cs: &crate::CharsetRegistry) -> bool {
        match self.overrides.get(ch.encode_utf8(&mut [0u8; 4])) {
            Some(&v) => v,
            // 无人表态 ⇒ 兜底判「常用」。这与原先 `!is_common_scope(ch) || base.contains()`
            // 里「域外一律放行」那一半是同一件事：出厂 `common_han` 类的 scope 就是
            // `is_common_scope`，域外没有别的类表态。
            None => cs.verdict_of_char(ch).unwrap_or(true),
        }
    }

    /// 簇的**默认**判定（忽略全部用户覆盖），供「与默认同向就删覆盖」那条判据用。
    /// 与 [`Self::fallback_verdict_of`] 的差别只在要不要看 `overrides`。
    fn default_verdict_of(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        // 同 `fallback_verdict_of`：全称语义，逐 char，⛔ 不加整簇短路。
        cluster
            .chars()
            .all(|ch| cs.verdict_of_char(ch).unwrap_or(true))
    }

    /// 默认判定（忽略用户覆盖）。供界面显示「默认：常用 → 现在：生僻」这类对照，
    /// 以及判断某条覆盖是否与默认同向（同向即冗余，写端据此删覆盖而不是存记录）。
    ///
    /// ★★ **默认字表管不着的字符，默认判定是「常用」而不是「生僻」**：读端对没有覆盖的
    /// 域外字符一律忽略、照常放行，那就是它们事实上的默认待遇。若照旧返回 `false`，
    /// 用户把 `、` 设成生僻会被 `apply_common_target` 判成「与默认同向」⇒ 删覆盖 ⇒
    /// **设了没有任何反应**，右键菜单还一直显示同一项，且全程无报错。
    pub fn is_base_common(&self, ch: char, cs: &crate::CharsetRegistry) -> bool {
        cs.verdict_of_char(ch).unwrap_or(true)
    }

    /// 一个字素簇的默认判定（忽略用户覆盖）。多码位簇恒「常用」——它整体落在默认字表
    /// 管辖域外，与单个域外字符同一待遇。
    pub fn is_cluster_common_by_default(&self, cluster: &str, cs: &crate::CharsetRegistry) -> bool {
        self.default_verdict_of(cluster, cs)
    }

    /// 某字的用户覆盖方向；`None` = 未覆盖。
    pub fn override_of(&self, ch: char) -> Option<bool> {
        self.override_of_cluster(ch.encode_utf8(&mut [0u8; 4]))
    }

    /// 某个字素簇的用户覆盖方向；`None` = 未覆盖。
    pub fn override_of_cluster(&self, cluster: &str) -> Option<bool> {
        self.overrides.get(cluster).copied()
    }

    /// 这串文本里**有没有**用户亲手标成生僻的字。
    ///
    /// 与 [`Self::is_string_common`] 的区别是强弱：那个答「按当前字表算不算常用」，
    /// 这个答「用户是不是明确说过不要它」。智能档的孤儿码位保底对后者不适用——
    /// 详见 [`crate::Candidate::user_rare`]。
    ///
    /// 词组里只要有一个字被降级就算——那个词整体也就不该再冒到前面来。
    pub fn has_user_rare(&self, text: &str) -> bool {
        self.units_of(text)
            .any(|u| match self.override_of_cluster(u) {
                Some(v) => !v,
                // ★ 簇本身没覆盖时**必须逐 `char` 再问一遍**，与 `is_string_common` 同一套回落。
                // 漏了这一步就是闸门两侧不一致的另一半：覆盖表的键是裸 `⚽`(U+26BD)，而候选
                // 文本是 `⚽️`(U+26BD U+FE0F)——闸门关着按 char 切能命中那条覆盖，开着按簇切
                // 却查不到，`user_rare` 悄悄从 true 变 false。后果比判定错更隐蔽：智能档对
                // `user_rare` 是**无条件滤除**（不吃孤儿码位保底），一旦丢了这个标记，那个字
                // 又会被保底原样放回、还留在第一位。
                None => u.chars().any(|c| self.override_of(c) == Some(false)),
            })
    }

    /// 按什么粒度切这串文本去查覆盖表：**没有多码位覆盖时按 `char` 切**（等价于旧行为、
    /// 零额外开销），有才按字素簇切。
    ///
    /// ★ 这道闸门是 [`Self::is_string_common`] 能保持热路径成本的原因：它对每个候选都要
    /// 跑，而字素簇分割比逐 `char` 贵得多。绝大多数用户一个 emoji 序列都不会登记。
    fn units_of<'a>(&self, text: &'a str) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        if self.has_multi_char_keys {
            use unicode_segmentation::UnicodeSegmentation;
            Box::new(text.graphemes(true))
        } else {
            // `split("")` 会带空串，用 char_indices 手工切才干净。
            Box::new(
                text.char_indices()
                    .map(move |(i, c)| &text[i..i + c.len_utf8()]),
            )
        }
    }

    /// 字符串是否常用：其中所有「汉字」都在表内，非汉字辅助字符（标点/字母/数字/
    /// emoji/符号）忽略。空串视为非常用。
    ///
    /// 「汉字」的作用域 = [`is_han`] ∪ [`is_pua`]，两侧各自解决一类误判：
    /// - **纳入 PUA**：本码表把私用区码位**当汉字使用**（如 `dwi` 下 U+E831 冒充生僻字、
    ///   占着汉字编码排在「仄」旁边），不查表就会让无字形的垃圾候选混进「常用字/智能」档；
    /// - **`is_han` 排除 CJK 标点/符号**：`、。《》` 等虽紧邻汉字块，却与 `，`、emoji 同属
    ///   辅助符号，规范汉字表对其无从判断，查表必然失败 → 含中文顿号的词条被静默滤掉。
    ///
    /// 两者是同一个判据的两端：**「码表拿它当汉字用」才查表，「它只是符号」就忽略**，
    /// 与 Unicode 块的相邻关系无关。
    ///
    /// ★★ **用户覆盖优先于上面整套作用域判断**，且不限字符种类：`overrides` 里有这个字符，
    /// 就无条件照它说的办。默认字表只对**没被表过态**的字符说话。
    ///
    /// 这条是「词库管理全范围放开」的读端一半，缺了它写端就不能放开：注音符号 `ㄅ`、
    /// 结构描述符 `⿰`、假名 `あ` 都在 [`is_common_scope`] 之外，若仍按作用域先筛一道，
    /// 用户把它们设成生僻会**存进库里却永不被查询**——设了、存了、毫无作用，且全程无报错。
    pub fn is_string_common(&self, text: &str, cs: &crate::CharsetRegistry) -> bool {
        if text.is_empty() {
            return false;
        }
        for unit in self.units_of(text) {
            match self.overrides.get(unit) {
                // 用户显式表过态：照办，与它是不是汉字无关。
                Some(&common) => {
                    if !common {
                        return false;
                    }
                }
                // 未表态：汉字（含被本码表当汉字用的 PUA）查默认字表，其余辅助字符忽略。
                // 多码位簇**逐 char 回落**，与闸门关着时的按 char 路径给出同一答案。
                None => {
                    if !self.fallback_verdict_of(unit, cs) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// **默认字表**管辖哪些字符 = [`is_han`] ∪ [`is_pua`]。
///
/// 语义限定为「用户**没**表过态时，要不要拿默认字表来判它」：
/// - 域内查不到 ⇒ 判非常用（生僻汉字、冒充汉字的 PUA）；
/// - 域外一律忽略 ⇒ 判定上不拖累整串（标点、字母、emoji、假名、注音…）。
///
/// ⚠️ **这不是写端准入**。写端准入是 [`is_markable`]，值域比本函数宽得多——用户可以给
/// 任何字符登记常用/生僻，[`CommonChars::is_string_common`] 会优先认那条覆盖。两者的
/// 同源性由「写端放行的字符，设成生僻后读端一定判非常用」保证（`markable_char_takes_effect`
/// 钉着），而不再靠「两处调同一个函数」。
///
/// 界面仍可用它区分「这个字在默认字表里有说法」与「纯用户规则」，但不得据此拒绝写入。
pub fn is_common_scope(ch: char) -> bool {
    is_han(ch) || is_pua(ch)
}

/// 能不能给这个字符登记常用/生僻覆盖——**除空白与控制字符外一律可以**。
///
/// 刻意放到最宽：常用性覆盖的用途已经不止「这个汉字算不算常用」，还包括「这类符号我不想
/// 在候选里看到」——字根、间架结构符（`⿰⿱`）、注音、假名都是用户明确提过要能关掉的
/// （issue #83），而它们无一落在 [`is_common_scope`] 内。按作用域拒绝写入，等于把
/// 「哪些东西可以被调整」这件事**定死在代码里**，正是那条反馈要去掉的东西。
///
/// 排除空白与控制字符不是作用域判断，是数据卫生：它们不会作为候选出现，登记进去只会在
/// 列表里显示成一行空白，用户既看不出是什么、也点不掉。
pub fn is_markable(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control()
}

/// 把一串文本切成一个个「用户眼里的字符」（字素簇）。
///
/// 导入旧版那种 `rare = "⿰ㄅ"` 的整串格式时用它——**不能按 `char` 切**，那会把
/// `👨‍👩‍👧` 拆成 5 条垃圾记录。对清一色单码位的旧文件，两种切法结果相同。
///
/// 单独导出这个函数是为了把字素簇算法**收在本 crate 一处**：别的 crate 需要切分就调它，
/// 不必各自依赖 `unicode-segmentation`，也就不会各自写出一套略有出入的切法。
pub fn split_markable_clusters(text: &str) -> impl Iterator<Item = &str> {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true)
}

/// 恰好由一个 `char` 组成时返回它，否则 `None`（多码位簇没有单个 char 可查默认表）。
fn single_char_of(s: &str) -> Option<char> {
    let mut it = s.chars();
    let ch = it.next()?;
    it.next().is_none().then_some(ch)
}

/// 这串文本是不是**用户眼里的一个字符**；是则原样返回，用作覆盖表的键。
///
/// 「一个」按 **Unicode 字素簇**（UAX #29）算，不是按 `char` 数：屏幕上是一个图形的东西，
/// 码位数可以差很多——
///
/// | 形态 | 例 | 码位数 |
/// |---|---|---|
/// | 变体选择符 | `⚽️` | 2 |
/// | 肤色修饰符 | `👍🏻` | 2 |
/// | ZWJ 序列 | `👨‍👩‍👧` | 5 |
/// | 区域指示符对（国旗） | `🇨🇳` | 2 |
/// | Keycap | `1️⃣` | 3 |
///
/// ★ **用户分辨不出的差别，不该决定能不能设置。** 先前按 `chars().count() == 1` 判，
/// `⚽️` 的右键里就没有「设为生僻字」；改成「跳过修饰码位数基础字符」后 `⚽️`、`👍🏻` 通了，
/// 而 ZWJ 序列、国旗、keycap 照旧被拒——那仍是我自己列举规则的产物。
/// ⛔ **别再退回自己列规则**：issue #83 的教训就是逐条列举 Unicode 必随版本静默漏一类。
///
/// 多个字素簇一律拒绝（`我们`、`abc`）：常用性是单字属性，给词组存覆盖读端永远查不到，
/// 而取首字会让用户以为整个词都标记了。
pub fn single_markable_char(text: &str) -> Option<&str> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut it = text.graphemes(true);
    let g = it.next()?;
    if it.next().is_some() {
        return None;
    }
    // 簇内任一 char 不可登记（空白、控制字符）即整簇拒绝。
    g.chars().all(is_markable).then_some(g)
}

/// 是否「须按通用规范汉字表判定常用性」的汉字。
///
/// 判定域是**真汉字块**，外加无独立输入语义的类汉字符号（部首、笔画）——后者与 PUA 同理，
/// 在码表里占着汉字编码出现，不在规范字表内即应判非常用。
///
/// **刻意排除**（虽紧邻汉字块但属辅助符号，规范汉字表对其无从判断）：
/// - `U+2FF0..=U+2FFF` 汉字结构描述符（IDC）：`⿰⿱⿲` 等，描述间架结构而非字；
/// - `U+3000..=U+303F` CJK 符号和标点：`、。《》〈〉「」〇` 等；
/// - `U+3040..=U+30FF` 假名、`U+3100..=U+318F` 注音/谚文、`U+3190..=U+319F` 汉文标注；
/// - `U+3200..=U+33FF` 带圈与兼容符号：`① ㈱ ℃ ㎡` 等。
///
/// ⛔ **这批符号不该为了「不想看到它们」而补进本域**：那等于宣称「它们不常用」，而规范
/// 汉字表 8104 条全是纯汉字，对符号无从判断——纳入后想留某个字根就得把符号一个个加进
/// 常用字表，且「全部字符」档下它们照样全回来。想隐藏整类符号是**另一根轴**（按 Unicode
/// 类别过滤），与常用性正交，见 issue #83。
///
/// 旧实现按整段 `0x2E80..=0x33FF` 圈定（名为 `is_cjk`，对齐 Go `isCJKChar`），把上述符号
/// 当成「必须查表的汉字」，而字表里只有 8105 个纯汉字 → 中文顿号一律判非常用，用户词库中
/// 含 `、` 的词条在「常用字/智能」档被静默滤掉。**指纹是判定不自洽**：同为中文标点，
/// `、`(U+3001) 判非常用、`，`(U+FF0C) 却判常用，差别只在落没落进那段区间。
fn is_han(ch: char) -> bool {
    let c = ch as u32;
    // BMP：汉字块之间夹着假名、注音、标点，只能逐块列举；且 BMP 已排满，不会再新增汉字块。
    (0x2E80..=0x2EFF).contains(&c)        // CJK 部首补充
        || (0x2F00..=0x2FDF).contains(&c) // 康熙部首
        || (0x31C0..=0x31EF).contains(&c) // CJK 笔画
        || (0x3400..=0x4DBF).contains(&c) // 扩展 A
        || (0x4E00..=0x9FFF).contains(&c) // 基本汉字
        || (0xF900..=0xFAFF).contains(&c) // 兼容汉字
        // 平面 2（SIP）与平面 3（TIP）**整体**纳入：这两个平面专用于表意文字，扩展 B–J
        // 与兼容汉字补充全在其中，将来的扩展 K/L 亦然。
        //
        // ★ 这里刻意不逐块列举。原先写作 `0x20000..=0x323AF`（停在扩展 H 末尾），
        // Unicode 17 新增的扩展 J 从 `0x323B0` 起——**只差一个码位**就落到域外，于是
        // 恒判「常用」、在任何档下都放行：虎码跟进 Unicode 17 后，用户的常用字档里冒出
        // 一批无字形的扩 J 生僻字（issue #83）。同一份列举还漏掉了扩展 I 与兼容汉字补充。
        // 逐块列举的写法保证每升一版 Unicode 就静默漏一次，故改为按平面兜底。
        || (0x20000..=0x3FFFF).contains(&c)
}

/// 候选文本的**语义单元数**：汉字逐字计，连续的西文/数字段整体计 1。
///
/// ```text
/// 「东西」    → 2      hello        → 1
/// 「的样子」  → 3      thank you    → 2
/// 「iPhone」  → 1      「新iPhone」 → 2
/// ```
///
/// 用途：词频位置提升的准入判据（`schema.*.frequency.promote_prefix = "single"`）。
///
/// **为什么不能用 `chars().count()`**：英文候选 `hello` 有 5 个 char，按字符数判据会被
/// 「只提升单字」的规则直接挡死——而英文**所有候选都是前缀匹配**（打 `hel` 出 `hello`），
/// 那等于英文调频全灭。语义单元数让同一条规则在中英文下都成立：一个汉字、一个西文词，
/// 都是用户心智里的「一个东西」。
///
/// PUA 按汉字计——本码表把私用区码位当生僻字使用，见 [`is_pua`]。
pub fn semantic_units(text: &str) -> usize {
    let mut units = 0usize;
    let mut in_latin_word = false;
    for ch in text.chars() {
        if is_han(ch) || is_pua(ch) {
            units += 1;
            in_latin_word = false;
        } else if ch.is_whitespace() {
            in_latin_word = false;
        } else if !in_latin_word {
            units += 1;
            in_latin_word = true;
        }
    }
    units
}

/// 是否 Unicode 私用区（PUA）。本码表把 PUA 码位当汉字使用（占汉字编码、冒充生僻字），
/// 故常用性判定须把 PUA 视作「必须查表的汉字」，不在规范字表内即判非常用。
///
/// 公开是因为 [`crate::charset_registry::Scope`] 要拿它当作用域判据；那里的作用域值域是
/// **闭集**（判定域漏一段 = 过滤静默失效），只能引用代码里这份权威定义，不能各写一份。
pub fn is_pua(ch: char) -> bool {
    let c = ch as u32;
    (0xE000..=0xF8FF).contains(&c)          // BMP 私用区
        || (0xF0000..=0xFFFFD).contains(&c) // 补充私用区 A
        || (0x100000..=0x10FFFD).contains(&c) // 补充私用区 B
}

#[cfg(test)]
mod semantic_units_tests {
    use super::semantic_units;

    /// 中英混排的计数口径——词频 `promote_prefix = "single"` 全靠它区分「单字/单词」与「词组」。
    #[test]
    fn counts_han_per_char_and_latin_per_word() {
        // 中文逐字
        assert_eq!(semantic_units("东"), 1);
        assert_eq!(semantic_units("东西"), 2);
        assert_eq!(semantic_units("的样子"), 3);
        // 西文整词计 1 —— 这是英文调频能工作的前提
        assert_eq!(semantic_units("hello"), 1);
        assert_eq!(semantic_units("a"), 1);
        assert_eq!(semantic_units("thank you"), 2);
        assert_eq!(semantic_units("e-mail"), 1, "连字符不断词");
        // 混排
        assert_eq!(semantic_units("新iPhone"), 2);
        assert_eq!(semantic_units("iPhone手机"), 3);
        // 边界
        assert_eq!(semantic_units(""), 0);
        assert_eq!(semantic_units("   "), 0);
        // 数字按一段计
        assert_eq!(semantic_units("2026"), 1);
    }

    /// 扩展区汉字与 PUA 均按汉字逐字计——码表把私用区码位当生僻字使用。
    #[test]
    fn counts_ext_han_and_pua_as_chars() {
        assert_eq!(semantic_units("\u{20000}"), 1, "扩展 B");
        assert_eq!(semantic_units("\u{E000}\u{E001}"), 2, "PUA 逐字计");
    }
}

#[cfg(test)]
mod tests {
    /// 与 [`CommonChars::from_base`] 配套的 registry：出厂 `common_han` 类的形态
    /// ——作用域 = 汉字域、名单 = 这些字、名单内常用、**域内名单外生僻**。
    ///
    /// 生产路径上这两者分别来自 `schemas/common_chars.txt` 与
    /// `charsets/common_han.yaml`（后者的 `file:` 就指向前者），测试里一并造出来。
    fn han_class(chars: impl IntoIterator<Item = char>) -> crate::CharsetRegistry {
        let (reg, dropped) = crate::CharsetRegistry::compile(vec![crate::ClassSpec {
            key: "common_han".into(),
            members: chars.into_iter().map(|c| c.to_string()).collect(),
            scope: Some(crate::Scope::Han),
            default_common: Some(true),
            outside_common: Some(false),
            ..Default::default()
        }]);
        assert!(dropped.is_empty());
        reg
    }

    /// 造「基表 + 与之配套的 registry」。判定要两份数据，测试里总是成对出现。
    fn base(
        chars: impl IntoIterator<Item = char> + Clone,
    ) -> (CommonChars, crate::CharsetRegistry) {
        (CommonChars::from_base(chars.clone()), han_class(chars))
    }

    use super::*;

    #[test]
    fn test_string_common() {
        let (cc, cs) = base(['我', '们']);
        assert!(cc.is_string_common("我们", &cs)); // 全部常用
        assert!(!cc.is_string_common("我鬱", &cs)); // 含生僻
        assert!(!cc.is_string_common("", &cs)); // 空串
        assert!(cc.is_string_common("我!", &cs)); // 非汉字忽略
    }

    #[test]
    fn test_cjk_punct_ignored() {
        // 回归：用户词库里含中文顿号的词条在「常用字/智能」档被滤掉。
        // 根因＝判定域按 `0x2E80..=0x33FF` 整段圈定，把 CJK 符号和标点区当成必须查表的汉字，
        // 而字表里只有纯汉字。判据现按语义而非 Unicode 块邻接：符号一律忽略。
        let (cc, cs) = base(['我', '们']);

        assert!(cc.is_string_common("、", &cs)); // 顿号单条词条（本次上报的现象）
        assert!(cc.is_string_common("我、们", &cs)); // 混排：标点不再拖累整词判定
        for s in ["。", "《", "》", "「", "」", "〇", "；", "："] {
            assert!(cc.is_string_common(s, &cs), "CJK 标点应忽略: {s}");
        }
        // 判定自洽：同为中文标点，落不落进旧区间都该同一结果（旧实现下 、=false 而 ，=true）
        assert_eq!(
            cc.is_string_common("、", &cs),
            cc.is_string_common("，", &cs)
        );
        // 带圈/兼容符号与假名同属辅助字符，规范汉字表管不着
        assert!(cc.is_string_common("①", &cs));
        assert!(cc.is_string_common("℃", &cs));
        assert!(cc.is_string_common("あ", &cs));
        // 真汉字仍按表判定，未被本次放宽波及
        assert!(!cc.is_string_common("我鬱", &cs));
        assert!(!cc.is_string_common("、鬱", &cs)); // 标点忽略，但同串里的生僻字照旧拦下
    }

    #[test]
    fn test_pua_not_common() {
        // 回归：五笔 dwi 下 U+E831（PUA）冒充生僻字混进常用字档。PUA 被本码表当汉字用，
        // 不在规范字表内即非常用；emoji/符号等真辅助字符仍忽略。
        let (cc, cs) = base(['仄']);
        assert!(cc.is_string_common("仄", &cs)); // 真汉字在表内
        assert!(!cc.is_string_common("\u{E831}", &cs)); // PUA 单字：非常用（正是 dwi 豆腐候选）
        assert!(!cc.is_string_common("仄\u{E831}", &cs)); // 含 PUA 的混合串亦非常用
        assert!(cc.is_string_common("仄😀", &cs)); // emoji（U+1F600）非汉字：忽略，不影响判定
    }

    /// 用户覆盖两个方向都要生效，且要能穿透到整串判定。
    #[test]
    fn overrides_win_over_base_in_both_directions() {
        let (mut cc, cs) = base(['我', '们']);
        assert!(cc.is_char_common('我', &cs));
        assert!(!cc.is_char_common('鬱', &cs));

        cc.set_overrides([("我".into(), false), ("鬱".into(), true)]);
        assert!(!cc.is_char_common('我', &cs), "常用字降级为生僻");
        assert!(cc.is_char_common('鬱', &cs), "生僻字升级为常用");
        // 整串判定走同一条路：降级后的字会拖累整个词。
        assert!(!cc.is_string_common("我们", &cs));
        assert!(cc.is_string_common("鬱", &cs));
        // 出厂判定不受覆盖影响——界面要靠它显示「出厂 → 现在」的对照。
        assert!(cc.is_base_common('我', &cs));
        assert!(!cc.is_base_common('鬱', &cs));
        assert_eq!(cc.override_of('我'), Some(false));
        assert_eq!(cc.override_of('们'), None);
    }

    /// `set_overrides` 是整体替换：撤销掉的那条必须真的消失。
    ///
    /// 若实现成增量合并，「恢复出厂」在下次重灌前不会生效——症状是点了没反应，
    /// 重启后才对，属于最难查的一类（[[project_runtime_mirror_state_config_sync]]）。
    #[test]
    fn set_overrides_replaces_rather_than_merges() {
        let (mut cc, cs) = base(['我']);
        cc.set_overrides([("我".into(), false), ("鬱".into(), true)]);
        assert!(!cc.is_char_common('我', &cs));

        // 用户撤销了「我」那条，store 全量读出来只剩「鬱」。
        cc.set_overrides([("鬱".into(), true)]);
        assert!(cc.is_char_common('我', &cs), "撤销后回到出厂判定");
        assert!(cc.is_char_common('鬱', &cs));
    }

    /// ⚠️ `is_empty` 只看出厂基表：它是上层「退化为不过滤」的判据。
    ///
    /// 把覆盖也算进去的话，出厂表缺失而用户设过一个字时它会返回 false，于是过滤照常
    /// 进行——此刻几乎所有字都不在表里、全被判生僻，智能档会把候选滤到只剩那一个字。
    #[test]
    fn is_empty_ignores_overrides() {
        let (mut cc, _cs) = base([]);
        assert!(cc.is_empty());
        cc.set_overrides([("鬱".into(), true)]);
        assert!(cc.is_empty(), "有覆盖也仍算数据缺失");
    }

    /// 判定域自洽：`is_common_scope` 放行的字符，正是 `is_string_common` 会去查表的那些。
    ///
    /// 上层拿 `is_common_scope` 做右键菜单的准入。两处一旦漂移，用户就能给一个读端
    /// 根本不查的字符（`、`、emoji）存下覆盖，然后发现「设了完全没用」且毫无报错。
    #[test]
    fn common_scope_matches_string_judgement() {
        let (cc, cs) = base([]);
        for ch in ['我', '鬱', '\u{E831}', '\u{20000}', '氵'] {
            assert!(is_common_scope(ch), "{ch} 应在默认字表管辖域内");
            // 域内且不在表里 ⇒ 整串判非常用。
            assert!(
                !cc.is_string_common(&ch.to_string(), &cs),
                "{ch} 应判非常用"
            );
        }
        for ch in ['、', '，', '①', '℃', 'あ', '😀', 'A', '7'] {
            assert!(!is_common_scope(ch), "{ch} 应在默认字表管辖域外");
            // 域外**且用户没表过态** ⇒ 被忽略，不拖累判定。
            assert!(cc.is_string_common(&ch.to_string(), &cs), "{ch} 应被忽略");
        }
    }

    /// Unicode 17 的扩展 J（`U+323B0` 起）必须在管辖域内。
    ///
    /// 原先 `is_han` 逐块列举到 `0x323AF`（扩展 H 末尾），**只差一个码位**：扩 J 落到域外
    /// ⇒ 恒判常用 ⇒ 常用字档、智能档都放行。虎码跟进 Unicode 17 后，用户的常用字候选里
    /// 冒出一批无字形的扩 J 生僻字（issue #83）。同一份列举还漏了扩展 I 与兼容汉字补充。
    #[test]
    fn supplementary_ideographic_planes_are_governed_wholesale() {
        let (cc, cs) = base([]);
        for (ch, what) in [
            ('\u{323B0}', "扩展 J 首字（Unicode 17 新增）"),
            ('\u{3347F}', "扩展 J 末字"),
            ('\u{2EBF0}', "扩展 I 首字"),
            ('\u{2F800}', "兼容汉字补充"),
            ('\u{3FFFF}', "平面 3 末尾（为将来的扩展 K/L 兜底）"),
        ] {
            assert!(is_common_scope(ch), "{what} 应在管辖域内");
            assert!(
                !cc.is_string_common(&ch.to_string(), &cs),
                "{what} 应判非常用"
            );
        }
    }

    /// 写端放行的字符，设成生僻后读端**一定**判非常用。
    ///
    /// 这是「词库管理全范围放开」后取代 `common_scope_matches_string_judgement` 那条
    /// 同源性的钉子：准入不再等于读端作用域，两者的关系变成「写得进去就一定生效」。
    /// 它防的还是同一件事——库里躺着一条用户以为生效、实际永不被查询的死记录，全程无报错。
    #[test]
    fn markable_char_takes_effect() {
        // 全都在 `is_common_scope` 之外，正是 issue #83 里用户点名要能关掉的那几类。
        let outside = ['ㄅ', 'ㆠ', '⿰', 'あ', '、', '㈱', '😀', 'A'];
        for ch in outside {
            assert!(!is_common_scope(ch), "{ch} 本就在默认字表管辖域外");
            assert!(is_markable(ch), "{ch} 应可登记");

            let (mut cc, cs) = base([]);
            cc.set_overrides([(ch.to_string(), false)]);
            assert!(
                !cc.is_string_common(&ch.to_string(), &cs),
                "{ch} 设为生僻后必须判非常用，否则就是一条死记录"
            );
            assert!(cc.has_user_rare(&ch.to_string()), "{ch} 应认作用户显式降级");

            // 反向：设为常用同样被认，且不拖累整串。
            cc.set_overrides([(ch.to_string(), true)]);
            assert!(
                cc.is_string_common(&ch.to_string(), &cs),
                "{ch} 设为常用应放行"
            );
        }

        // 空白与控制字符是唯一的例外，理由是数据卫生而非作用域。
        for ch in [' ', '\t', '\u{3000}', '\u{0}'] {
            assert!(!is_markable(ch), "U+{:04X} 不该可登记", ch as u32);
        }
    }

    /// 用户眼里的「一个字符」= 一个**字素簇**，码位数可以是 1、2、3、5……
    ///
    /// 用户报的 `⚽️` 就卡在旧判据上（`chars().count() == 1`），候选右键里根本没有
    /// 「设为生僻字」这一项。词库里存的正是带 FE0F 的那个形态
    /// （`wubi86_jidian_emoji.dict.yaml` 实测）。
    ///
    /// ⛔ 中间版本改成「跳过修饰码位、数基础字符」——那仍是自己列规则，`👨‍👩‍👧`、`🇨🇳`、
    /// `1️⃣` 照样被拒。现在直接用 UAX #29 的字素簇，一次覆盖全部形态。
    #[test]
    fn one_grapheme_is_one_char_regardless_of_code_points() {
        for (text, what) in [
            ("\u{26BD}\u{FE0F}", "变体选择符（用户报的 ⚽️）"),
            ("\u{26BD}", "裸形态"),
            ("\u{1F44D}\u{1F3FB}", "肤色修饰符"),
            (
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
                "ZWJ 家庭序列",
            ),
            ("\u{1F1E8}\u{1F1F3}", "区域指示符对（国旗）"),
            ("\u{0031}\u{FE0F}\u{20E3}", "keycap"),
            ("我", "普通汉字"),
            ("ㄅ", "注音"),
        ] {
            assert_eq!(
                single_markable_char(text),
                Some(text),
                "{what} 应算作一个字符，且键就是整簇"
            );
        }

        // 多个字素簇一律拒绝：常用性是单字属性，取首簇会让用户以为整词都标记了。
        for text in ["我们", "abc", "\u{1F468}\u{1F469}"] {
            assert_eq!(single_markable_char(text), None, "{text:?} 是多个字符");
        }
        assert_eq!(single_markable_char(""), None);
        assert_eq!(single_markable_char(" "), None, "空白不可登记");
    }

    /// 多码位簇登记后，读端必须对**整簇**生效。
    ///
    /// ⚠️ 键是整簇，故 `⚽`(裸) 与 `⚽️`(带 FE0F) 是两个不同的键——用户在候选里点的是哪个
    /// 形态就设哪个，而候选文本来自词库，形态是固定的，两者不会互相错过。
    #[test]
    fn multi_code_point_cluster_override_takes_effect() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let (mut cc, cs) = base([]);
        cc.set_overrides([(family.to_string(), false)]);
        assert!(!cc.is_string_common(family, &cs), "整个 ZWJ 序列应判非常用");
        assert!(cc.has_user_rare(family));
        // 序列里的**单个成员**不受牵连：用户关的是这个组合，不是所有 👨。
        assert!(
            cc.is_string_common("\u{1F468}", &cs),
            "只登记了组合，单个成员不该跟着被判非常用"
        );

        // 混在句子里也认得出来。
        assert!(!cc.is_string_common(&format!("一家{family}"), &cs));
    }

    /// 按字判与按串判**必须一致**，域外字符尤其。
    ///
    /// 这条自洽性断言防的是一类特别难想到的账对不上：`is_char_common` 走
    /// `is_base_common`、`is_string_common` 走「域外即忽略」，两条路对同一个 `、` 若给出
    /// 相反答案，词库管理页会显示「常用」而候选窗里它已被滤掉——两边都不报错。
    /// 写端的「与默认同向就删覆盖」也建立在这份一致上（见 `is_base_common`）。
    #[test]
    fn char_and_string_judgements_agree() {
        let (mut cc, cs) = base(['我']);
        cc.set_overrides([("、".into(), false), ("鬱".into(), true)]);
        // 覆盖过的、没覆盖的、域内的、域外的各来一遍。
        for ch in [
            '我',
            '鬱',
            '、',
            '，',
            'あ',
            'ㄅ',
            '⿰',
            '😀',
            'A',
            '\u{323B0}',
        ] {
            assert_eq!(
                cc.is_char_common(ch, &cs),
                cc.is_string_common(&ch.to_string(), &cs),
                "{ch} 的按字判与按串判不一致"
            );
        }
    }

    /// 覆盖优先于默认字表——**两个方向都要**。
    #[test]
    fn override_wins_over_base_table() {
        let (mut cc, cs) = base(['我']);
        assert!(cc.is_string_common("我", &cs));
        cc.set_overrides([("我".into(), false)]);
        assert!(
            !cc.is_string_common("我", &cs),
            "默认常用的字被降级后须判非常用"
        );

        let (mut cc, cs) = base([]);
        cc.set_overrides([("鬱".into(), true)]);
        assert!(
            cc.is_string_common("鬱", &cs),
            "默认生僻的字被提升后须判常用"
        );
    }

    /// 全表列举：**按字表原序**，用户加的字追加在后。
    ///
    /// 顺序是硬要求：`common_chars.txt` 按级别拼接（一级→二级→三级），设置页分页浏览
    /// 全靠它。若从 `HashSet` 迭代，每次启动的顺序都不一样，第 2 页的内容会随机变。
    #[test]
    fn list_all_keeps_table_order_and_appends_extras() {
        let (mut cc, cs) = base(['一', '乙', '二']);
        assert_eq!(
            cc.list_all(&cs),
            vec![
                ("一".to_string(), true, true),
                ("乙".to_string(), true, true),
                ("二".to_string(), true, true)
            ],
            "无覆盖时＝字表原序，且默认与现在一致"
        );

        // 一个降级（在表内）+ 一个新增（不在表内）。
        cc.set_overrides([("乙".into(), false), ("槮".into(), true)]);
        assert_eq!(
            cc.list_all(&cs),
            vec![
                ("一".to_string(), true, true),
                // 默认仍是常用，现在被改成生僻——两个值都要保留，界面靠差异显示对照。
                ("乙".to_string(), true, false),
                ("二".to_string(), true, true),
                // 表外字追加在最后，默认判定恒 false（＝出厂表里没有它）。
                ("槮".to_string(), false, true),
            ]
        );
    }

    /// 表外字按码位排序追加：`HashMap` 迭代序随机，直接追加会让这批字每次刷新都换位置。
    #[test]
    fn list_all_orders_extras_deterministically() {
        let (mut cc, cs) = base(['一']);
        cc.set_overrides([
            ("鬱".into(), true),
            ("槮".into(), true),
            ("乂".into(), true),
        ]);
        let extras: Vec<String> = cc
            .list_all(&cs)
            .into_iter()
            .skip(1)
            .map(|(c, _, _)| c)
            .collect();
        let mut sorted = extras.clone();
        sorted.sort_unstable();
        assert_eq!(extras, sorted, "表外字必须按码位升序，顺序才稳定");
    }

    /// 字表里的重复字只列一次——历史数据里难免有重复，列两行一模一样的字很怪。
    #[test]
    fn list_all_dedupes_repeated_chars() {
        let (cc, cs) = base(['一', '乙', '一']);
        assert_eq!(cc.list_all(&cs).len(), 2);
    }

    /// 域外字符的覆盖**现在生效**（issue #83 放开写端准入的读端前提）。
    ///
    /// 本条取代旧的 `overrides_on_out_of_scope_chars_are_inert`：那条钉的是相反的行为
    /// ——域外覆盖形同虚设，因此写端必须按同一个作用域拒绝写入。用户要求词库管理全范围
    /// 放开后，那个前提被替换成了「写得进去就一定生效」，两条断言必然互斥，故整条改写而
    /// 不是加一条新的。
    #[test]
    fn overrides_on_out_of_scope_chars_take_effect() {
        let (mut cc, cs) = base(['我']);
        cc.set_overrides([("、".into(), false), ("😀".into(), false)]);
        assert!(
            !cc.is_string_common("、", &cs),
            "用户把顿号设成生僻，就该判非常用"
        );
        assert!(
            !cc.is_string_common("我😀", &cs),
            "整串含一个被降级的字符即判非常用"
        );
        // 没被表过态的域外字符照旧忽略：零回归是放开的前提。
        assert!(cc.is_string_common("我，", &cs));
    }
}
