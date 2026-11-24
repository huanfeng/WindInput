//! 词法分析器
//!
//! 对照 Go `wind_input/internal/cmdbar/parser/lexer.go`。扫描表达式级子集：
//! 字符串连同两侧引号一起扫描，产出的 [`TokenKind::String`] token 携带已解码的
//! `parts`（字面段 + `{expr}` 插值的原始子串，交给 parser 层再解析）。
//!
//! 关键不变量（与 Go 同）：ident 起始用 **ASCII 字节**判定，避免 UTF-8 多字节首字节
//! 被误判为字母而 scanIdent 不前进造成死循环。

use crate::error::{CmdbarError, Result};

/// 词法 token 类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Eof,
    Ident,
    Number,
    /// 字符串字面量；解码后的片段在 `Token::parts`。
    String,
    LParen,
    RParen,
    Comma,
    Dot,
    /// `{` —— 表达式位开启 options-bag ObjectLit。
    LBrace,
    /// `}` —— 关闭 options-bag ObjectLit。
    RBrace,
    /// `:` —— ObjectLit 内 key 与 value 的分隔。
    Colon,
    /// `=` —— 函数调用的具名参数分隔（`proc.run("x.exe", cwd="D:/d")`）。
    ///
    /// 刻意不复用 ObjectLit 的 `:`：`{k: v}` 是**短语级**修饰符（`$CC` 的末参，
    /// 控制候选行为），`k=v` 是**函数级**参数（控制这一次调用）。两者作用域不同，
    /// 共用一个符号会让写错位置的写法静默生效在另一层。
    Assign,
}

/// 字符串字面量的一个原始片段：字面文本或待再解析的插值原文。
#[derive(Debug, Clone, PartialEq)]
pub enum RawStringPart {
    /// 已解码的字面文本。
    Lit { text: String, offset: usize },
    /// `{...}` 之间的原始源（含义由 parser 层再 lex+parse 成表达式）。
    Interp { raw: String, offset: usize },
}

/// 词法 token。
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    /// 仅当 `kind == Number` 有效。
    pub number: f64,
    /// 仅当 `kind == String` 有效。
    pub parts: Vec<RawStringPart>,
    /// token 在源中的起始字节偏移。
    pub offset: usize,
}

impl Token {
    fn simple(kind: TokenKind, lexeme: &str, offset: usize) -> Self {
        Token {
            kind,
            lexeme: lexeme.to_string(),
            number: 0.0,
            parts: Vec::new(),
            offset,
        }
    }
}

