//! Garry's Mod's Lua dialect ("GLua") turned into plain LuaJIT source.
//!
//! GMod runs LuaJIT (Lua 5.1 with `goto`) with a few additions taken from C:
//!
//! | GLua | becomes |
//! |---|---|
//! | `a != b` | `a ~= b` |
//! | `!a` | `not a` |
//! | `a && b`, `a \|\| b` | `a and b`, `a or b` |
//! | `// comment`, `/* comment */` | blanks (line breaks kept) |
//! | `continue` | `goto` to a label at the end of the loop body |
//! | `DEFINE_BASECLASS("name")` | `local BaseClass = baseclass.Get("name")` |
//!
//! [`translate`] rewrites a chunk token by token and moves nothing to another line, so the
//! line numbers in LuaJIT's error messages and tracebacks are those of the original file.
//! It works on bytes: Lua source is not always UTF-8 (old addons are full of Windows-1251
//! and Windows-1252 text in strings), and LuaJIT does not mind. A chunk without any of the
//! additions comes back unchanged and borrowed.
//!
//! `continue` in a `while` or `for` loop becomes `goto label`, and the loop body is wrapped
//! as `do <body> end ::label::`. The extra block puts the label outside the scope of the
//! body's locals (a `goto` may not jump into the scope of a local) and keeps a `return` or
//! `break` at the end of the body the last statement of its block. A `repeat ... until`
//! body cannot be wrapped, because its condition sees the body's locals; there the label
//! goes right before `until`, and LuaJIT refuses the jump when a local is declared between
//! the `continue` and the `until`.
//!
//! `continue` is the keyword only where a statement can stand and the block ends right
//! after it (`continue end`, `continue else`, `continue;` ...). Elsewhere - `t.continue`,
//! `local continue`, `goto continue`, `::continue::` - it stays a name, as in LuaJIT.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::fmt;

/// A chunk that cannot be translated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Line counted from 1.
    pub line: u32,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for Error {}

/// Translates a GLua chunk into LuaJIT source with the same lines. Problems LuaJIT itself
/// would report (an unfinished string, a stray symbol) are left for it to report; only what
/// the translation itself cannot handle is an [`Error`].
pub fn translate(src: &[u8]) -> Result<Cow<'_, [u8]>, Error> {
    let mut edits = Vec::new();
    let tokens = lex(src, &mut edits)?;
    structure(src, &tokens, &mut edits)?;
    if edits.is_empty() {
        return Ok(Cow::Borrowed(src));
    }
    Ok(Cow::Owned(apply(src, edits)))
}

/// [`translate`] for text known to be UTF-8; the result is UTF-8 too.
pub fn translate_str(src: &str) -> Result<Cow<'_, str>, Error> {
    Ok(match translate(src.as_bytes())? {
        Cow::Borrowed(_) => Cow::Borrowed(src),
        Cow::Owned(bytes) => {
            Cow::Owned(String::from_utf8(bytes).expect("only ASCII is inserted, and only whole tokens and comments are removed"))
        }
    })
}

/// Whether a chunk uses any of GMod's additions.
pub fn uses_extensions(src: &[u8]) -> bool {
    !matches!(translate(src), Ok(Cow::Borrowed(_)))
}

// ---------------------------------------------------------------------------------------
// Edits

struct Edit {
    start: usize,
    end: usize,
    text: Cow<'static, str>,
}

impl Edit {
    fn replace(start: usize, end: usize, text: impl Into<Cow<'static, str>>) -> Edit {
        Edit { start, end, text: text.into() }
    }

    fn insert(at: usize, text: impl Into<Cow<'static, str>>) -> Edit {
        Edit { start: at, end: at, text: text.into() }
    }
}

