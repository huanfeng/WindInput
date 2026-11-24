//! 词库多段导入导出编排（用户词库 / 临时词库 / 词频 / 候选调整）。
//!
//! 把 store 各数据域原语组合成「一个 .wdict 文件多段、按类型可选」的导入导出，对齐旧 Go
//! wind_dict 一文件多段的形态。段与引擎类型的适用关系（码表四段 / 混输仅候选调整 /
//! 拼音三段）由调用方（RPC / UI）决定，本模块只按传入的 sections 处理。

use crate::store::Store;
use crate::user_words::WordsImportCounts;
use crate::wdict::{self, DictWdict, FreqIo, WordIo};

/// 词库数据段类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictSection {
    /// 用户词库
    UserWords,
    /// 临时词库
    TempWords,
    /// 词频
    Freq,
    /// 候选调整（shadow）
    Shadow,
}

impl DictSection {
    /// RPC/UI 稳定标识（camelCase）。
    pub fn key(&self) -> &'static str {
        match self {
            DictSection::UserWords => "userWords",
            DictSection::TempWords => "tempWords",
            DictSection::Freq => "freq",
            DictSection::Shadow => "shadow",
        }
    }

    /// wdict 段标签（`--- !<tag>`）。
    pub fn tag(&self) -> &'static str {
        match self {
            DictSection::UserWords => "words",
            DictSection::TempWords => "temp_words",
            DictSection::Freq => "freq",
            DictSection::Shadow => "shadow",
        }
    }

    /// 从 RPC key / wdict 段名解析（宽松：兼容多种写法）。
    pub fn from_key(k: &str) -> Option<Self> {
        match k {
            "userWords" | "words" | "user_words" => Some(DictSection::UserWords),
            "tempWords" | "temp_words" => Some(DictSection::TempWords),
            "freq" => Some(DictSection::Freq),
            "shadow" => Some(DictSection::Shadow),
            _ => None,
        }
    }
}

/// 单段导入结果（供 RPC 序列化）。
#[derive(Debug, Clone, Default)]
pub struct SectionImport {
    pub key: &'static str,
    /// 用户词库分类计数（仅 UserWords）。
    pub words: Option<WordsImportCounts>,
    /// 其余段的导入条数。
    pub imported: usize,
    pub skipped: usize,
}

/// 多段导入结果。
#[derive(Debug, Clone, Default)]
pub struct DictImportReport {
    pub sections: Vec<SectionImport>,
}

impl Store {
    /// 词频记录的导出编码：查用户词 / 临时词表补出音节边界，渲染成带空格的音节码。
    ///
    /// 词频表是唯一不带 boundary 的持久层（value 仅 `count + last_used`），边界只能反查。
    /// **store 层查不到系统词典**（那需要 engine），故系统词的词频记录保持扁平码——
    /// 设置页列表另有一条经引擎的反查路径（`Coordinator::freq_display_code`）覆盖得更全。
    ///
    /// 查不到即原样返回，不猜。
    fn spaced_freq_code(&self, schema: &str, code: &str, text: &str) -> String {
        let pick = |recs: Vec<crate::user_words::UserWordRecord>| {
            recs.into_iter()
                .find(|w| w.text == text)
                .map(|w| w.boundary)
                .filter(|b| *b != 0)
        };
        let b = self
            .get_user_words(schema, code)
            .ok()
            .and_then(pick)
            .or_else(|| self.get_temp_words(schema, code).ok().and_then(pick))
            .unwrap_or(0);
        crate::wdict::join_code_by_boundary(code, b)
    }

    /// 导出所选数据段为单个多段 wdict 文本。缺数据的段仍会写出（空段），保证文件自描述。
    /// `engine_type` 写入头部供导入时校验（防跨引擎类型误导）；调用方（RPC）解析后传入。
    pub fn export_dict_sections_wdict(
        &self,
        schema: &str,
        sections: &[DictSection],
        exported_at: &str,
        engine_type: &str,
    ) -> anyhow::Result<String> {
        let mut d = DictWdict::default();
        for sec in sections {
            match sec {
                DictSection::UserWords => d.words = Some(self.collect_user_word_rows(schema)?),
                DictSection::TempWords => {
                    let recs = self.search_temp_words_prefix(schema, "", 0)?;
                    d.temp_words = Some(
                        recs.into_iter()
                            // 带空格的音节码，与 collect_user_word_rows 同款（导入端
                            // import_temp_word_rows 会拆回 flat + 边界）。
                            .map(|r| WordIo {
                                code: wdict::join_code_by_boundary(&r.code, r.boundary),
                                text: r.text,
                                weight: r.weight,
                                count: r.count,
                                boundary: None,
                            })
                            .collect(),
                    );
                }
                DictSection::Freq => {
                    let (rows, _total) = self.list_freq_paged(schema, "", 0, 0)?;
                    d.freq = Some(
                        rows.into_iter()
                            .map(|(code, text, rec)| FreqIo {
                                // 与 words / temp_words 两段同形，输出带空格的音节码。
                                // 词频表自己不存 boundary，只能反查——这里只查得到用户词与
                                // 临时词（store 层没有系统词典），系统词的记录保持扁平。
                                // 对功能无影响：导入端会拆回扁平 key，词频表也不存边界；
                                // 带空格纯为文件可读性与三段格式一致。
                                code: self.spaced_freq_code(schema, &code, &text),
                                text,
                                count: rec.count,
                                last_used: rec.last_used,
                            })
                            .collect(),
                    );
                }
                DictSection::Shadow => d.shadow = Some(self.export_shadow_actions(schema)?),
            }
        }
        Ok(wdict::export_dict_sections(
            &d,
            exported_at,
            schema,
            engine_type,
        ))
    }

