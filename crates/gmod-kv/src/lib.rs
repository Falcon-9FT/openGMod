//! Reader for Valve's KeyValues text format: `gameinfo.txt`, materials (`.vmt`), sound
//! scripts, VGUI resource files (`.res`), `cfg/mount.cfg`, Steam's `.vdf` and `.acf` files.
//!
//! The grammar is small, but the files in the wild are not tidy, so the reader follows the
//! engine's tokenizer rather than a strict grammar:
//!
//! * A token is a quoted string, `{`, `}`, or a run of characters up to whitespace, `"`, `{`
//!   or `}`. `//` starts a comment only where a token could start: `1//x` is one token.
//! * Escape sequences (`\n`, `\"` ...) are off by default, as in the engine: materials name
//!   their textures with backslashes (`models\props_c17\chair`). Steam's own files use them
//!   ([`Options::escapes`]).
//! * An unquoted token with `[` ... `]` in it (`[$WIN32]`, `[!$X360]`) is a condition for the
//!   pair it stands next to. It is kept on the [`Pair`] and applied by
//!   [`Block::apply_conditions`].
//! * Keys are compared without regard to ASCII case and may repeat (sound scripts list
//!   several `wave` keys); the order of the file is kept.
//! * `#base` and `#include` at the top level name other files. They are returned as
//!   [`Directive`]s for the caller to load: merging needs a file system.
//! * A NUL byte ends the text.
//! * Errors do not stop the reader. Like the engine it closes the block it is in and carries
//!   on, so a broken file yields what the engine would have read from it. [`parse`] reports
//!   the first error as a failure; [`parse_lenient`] returns the document together with all
//!   errors.

#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::fmt;

mod text;

pub use text::decode_text;

/// A value: a string, or a block of pairs in braces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Block(Block),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Block(_) => None,
        }
    }

    pub fn as_block(&self) -> Option<&Block> {
        match self {
            Value::Block(b) => Some(b),
            Value::Str(_) => None,
        }
    }
}

/// One `key value` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub key: String,
    pub value: Value,
    /// The condition written next to the pair, brackets included (`[$WIN32]`).
    pub condition: Option<String>,
    /// Line of the key, counted from 1.
    pub line: u32,
}

/// The pairs between a `{` and its `}`, or the top level of a file, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    pub pairs: Vec<Pair>,
}

impl Block {
    pub fn iter(&self) -> std::slice::Iter<'_, Pair> {
        self.pairs.iter()
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The value of the first pair named `key` (ASCII case ignored, as in the engine).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.pairs.iter().find(|p| p.key.eq_ignore_ascii_case(key)).map(|p| &p.value)
    }

    /// The values of every pair named `key`, in file order.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        self.pairs.iter().filter(move |p| p.key.eq_ignore_ascii_case(key)).map(|p| &p.value)
    }

    /// The first pair named `key`, if its value is a string.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// The first pair named `key`, if its value is a block.
    pub fn block(&self, key: &str) -> Option<&Block> {
        self.get(key).and_then(Value::as_block)
    }

    /// Drops every pair whose condition does not hold, here and in all nested blocks.
    /// `defined` says whether a symbol such as `WIN32` is set; [`pc_symbol`] gives the
    /// engine's answers for this computer.
    pub fn apply_conditions(&mut self, defined: &dyn Fn(&str) -> bool) {
        self.pairs.retain(|p| p.condition.as_deref().is_none_or(|c| eval_condition(c, defined)));
        for pair in &mut self.pairs {
            if let Value::Block(block) = &mut pair.value {
                block.apply_conditions(defined);
            }
        }
    }
}

impl<'a> IntoIterator for &'a Block {
    type Item = &'a Pair;
    type IntoIter = std::slice::Iter<'a, Pair>;

    fn into_iter(self) -> Self::IntoIter {
        self.pairs.iter()
    }
}

/// `#base` merges the named file under this one (keys this file lacks are added);
/// `#include` appends the named file's top-level blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveKind {
    Base,
    Include,
}

/// A `#base "file"` or `#include "file"` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub kind: DirectiveKind,
    pub path: String,
    pub line: u32,
}

/// A whole file: its directives and its top-level pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub directives: Vec<Directive>,
    pub root: Block,
}