/// 词法分析器。一次 [`Lexer::tokenize`] 消费整段输入。
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    /// 消费整段输入，返回 token 序列（末尾恒附 `Eof`）。
    pub fn tokenize(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'(' => {
                    tokens.push(Token::simple(TokenKind::LParen, "(", self.pos));
                    self.pos += 1;
                }
                b')' => {
                    tokens.push(Token::simple(TokenKind::RParen, ")", self.pos));
                    self.pos += 1;
                }
                b',' => {
                    tokens.push(Token::simple(TokenKind::Comma, ",", self.pos));
                    self.pos += 1;
                }
                b'.' => {
                    tokens.push(Token::simple(TokenKind::Dot, ".", self.pos));
                    self.pos += 1;
                }
                b'{' => {
                    // 表达式位的 '{' 开启 options-bag。字符串内的 `{...}` 由 scan_string
                    // 当作插值处理，永不走到这里。
                    tokens.push(Token::simple(TokenKind::LBrace, "{", self.pos));
                    self.pos += 1;
                }
                b'}' => {
                    tokens.push(Token::simple(TokenKind::RBrace, "}", self.pos));
                    self.pos += 1;
                }
                b':' => {
                    tokens.push(Token::simple(TokenKind::Colon, ":", self.pos));
                    self.pos += 1;
                }
                b'=' => {
                    tokens.push(Token::simple(TokenKind::Assign, "=", self.pos));
                    self.pos += 1;
                }
                b'"' | b'\'' => {
                    let tok = self.scan_string(c)?;
                    tokens.push(tok);
                }
                b'-' | b'0'..=b'9' => {
                    let tok = self.scan_number()?;
                    tokens.push(tok);
                }
                _ if is_ascii_ident_start(c) => {
                    let tok = self.scan_ident();
                    tokens.push(tok);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    return Err(CmdbarError::parse(
                        self.pos,
                        format!("unexpected character {ch:?}"),
                    ));
                }
            }
        }
        tokens.push(Token::simple(TokenKind::Eof, "", self.pos));
        Ok(tokens)
    }

    /// 扫描标识符。起始已由 ASCII 判定保证；续接允许字母/数字/下划线（含 unicode）。
    fn scan_ident(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let ch = self.src[self.pos..].chars().next().unwrap();
            if !is_ident_cont(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
        // 防御：理论上不触达（dispatch 已用 ASCII 判定收紧）。
        if self.pos == start && start < self.bytes.len() {
            self.pos += 1;
        }
        Token::simple(TokenKind::Ident, &self.src[start..self.pos], start)
    }

    /// 扫描数字：`-?digits(.digits)?`。裸 `-` 不成数字 → 报错。
    fn scan_number(&mut self) -> Result<Token> {
        let start = self.pos;
        let mut p = self.pos;
        if p < self.bytes.len() && self.bytes[p] == b'-' {
            if p + 1 >= self.bytes.len() || !self.bytes[p + 1].is_ascii_digit() {
                return Err(CmdbarError::parse(self.pos, "unexpected '-'"));
            }
            p += 1;
        }
        while p < self.bytes.len() && self.bytes[p].is_ascii_digit() {
            p += 1;
        }
        if p < self.bytes.len() && self.bytes[p] == b'.' {
            p += 1;
            let d_start = p;
            while p < self.bytes.len() && self.bytes[p].is_ascii_digit() {
                p += 1;
            }
            if p == d_start {
                return Err(CmdbarError::parse(start, "invalid number"));
            }
        }
        let lex = &self.src[start..p];
        if lex.is_empty() || lex == "-" {
            return Err(CmdbarError::parse(start, "invalid number"));
        }
        let f: f64 = lex
            .parse()
            .map_err(|_| CmdbarError::parse(start, format!("invalid number {lex:?}")))?;
        self.pos = p;
        Ok(Token {
            kind: TokenKind::Number,
            lexeme: lex.to_string(),
            number: f,
            parts: Vec::new(),
            offset: start,
        })
    }

    /// 扫描以 `quote` 起止的字符串。内部转义 `\" \\ \{ \} \( \) \n \t \r`，
    /// `{...}` 捕获为插值（嵌套大括号计数，且尊重内层字符串边界）。
    fn scan_string(&mut self, quote: u8) -> Result<Token> {
        let start = self.pos;
        self.pos += 1; // 消费开引号
        let mut parts: Vec<RawStringPart> = Vec::new();
        let mut lit = String::new();
        let mut lit_off = self.pos;

        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == quote {
                flush_lit(&mut parts, &mut lit, lit_off);
                self.pos += 1; // 消费闭引号
                return Ok(Token {
                    kind: TokenKind::String,
                    lexeme: self.src[start..self.pos].to_string(),
                    number: 0.0,
                    parts,
                    offset: start,
                });
            }
            if c == b'\\' && self.pos + 1 < self.bytes.len() {
                // 被转义字符可能是多字节（如中文路径 `E:\文档`）：按完整 UTF-8 字符推进，
                // 否则 `pos += 2` 会落进多字节字符中间，下一轮 str 切片 panic（字节边界）。
                let esc = self.src[self.pos + 1..].chars().next().unwrap();
                decode_escape(esc, &mut lit);
                self.pos += 1 + esc.len_utf8();
                continue;
            }
            // `${NAME}` 内部目录变量：先于插值判定，展开/保留都并入字面缓冲。
            if c == b'$'
                && let Some((text, next)) = scan_dir_var(self.src, self.pos)?
            {
                lit.push_str(&text);
                self.pos = next;
                continue;
            }
            if c == b'{' {
                flush_lit(&mut parts, &mut lit, lit_off);
                let brace_off = self.pos;
                let interp_start = self.pos + 1;
                let end = self.scan_interp_body(interp_start)?;
                parts.push(RawStringPart::Interp {
                    raw: self.src[interp_start..end].to_string(),
                    offset: brace_off,
                });
                self.pos = end + 1; // 跳过 '}'
                lit_off = self.pos;
                continue;
            }
            if c == b'}' {
                return Err(CmdbarError::parse(self.pos, "unmatched '}' in string"));
            }
            // 拷贝一个完整 UTF-8 字符
            let ch = self.src[self.pos..].chars().next().unwrap();
            lit.push(ch);
            self.pos += ch.len_utf8();
        }
        Err(CmdbarError::parse(start, "unclosed string"))
    }

    /// 从 `start`（`{` 之后）扫描到匹配的 `}`，返回 `}` 的字节位置。
    /// 嵌套大括号计数，内层字符串里的大括号被忽略。
    fn scan_interp_body(&self, start: usize) -> Result<usize> {
        let mut depth = 1;
        let mut p = start;
        let mut inner = 0u8; // 当前内层字符串引号，0 表示不在字符串内
        while p < self.bytes.len() && depth > 0 {
            let ch = self.bytes[p];
            if inner != 0 {
                if ch == b'\\' && p + 1 < self.bytes.len() {
                    p = skip_escaped(self.bytes, p);
                    continue;
                }
                if ch == inner {
                    inner = 0;
                }
                p += 1;
                continue;
            }
            if ch == b'"' || ch == b'\'' {
                inner = ch;
                p += 1;
                continue;
            }
            if ch == b'{' {
                depth += 1;
            } else if ch == b'}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            p += 1;
        }
        if depth != 0 {
            return Err(CmdbarError::parse(start - 1, "unclosed '{' in string"));
        }
        Ok(p)
    }
}