    /// 从多段 wdict 文本导入所选数据段。**只处理文件中实际存在的段**（防 replace 误清空
    /// 文件未携带的段）。replace=先清该段再导入；否则 Merge。
    ///
    /// `contract` 是**词条准入闸口**：解析之后、落库之前，对 `words` 与 `temp_words` 两段
    /// 的行做过滤与补齐（拼音词条的音节边界求解在此发生）。
    ///
    /// ★ 为什么是注入而不是在本层实现：准入判据要跑引擎的求解链，而 `wind-store` 拿不到
    /// `engine_mgr`（同 `CodePolicy` 的理由）。本层只负责「少了几行」，不关心为什么少。
    /// 被闸口丢掉的行计入该段的 `skipped`，`imported` 取闸口之后的条数。
    ///
    /// ⚠️ `freq` 段**有意不过闸**：词频表按既定决策不带 boundary 字段，且它的 code 来自
    /// 用户既有词条、不是新入库的词。`shadow` 段不是词条。
    pub fn import_dict_sections_wdict(
        &self,
        schema: &str,
        text: &str,
        sections: &[DictSection],
        replace: bool,
        contract: &mut dyn FnMut(DictSection, Vec<wdict::WordIo>) -> Vec<wdict::WordIo>,
    ) -> anyhow::Result<DictImportReport> {
        let present = wdict::sections_present(text);
        let has = |tag: &str| present.iter().any(|t| t == tag);
        let mut rep = DictImportReport::default();
        for sec in sections {
            if !has(sec.tag()) {
                continue; // 文件无此段，跳过（不清空、不计入）
            }
            match sec {
                DictSection::UserWords => {
                    let (rows, skipped) =
                        wdict::parse_words_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
                    let parsed = rows.len();
                    let rows = contract(*sec, rows);
                    let rejected = parsed - rows.len();
                    if replace {
                        self.clear_user_words(schema)?;
                    }
                    let counts = self.import_user_words(schema, &rows)?;
                    rep.sections.push(SectionImport {
                        key: sec.key(),
                        words: Some(counts),
                        imported: rows.len(),
                        skipped: skipped + rejected,
                    });
                }
                DictSection::TempWords => {
                    let (rows, skipped) =
                        wdict::parse_temp_words_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
                    let parsed = rows.len();
                    // 临时词同样过闸：它会随 `promote_temp_word` 晋升成用户词并**带着
                    // boundary 一起走**，放它进来等于给不变量开一条绕行路。
                    let rows = contract(*sec, rows);
                    let rejected = parsed - rows.len();
                    if replace {
                        self.clear_temp_words(schema)?;
                    }
                    let n = self.import_temp_word_rows(schema, &rows)?;
                    rep.sections.push(SectionImport {
                        key: sec.key(),
                        words: None,
                        imported: n,
                        skipped: skipped + rejected,
                    });
                }
                DictSection::Freq => {
                    let (rows, skipped) =
                        wdict::parse_freq_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
                    if replace {
                        self.clear_freq(schema)?;
                        // 联想历史与词频同属「选词学习」，.wdict 不带它：替换词频即一并清空，
                        // 免得留下与新词频不同源的旧历史（同备份还原的 freq 段）。
                        self.clear_assoc_history(schema)?;
                    }
                    let n = self.import_freq_rows(schema, &rows)?;
                    rep.sections.push(SectionImport {
                        key: sec.key(),
                        words: None,
                        imported: n,
                        skipped,
                    });
                }
                DictSection::Shadow => {
                    let (actions, skipped) =
                        wdict::parse_shadow_wdict(text).map_err(|e| anyhow::anyhow!(e))?;
                    if replace {
                        self.clear_shadow(schema)?;
                    }
                    let n = self.import_shadow_actions(schema, &actions)?;
                    rep.sections.push(SectionImport {
                        key: sec.key(),
                        words: None,
                        imported: n,
                        skipped,
                    });
                }
            }
        }
        Ok(rep)
    }
}

#[cfg(test)]
#[path = "dict_export_tests.rs"]
mod tests;