/// How to read a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Read `\n`, `\t`, `\\`, `\"` and the other C escapes in quoted strings.
    pub escapes: bool,
}

/// Something in the text the engine would have complained about.
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

/// Reads `text` without escape sequences and fails on the first error.
pub fn parse(text: &str) -> Result<Document, Error> {
    parse_with(text, Options::default())
}

/// Reads `text` and fails on the first error.
pub fn parse_with(text: &str, options: Options) -> Result<Document, Error> {
    let (document, mut errors) = parse_lenient(text, options);
    if errors.is_empty() { Ok(document) } else { Err(errors.swap_remove(0)) }
}

/// Reads `text` the way the engine does: whatever can be read is returned, together with
/// everything that was wrong with it.
pub fn parse_lenient(text: &str, options: Options) -> (Document, Vec<Error>) {
    let mut parser = Parser { lexer: Lexer::new(text, options.escapes), errors: Vec::new(), peeked: None, depth: 0 };
    let document = parser.document();
    (document, parser.errors)
}

/// Whether a condition such as `[$WIN32]`, `[!$X360]` or `[$WINDOWS||$OSX]` holds. `defined`
/// answers for one symbol (without `$`). `&&` binds tighter than `||`; no parentheses.
pub fn eval_condition(condition: &str, defined: &dyn Fn(&str) -> bool) -> bool {
    let inner = condition.trim();
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    inner.split("||").any(|alternative| {
        alternative.split("&&").all(|term| {
            let term = term.trim();
            let (negated, symbol) = match term.strip_prefix('!') {
                Some(rest) => (true, rest.trim()),
                None => (false, term),
            };
            defined(symbol.trim_start_matches('$')) != negated
        })
    })
}

/// The condition symbols the engine defines on this computer. `WIN32` stands for any PC,
/// whatever its system; the console symbols (`X360`, `PS3`, `GAMECONSOLE`) are never set.
pub fn pc_symbol(symbol: &str) -> bool {
    match symbol.to_ascii_uppercase().as_str() {
        "WIN32" => true,
        "WINDOWS" => cfg!(windows),
        "OSX" => cfg!(target_os = "macos"),
        "LINUX" => cfg!(target_os = "linux"),
        "POSIX" => cfg!(unix),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open,
    Close,
    /// An unquoted string.
    Bare,
    Quoted,
    /// An unquoted token with `[` ... `]`.
    Cond,
}

#[derive(Debug)]
struct Token<'a> {
    kind: Kind,
    text: Cow<'a, str>,
    line: u32,
}