fn apply(src: &[u8], mut edits: Vec<Edit>) -> Vec<u8> {
    // stable: of insertions at one place, the one made first comes first; an insertion comes
    // before a replacement starting at the same place
    edits.sort_by_key(|e| (e.start, e.end));
    let mut out = Vec::with_capacity(src.len() + edits.len() * 8);
    let mut pos = 0;
    for edit in edits {
        debug_assert!(edit.start >= pos, "overlapping edits");
        out.extend_from_slice(&src[pos..edit.start]);
        out.extend_from_slice(edit.text.as_bytes());
        pos = edit.end;
    }
    out.extend_from_slice(&src[pos..]);
    out
}

// ---------------------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A name or a keyword.
    Name,
    Number,
    /// A short or a long string.
    Str,
    Punct,
}

#[derive(Debug, Clone, Copy)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
    line: u32,
}

/// LuaJIT's identifier characters: letters, digits, `_` and every byte from 0x80 up.
fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Length of the line break at `i` (`\n`, `\r`, `\r\n` or `\n\r`, as LuaJIT counts them), or 0.
fn line_break(src: &[u8], i: usize) -> usize {
    match src.get(i) {
        Some(&c @ (b'\n' | b'\r')) => match src.get(i + 1) {
            Some(&d @ (b'\n' | b'\r')) if d != c => 2,
            _ => 1,
        },
        _ => 0,
    }
}

/// Line breaks in `src[start..end]`.
fn count_lines(src: &[u8], start: usize, end: usize) -> u32 {
    let mut lines = 0;
    let mut i = start;
    while i < end {
        match line_break(src, i) {
            0 => i += 1,
            n => {
                lines += 1;
                i += n;
            }
        }
    }
    lines
}

/// The level of a long bracket opening at `i` (`[[`: 0, `[==[`: 2), if one does.
fn long_bracket(src: &[u8], i: usize) -> Option<usize> {
    if src.get(i) != Some(&b'[') {
        return None;
    }
    let level = src[i + 1..].iter().take_while(|&&c| c == b'=').count();
    (src.get(i + 1 + level) == Some(&b'[')).then_some(level)
}

/// The end of a long bracket of `level` whose contents start at `from`.
fn long_bracket_end(src: &[u8], from: usize, level: usize) -> Option<usize> {
    let mut i = from;
    while i < src.len() {
        if src[i] == b']' && src[i + 1..].iter().take_while(|&&c| c == b'=').count() >= level && src.get(i + 1 + level) == Some(&b']') {
            return Some(i + level + 2);
        }
        i += 1;
    }
    None
}