fn flush_lit(parts: &mut Vec<RawStringPart>, lit: &mut String, offset: usize) {
    if !lit.is_empty() {
        parts.push(RawStringPart::Lit {
            text: std::mem::take(lit),
            offset,
        });
    }
}

/// 解码字符串内一个转义字符（白名单；未知保留 `\X`，与 Go 宽松策略一致）。
/// 公开给模板/字面量路径复用（统一转义来源）。
///
/// 取**完整字符**（而非单字节）：中文等多字节字符被转义时（如 `\文`）仍能正确保留，
/// 不再把多字节 lead 字节当 Latin-1 单字节处理产生乱码 / 后续切片 panic。
/// `\$` 在白名单内：`${NAME}` 现在是内部目录变量语法（见 [`scan_dir_var`]），
/// 想让 `$` 后面紧跟一个真插值就得写 `"\${last()}"`，否则整段会被当变量处理。
pub fn decode_escape(next: char, out: &mut String) {
    match next {
        '\\' | '"' | '\'' | '{' | '}' | '(' | ')' | '$' => out.push(next),
        'n' => out.push('\n'),
        't' => out.push('\t'),
        'r' => out.push('\r'),
        _ => {
            out.push('\\');
            out.push(next);
        }
    }
}

/// 在 `pos`（须指向 `$`）处尝试识别 `${NAME}` 内部目录变量，返回
/// `Some((应写入字面缓冲的文本, `}` 之后的位置))`；`$` 后不是 `{…}` 形式则 `None`
/// （调用方按普通字面 `$` 处理）。
///
/// 两类结果都写字面、都不产生插值：
/// - `NAME` 是内部目录变量（[`wind_config::is_dir_var`]）→ 展开为绝对目录；
/// - 否则 → **原样保留 `${NAME}`**。
///
/// 「未知就留字面」是刻意的：短语文本里 `${YC}` 这类旧模板变量合法存在
/// （由 `wind_phrase::expand_template` 负责，不归命令栏管）。若把 `{NAME}` 当插值，
/// `NAME` 查不到函数 → `UnknownFunc` → **整条候选被静默丢弃**，这正是
/// `parser::has_top_level_brace` 已在顶层分流处修过一次的回归；那次只修了「要不要走
/// 命令栏」的门口判定，字符串内部仍按插值处理，于是 `open("${APP_DIR}")` 依旧哑失败。
/// 此处补齐同一豁免，两处语义至此对齐。
///
/// 变量名里出现 `}` 不可能（变量名集是固定白名单），故用字节 `find` 定位闭合括号；
/// `$`/`{`/`}` 均为 ASCII，切出的 `name` 边界天然落在 UTF-8 字符边界上。
pub(crate) fn scan_dir_var(src: &str, pos: usize) -> Result<Option<(String, usize)>> {
    let b = src.as_bytes();
    if b.get(pos) != Some(&b'$') || b.get(pos + 1) != Some(&b'{') {
        return Ok(None);
    }
    let name_start = pos + 2;
    let Some(rel) = src[name_start..].find('}') else {
        // 未闭合：交回调用方按字面 `$` + 后续 `{` 走原有插值路径（含其未闭合报错）。
        return Ok(None);
    };
    let name = &src[name_start..name_start + rel];
    let after = name_start + rel + 1;
    if !wind_config::is_dir_var(name) {
        return Ok(Some((format!("${{{name}}}"), after)));
    }
    // 是内部目录变量却定位不到目录：展开成空串会静默拼出错误路径（如把文件落到
    // 盘根），故硬失败。能走到这里说明用户目录探测已经坏了，属真异常而非用法问题。
    let dir = wind_config::dir_var_str(name)
        .ok_or_else(|| CmdbarError::parse(pos, format!("无法定位目录变量 ${{{name}}}")))?;
    Ok(Some((dir, after)))
}

/// UTF-8 lead 字节 → 该字符的字节数（1~4）。
pub(crate) fn utf8_char_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead < 0xE0 {
        2
    } else if lead < 0xF0 {
        3
    } else {
        4
    }
}

/// 字节级扫描器遇到反斜杠时的推进：跳过 `\`（1 字节）+ 其后**完整** UTF-8 字符。
/// `i` 须指向反斜杠且保证 `i + 1 < b.len()`。用固定 `+2` 会在被转义字符为多字节时
/// 错位地把后续字节误读成引号/括号/逗号等分隔符（边界统计错乱）。
pub(crate) fn skip_escaped(b: &[u8], i: usize) -> usize {
    (i + 1 + utf8_char_len(b[i + 1])).min(b.len())
}

/// 仅判定 ASCII 字节是否为 ident 起始（dispatch 阶段必须字节级判定）。
fn is_ascii_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

/// ident 续接：下划线 / 字母 / 数字（含 unicode，对齐 Go isIdentCont）。
fn is_ident_cont(r: char) -> bool {
    r == '_' || r.is_alphabetic() || r.is_numeric()
}

#[cfg(test)]
#[path = "lexer_tests.rs"]
mod tests;