impl Token<'_> {
    fn is_string(&self) -> bool {
        matches!(self.kind, Kind::Bare | Kind::Quoted)
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    line: u32,
    escapes: bool,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, escapes: bool) -> Lexer<'a> {
        let text = match text.find('\0') {
            Some(end) => &text[..end],
            None => text,
        };
        Lexer { text, pos: 0, line: 1, escapes }
    }

    fn next(&mut self, errors: &mut Vec<Error>) -> Option<Token<'a>> {
        let bytes = self.text.as_bytes();
        loop {
            while self.pos < bytes.len() && is_space(bytes[self.pos]) {
                if bytes[self.pos] == b'\n' {
                    self.line += 1;
                }
                self.pos += 1;
            }
            if bytes[self.pos..].starts_with(b"//") {
                // up to the line break, which the loop above counts
                self.pos = bytes[self.pos..].iter().position(|&c| c == b'\n').map_or(bytes.len(), |n| self.pos + n);
                continue;
            }
            break;
        }
        let start = self.pos;
        let line = self.line;
        match bytes.get(start)? {
            b'{' => {
                self.pos += 1;
                Some(Token { kind: Kind::Open, text: Cow::Borrowed("{"), line })
            }
            b'}' => {
                self.pos += 1;
                Some(Token { kind: Kind::Close, text: Cow::Borrowed("}"), line })
            }
            b'"' => Some(self.quoted(errors)),
            _ => {
                let mut end = start;
                let mut bracket = false;
                let mut condition = false;
                while let Some(&c) = bytes.get(end) {
                    if c == b'"' || c == b'{' || c == b'}' || is_space(c) {
                        break;
                    }
                    bracket |= c == b'[';
                    condition |= bracket && c == b']';
                    end += 1;
                }
                self.pos = end;
                let kind = if condition { Kind::Cond } else { Kind::Bare };
                Some(Token { kind, text: Cow::Borrowed(&self.text[start..end]), line })
            }
        }
    }

    /// A quoted string; `self.pos` is at the opening quote.
    fn quoted(&mut self, errors: &mut Vec<Error>) -> Token<'a> {
        let line = self.line;
        let start = self.pos + 1;
        let rest = &self.text[start..];
        if !self.escapes {
            let (content, after) = match rest.find('"') {
                Some(end) => (&rest[..end], start + end + 1),
                None => {
                    errors.push(Error { line, message: "string without a closing quote".into() });
                    (rest, self.text.len())
                }
            };
            self.line += content.bytes().filter(|&c| c == b'\n').count() as u32;
            self.pos = after;
            return Token { kind: Kind::Quoted, text: Cow::Borrowed(content), line };
        }
        let mut out = String::new();
        let mut chars = rest.char_indices();
        let mut after = None;
        while let Some((offset, c)) = chars.next() {
            match c {
                '"' => {
                    after = Some(start + offset + 1);
                    break;
                }
                '\\' => match chars.next() {
                    Some((_, escaped)) => {
                        if escaped == '\n' {
                            self.line += 1;
                        }
                        match escaped {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            'v' => out.push('\x0B'),
                            'b' => out.push('\x08'),
                            'r' => out.push('\r'),
                            'f' => out.push('\x0C'),
                            'a' => out.push('\x07'),
                            '\\' | '?' | '\'' | '"' => out.push(escaped),
                            other => {
                                out.push('\\');
                                out.push(other);
                            }
                        }
                    }
                    None => out.push('\\'),
                },
                '\n' => {
                    self.line += 1;
                    out.push(c);
                }
                c => out.push(c),
            }
        }
        self.pos = after.unwrap_or_else(|| {
            errors.push(Error { line, message: "string without a closing quote".into() });
            self.text.len()
        });
        Token { kind: Kind::Quoted, text: Cow::Owned(out), line }
    }
}

// ---------------------------------------------------------------------------------------
// Structure

/// Blocks nested deeper than this are skipped: text from a server must not be able to exhaust
/// the stack with `{{{{...`. Real files nest a handful of levels.
const MAX_DEPTH: usize = 128;