/// The tokens of a chunk. GLua's operators and comments are turned into edits on the way.
/// Lexing stops quietly where LuaJIT would report an error of its own (an unfinished string
/// or long comment): the chunk will not compile anyway, and LuaJIT says why better.
fn lex(src: &[u8], edits: &mut Vec<Edit>) -> Result<Vec<Token>, Error> {
    let mut tokens = Vec::new();
    let mut line = 1u32;
    let mut i = 0;
    if src.starts_with(&[0xEF, 0xBB, 0xBF]) {
        edits.push(Edit::replace(0, 3, ""));
        i = 3;
    }
    let n = src.len();
    while i < n {
        let c = src[i];
        let start = i;
        match c {
            b'\n' | b'\r' => {
                i += line_break(src, i);
                line += 1;
            }
            b' ' | b'\t' | 0x0B | 0x0C => i += 1,
            b'-' if src.get(i + 1) == Some(&b'-') => {
                i += 2;
                if let Some(level) = long_bracket(src, i) {
                    let Some(end) = long_bracket_end(src, i + level + 2, level) else { break };
                    line += count_lines(src, i, end);
                    i = end;
                } else {
                    while i < n && line_break(src, i) == 0 {
                        i += 1;
                    }
                }
            }
            b'/' if src.get(i + 1) == Some(&b'/') => {
                while i < n && line_break(src, i) == 0 {
                    i += 1;
                }
                edits.push(Edit::replace(start, i, " "));
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                let Some(close) = src[i + 2..].windows(2).position(|w| w == b"*/") else {
                    return Err(Error { line, message: "unfinished /* comment */".into() });
                };
                let end = i + 2 + close + 2;
                // keep the line breaks so that everything after stays on its line
                let mut blank = String::from(" ");
                blank.extend(src[start..end].iter().filter(|&&b| b == b'\n' || b == b'\r').map(|&b| b as char));
                line += count_lines(src, start, end);
                edits.push(Edit::replace(start, end, blank));
                i = end;
            }
            b'"' | b'\'' => {
                i += 1;
                let mut finished = false;
                while i < n {
                    match src[i] {
                        b'\\' => {
                            i += 1;
                            match src.get(i) {
                                Some(b'\n' | b'\r') => {
                                    i += line_break(src, i);
                                    line += 1;
                                }
                                Some(b'z') => {
                                    i += 1;
                                    while i < n && (src[i].is_ascii_whitespace() || src[i] == 0x0B) {
                                        match line_break(src, i) {
                                            0 => i += 1,
                                            k => {
                                                i += k;
                                                line += 1;
                                            }
                                        }
                                    }
                                }
                                Some(_) => i += 1,
                                None => {}
                            }
                        }
                        b'\n' | b'\r' => break,
                        q if q == c => {
                            i += 1;
                            finished = true;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                if !finished {
                    break;
                }
                tokens.push(Token { kind: Kind::Str, start, end: i, line });
            }
            b'[' if long_bracket(src, i).is_some() => {
                let level = long_bracket(src, i).unwrap_or(0);
                let Some(end) = long_bracket_end(src, i + level + 2, level) else { break };
                let token_line = line;
                line += count_lines(src, i, end);
                tokens.push(Token { kind: Kind::Str, start, end, line: token_line });
                i = end;
            }
            b'0'..=b'9' => {
                i = number_end(src, i);
                tokens.push(Token { kind: Kind::Number, start, end: i, line });
            }
            b'.' if src.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                i = number_end(src, i + 1);
                tokens.push(Token { kind: Kind::Number, start, end: i, line });
            }
            c if is_ident(c) => {
                while i < n && is_ident(src[i]) {
                    i += 1;
                }
                tokens.push(Token { kind: Kind::Name, start, end: i, line });
            }
            _ => {
                let rest = &src[i..];
                let (len, replacement): (usize, Option<&'static str>) = if rest.starts_with(b"!=") {
                    (2, Some("~="))
                } else if rest.starts_with(b"&&") {
                    (2, Some(" and "))
                } else if rest.starts_with(b"||") {
                    (2, Some(" or "))
                } else if c == b'!' {
                    (1, Some(" not "))
                } else if rest.starts_with(b"...") {
                    (3, None)
                } else if [&b".."[..], b"::", b"==", b"~=", b"<=", b">="].iter().any(|op| rest.starts_with(op)) {
                    (2, None)
                } else {
                    (1, None)
                };
                if let Some(text) = replacement {
                    edits.push(Edit::replace(start, start + len, text));
                }
                i += len;
                tokens.push(Token { kind: Kind::Punct, start, end: i, line });
            }
        }
    }
    Ok(tokens)
}

/// The end of a number starting at `i`, as LuaJIT's lexer takes it: identifier characters and
/// dots, and a sign right after an exponent mark (`e`, or `p` in hexadecimal).
fn number_end(src: &[u8], i: usize) -> usize {
    let first = src[i];
    let exponent = if first == b'0' && src.get(i + 1).is_some_and(|&c| c | 0x20 == b'x') { b'p' } else { b'e' };
    let mut prev = first;
    let mut j = i;
    while let Some(&c) = src.get(j) {
        if is_ident(c) || c == b'.' || ((c == b'-' || c == b'+') && prev | 0x20 == exponent) {
            prev = c;
            j += 1;
        } else {
            break;
        }
    }
    j
}

// ---------------------------------------------------------------------------------------
// Blocks and `continue`

enum Frame {
    Function,
    /// `if ... end`, `do ... end`.
    Block,
    /// `while`/`for` up to its `do`.
    LoopHead,
    /// A loop body: where it starts (after `do`) and the label of its `continue`, if any.
    Loop {
        body: usize,
        label: Option<u32>,
    },
    Repeat {
        label: Option<u32>,
    },
}

fn label(n: u32) -> String {
    format!("__glua_continue_{n}")
}

fn structure(src: &[u8], tokens: &[Token], edits: &mut Vec<Edit>) -> Result<(), Error> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut labels = 0u32;
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Name {
            continue;
        }
        match &src[token.start..token.end] {
            b"function" => stack.push(Frame::Function),
            b"if" => stack.push(Frame::Block),
            b"while" | b"for" => stack.push(Frame::LoopHead),
            b"do" => match stack.last_mut() {
                Some(frame @ Frame::LoopHead) => *frame = Frame::Loop { body: token.end, label: None },
                _ => stack.push(Frame::Block),
            },
            b"repeat" => stack.push(Frame::Repeat { label: None }),
            b"end" => {
                if let Some(Frame::Loop { body, label: Some(n) }) = stack.pop() {
                    edits.push(Edit::insert(body, " do"));
                    edits.push(Edit::insert(token.start, format!("end ::{}:: ", label(n))));
                }
            }
            b"until" => {
                if let Some(Frame::Repeat { label: Some(n) }) = stack.pop() {
                    edits.push(Edit::insert(token.start, format!("::{}:: ", label(n))));
                }
            }
            // GMod's one macro: a statement that defines the local `BaseClass`
            b"DEFINE_BASECLASS" if is_macro_call(src, tokens, index) => {
                edits.push(Edit::replace(token.start, token.end, "local BaseClass = baseclass.Get"));
            }
            b"continue" if is_continue_statement(src, tokens, index) => {
                let mut target = None;
                for frame in stack.iter_mut().rev() {
                    match frame {
                        Frame::Loop { label, .. } | Frame::Repeat { label } => {
                            target = Some(label);
                            break;
                        }
                        Frame::Function => break,
                        Frame::Block | Frame::LoopHead => {}
                    }
                }
                let Some(target) = target else {
                    return Err(Error { line: token.line, message: "'continue' outside a loop".into() });
                };
                let n = *target.get_or_insert_with(|| {
                    labels += 1;
                    labels
                });
                edits.push(Edit::replace(token.start, token.end, format!("goto {}", label(n))));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether the name at `index` is called as a statement of its own: `(` follows, and it is
/// not a field (`t.DEFINE_BASECLASS(...)`).
fn is_macro_call(src: &[u8], tokens: &[Token], index: usize) -> bool {
    let text = |t: &Token| &src[t.start..t.end];
    let called = tokens.get(index + 1).is_some_and(|next| text(next) == b"(");
    let field = index.checked_sub(1).is_some_and(|p| matches!(text(&tokens[p]), b"." | b":" | b"::"));
    called && !field
}

/// Whether the `continue` at `index` is the statement rather than a name: the block must
/// end right after it, and what comes before it must not expect an operand or a name.
fn is_continue_statement(src: &[u8], tokens: &[Token], index: usize) -> bool {
    let text = |t: &Token| &src[t.start..t.end];
    let ends_block = tokens.get(index + 1).is_some_and(|next| matches!(text(next), b"end" | b"else" | b"elseif" | b"until" | b";"));
    if !ends_block {
        return false;
    }
    let Some(prev) = index.checked_sub(1).map(|p| &tokens[p]) else { return true };
    match prev.kind {
        // `.`, `:`, `::`, `=`, `,`, `(`, operators: a name or an operand follows
        Kind::Punct => matches!(text(prev), b")" | b"]" | b"}" | b";" | b"..."),
        Kind::Name => !matches!(text(prev), b"goto" | b"local" | b"function" | b"for" | b"return" | b"and" | b"or" | b"not" | b"in"),
        Kind::Number | Kind::Str => true,
    }
}

#[cfg(test)]
mod tests;