struct Parser<'a> {
    lexer: Lexer<'a>,
    errors: Vec<Error>,
    peeked: Option<Option<Token<'a>>>,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn next(&mut self) -> Option<Token<'a>> {
        match self.peeked.take() {
            Some(token) => token,
            None => self.lexer.next(&mut self.errors),
        }
    }

    fn peek(&mut self) -> Option<&Token<'a>> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lexer.next(&mut self.errors));
        }
        self.peeked.as_ref().and_then(Option::as_ref)
    }

    fn error(&mut self, line: u32, message: impl Into<String>) {
        self.errors.push(Error { line, message: message.into() });
    }

    fn document(&mut self) -> Document {
        let mut document = Document::default();
        while let Some(token) = self.next() {
            if token.kind == Kind::Quoted && token.text.is_empty() {
                self.error(token.line, "empty key at the top level: the rest of the file is ignored");
                break;
            }
            if token.kind == Kind::Close {
                self.error(token.line, "`}` without a matching `{`");
                continue;
            }
            let directive = match token.text.as_ref() {
                t if t.eq_ignore_ascii_case("#base") => Some(DirectiveKind::Base),
                t if t.eq_ignore_ascii_case("#include") => Some(DirectiveKind::Include),
                _ => None,
            };
            if let Some(kind) = directive {
                match self.next() {
                    Some(name) if name.is_string() && !name.text.is_empty() => {
                        document.directives.push(Directive { kind, path: name.text.into_owned(), line: token.line });
                    }
                    _ => self.error(token.line, format!("`{}` without a file name", token.text)),
                }
                continue;
            }
            let key = token.text.into_owned();
            let line = token.line;
            let mut condition = None;
            let mut next = self.next();
            if let Some(t) = &next
                && t.kind == Kind::Cond
            {
                condition = Some(t.text.to_string());
                next = self.next();
            }
            let block = match next {
                Some(t) if t.kind == Kind::Open => self.block(),
                Some(t) => {
                    // the engine loses the token it read in place of the brace
                    self.error(t.line, format!("`{key}` at the top level must be followed by `{{`"));
                    Block::default()
                }
                None => {
                    self.error(line, format!("`{key}` at the top level must be followed by `{{`, not the end of the file"));
                    Block::default()
                }
            };
            document.root.pairs.push(Pair { key, value: Value::Block(block), condition, line });
        }
        document
    }

    /// The pairs up to the `}` that closes a block whose `{` was just read.
    fn block(&mut self) -> Block {
        if self.depth >= MAX_DEPTH {
            let line = self.lexer.line;
            self.error(line, format!("blocks nested deeper than {MAX_DEPTH} levels are skipped"));
            self.skip_block();
            return Block::default();
        }
        self.depth += 1;
        let block = self.block_contents();
        self.depth -= 1;
        block
    }

    /// Consumes the rest of a block without looking into it.
    fn skip_block(&mut self) {
        let mut open = 1usize;
        while let Some(token) = self.next() {
            match token.kind {
                Kind::Open => open += 1,
                Kind::Close => {
                    open -= 1;
                    if open == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    fn block_contents(&mut self) -> Block {
        let mut block = Block::default();
        loop {
            let Some(token) = self.next() else {
                let line = self.lexer.line;
                self.error(line, "the file ends inside a block (missing `}`)");
                break;
            };
            if token.kind == Kind::Close {
                break;
            }
            if token.kind == Kind::Quoted && token.text.is_empty() {
                self.error(token.line, "empty key: the rest of the block is skipped");
                break;
            }
            let key = token.text.into_owned();
            let line = token.line;
            let mut condition = None;
            let mut next = self.next();
            if let Some(t) = &next
                && t.kind == Kind::Cond
            {
                condition = Some(t.text.to_string());
                next = self.next();
            }
            let value = match next {
                Some(t) if t.kind == Kind::Open => Value::Block(self.block()),
                Some(t) if t.is_string() => Value::Str(t.text.into_owned()),
                Some(t) => {
                    let what = if t.kind == Kind::Close { "`}`" } else { "a second condition" };
                    self.error(t.line, format!("`{key}` is followed by {what} instead of a value"));
                    block.pairs.push(Pair { key, value: Value::Str(String::new()), condition, line });
                    break;
                }
                None => {
                    self.error(line, format!("`{key}` has no value: the file ends"));
                    block.pairs.push(Pair { key, value: Value::Str(String::new()), condition, line });
                    break;
                }
            };
            block.pairs.push(Pair { key, value, condition, line });
            // a condition may also follow the value
            if self.peek().is_some_and(|t| t.kind == Kind::Cond)
                && let (Some(t), Some(pair)) = (self.next(), block.pairs.last_mut())
            {
                pair.condition = Some(t.text.into_owned());
            }
        }
        block
    }
}

// ---------------------------------------------------------------------------------------
// Writing

impl fmt::Display for Document {
    /// Writes the document back as text. Values are written without escapes, so a value
    /// that contains `"` does not survive the round trip (the format has no way to say it).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for directive in &self.directives {
            let word = match directive.kind {
                DirectiveKind::Base => "#base",
                DirectiveKind::Include => "#include",
            };
            writeln!(f, "{word} \"{}\"", directive.path)?;
        }
        write_block(f, &self.root, 0)
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_block(f, self, 0)
    }
}

fn write_block(f: &mut fmt::Formatter<'_>, block: &Block, depth: usize) -> fmt::Result {
    for pair in block {
        let indent = "\t".repeat(depth);
        let condition = pair.condition.as_deref().map(|c| format!(" {c}")).unwrap_or_default();
        match &pair.value {
            Value::Str(s) => writeln!(f, "{indent}\"{}\"\t\"{s}\"{condition}", pair.key)?,
            Value::Block(inner) => {
                writeln!(f, "{indent}\"{}\"{condition}", pair.key)?;
                writeln!(f, "{indent}{{")?;
                write_block(f, inner, depth + 1)?;
                writeln!(f, "{indent}}}")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
