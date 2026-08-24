//! Source text -> tokens.
//!
//! The lexer recognises far more than the M0 subset can lower, and that is
//! deliberate. A tokenizer that stopped at the first unknown byte could only
//! ever say "unexpected character"; this one reads the whole `"hello"`, the
//! whole `0x10`, the whole `` `t${x}` ``, and hands the parser a token that
//! already knows how to name itself. That is what makes the capability
//! diagnostics in [`crate::diag`] possible.
//!
//! Out-of-subset lexemes become [`TokenKind::Unsupported`] carrying the noun
//! phrase for the diagnostic and the [`Boundary`] it ran into. As milestones
//! land, those lexemes graduate into real token kinds one at a time; nothing
//! else about the lexer changes.

use crate::diag::{Boundary, CompileError, malformed};

/// One lexeme and where it starts, in bytes from the start of the source.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) offset: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    /// A decimal integer literal, as its unsigned magnitude. The sign belongs
    /// to the parser: `-2147483648` is a unary minus applied to a magnitude
    /// that does not fit in an `i32` on its own.
    Int(u64),
    /// `$N` -- the Nth argument of this call. Held wide so an absurd index is
    /// a bounds decision in the parser rather than a silent wrap here.
    Arg(u64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Semi,
    /// A name. Whether one means anything is not the lexer's business: the
    /// language has no bindings to resolve it against, and the `eval_wasm`
    /// skin resolves it to a host import. See [`crate::Names`].
    Ident(String),
    /// Real JavaScript this engine does not lower yet.
    Unsupported(Unlowered),
    Eof,
}

/// A lexeme the engine can name but not lower: the noun phrase for "this
/// engine does not support {phrase} yet", and which boundary it ran into.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Unlowered {
    pub(crate) boundary: Boundary,
    pub(crate) phrase: String,
}

impl TokenKind {
    /// A short name for use inside a [`crate::diag::malformed`] sentence.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Int(_) => "a number",
            Self::Arg(_) => "an argument reference",
            Self::Plus => "a `+`",
            Self::Minus => "a `-`",
            Self::Star => "a `*`",
            Self::Slash => "a `/`",
            Self::Percent => "a `%`",
            Self::LParen => "a `(`",
            Self::RParen => "a `)`",
            Self::Semi => "a `;`",
            Self::Ident(_) => "a name",
            Self::Unsupported(_) => "an unsupported construct",
            Self::Eof => "the end of the source",
        }
    }
}

/// Tokenize the whole source. Always ends with exactly one [`TokenKind::Eof`].
///
/// Fails only on input the lexer cannot finish reading at all (an unclosed
/// block comment); everything else it can name, it names.
pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, CompileError> {
    let mut lexer = Lexer {
        src: source,
        bytes: source.as_bytes(),
        pos: 0,
    };
    let mut tokens = Vec::new();
    loop {
        lexer.skip_trivia()?;
        let offset = lexer.pos;
        if offset >= lexer.bytes.len() {
            tokens.push(Token {
                kind: TokenKind::Eof,
                offset,
            });
            return Ok(tokens);
        }
        let kind = lexer.lexeme();
        tokens.push(Token { kind, offset });
    }
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    /// Whitespace, line terminators, and both comment forms.
    fn skip_trivia(&mut self) -> Result<(), CompileError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) => self.pos += 1,
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while !matches!(self.peek(), None | Some(b'\n' | b'\r')) {
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let opened = self.pos;
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => {
                                return Err(malformed(
                                    &format!(
                                        "needs a `*/` to close the comment opened at byte {opened}; the source ends first"
                                    ),
                                    opened,
                                ));
                            }
                            Some(b'*') if self.peek_at(1) == Some(b'/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(_) => self.pos += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// One lexeme, starting at a byte that is neither whitespace nor a comment.
    fn lexeme(&mut self) -> TokenKind {
        let byte = self.bytes[self.pos];
        match byte {
            b'0'..=b'9' => self.number(),
            b'$' if matches!(self.peek_at(1), Some(b'0'..=b'9')) => self.argument(),
            b'A'..=b'Z' | b'a'..=b'z' | b'_' | b'$' => self.word(),
            b'"' | b'\'' => self.quoted(byte, "string literals"),
            b'`' => self.quoted(b'`', "template literals"),
            _ => self.punctuation(byte),
        }
    }

    /// A numeric literal in any JavaScript form. Only plain decimal integers
    /// that fit an `i32` survive; the rest name themselves.
    fn number(&mut self) -> TokenKind {
        let start = self.pos;
        if self.bytes[start] == b'0' {
            let phrase = match self.peek_at(1) {
                Some(b'x' | b'X') => Some("hexadecimal number literals"),
                Some(b'o' | b'O') => Some("octal number literals"),
                Some(b'b' | b'B') => Some("binary number literals"),
                _ => None,
            };
            if let Some(phrase) = phrase {
                self.pos += 2;
                self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'_');
                return subset(phrase);
            }
        }
        self.eat_while(|b| b.is_ascii_digit());
        // The suffixes that turn a decimal integer into something else. Each
        // is consumed whole so the token spans the literal the author wrote.
        let phrase = match self.peek() {
            Some(b'.') => Some("fractional numbers"),
            Some(b'e' | b'E') => Some("numbers with an exponent"),
            Some(b'n') => Some("BigInt literals"),
            _ => None,
        };
        if let Some(phrase) = phrase {
            self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'+' || b == b'-');
            return subset(phrase);
        }
        match self.src[start..self.pos].parse::<u64>() {
            Ok(value) => TokenKind::Int(value),
            // Beyond `u64` there is no doubt at all which boundary was hit.
            Err(_) => subset(OUT_OF_I32_RANGE),
        }
    }

    /// `$N`, the Nth argument of this call.
    fn argument(&mut self) -> TokenKind {
        self.pos += 1;
        let start = self.pos;
        self.eat_while(|b| b.is_ascii_digit());
        match self.src[start..self.pos].parse::<u64>() {
            Ok(index) => TokenKind::Arg(index),
            Err(_) => subset(TOO_MANY_ARGUMENTS),
        }
    }

    /// An identifier or a reserved word.
    ///
    /// A plain identifier becomes [`TokenKind::Ident`] and the parser decides
    /// what it can mean. The words listed here never can: they either name a
    /// JavaScript runtime facility (`eval`, `undefined`) or are reserved, and
    /// in both cases treating them as a name would be a lie about what the
    /// source says.
    fn word(&mut self) -> TokenKind {
        let start = self.pos;
        self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
        let word = &self.src[start..self.pos];
        match word {
            "true" | "false" => subset("boolean literals"),
            "null" => subset("the `null` literal"),
            // Not reserved words, but not names either: each is a JavaScript
            // value or facility the engine would have to *implement*, so
            // resolving them like an ordinary name would answer the wrong
            // question. `eval` is a scheduled capability, not an excluded one.
            "undefined" => full_js("the `undefined` value"),
            "eval" => full_js("the `eval` function"),
            _ if is_reserved(word) => full_js(&format!("the `{word}` keyword")),
            _ => TokenKind::Ident(word.to_string()),
        }
    }

    /// A string or template literal, consumed whole so what follows it is not
    /// mistaken for code.
    fn quoted(&mut self, quote: u8, phrase: &str) -> TokenKind {
        self.pos += 1;
        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'\\' if self.pos < self.bytes.len() => self.pos += 1,
                b if b == quote => break,
                _ => {}
            }
        }
        subset(phrase)
    }

    /// Operators and delimiters. Multi-byte forms are matched before their
    /// single-byte prefixes, so `**` never reads as two `*`.
    fn punctuation(&mut self, byte: u8) -> TokenKind {
        let next = self.peek_at(1);
        let (len, kind) = match (byte, next) {
            (b'*', Some(b'*')) => (2, subset("exponentiation")),
            (b'+', Some(b'+')) | (b'-', Some(b'-')) => {
                (2, subset("the increment and decrement operators"))
            }
            (b'=', Some(b'>')) => (2, full_js("arrow functions")),
            (b'=', Some(b'=')) | (b'!', Some(b'=')) => (2, subset("comparison operators")),
            (b'<', Some(b'<')) | (b'>', Some(b'>')) => (2, subset("bitwise operators")),
            (b'<' | b'>', Some(b'=')) => (2, subset("comparison operators")),
            (b'&', Some(b'&')) | (b'|', Some(b'|')) | (b'?', Some(b'?')) => {
                (2, subset("logical operators"))
            }
            (b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^', Some(b'=')) => {
                (2, subset("assignment"))
            }
            (b'+', _) => (1, TokenKind::Plus),
            (b'-', _) => (1, TokenKind::Minus),
            (b'*', _) => (1, TokenKind::Star),
            (b'/', _) => (1, TokenKind::Slash),
            (b'%', _) => (1, TokenKind::Percent),
            (b'(', _) => (1, TokenKind::LParen),
            (b')', _) => (1, TokenKind::RParen),
            (b';', _) => (1, TokenKind::Semi),
            // The three that would need a world beyond the two bindings: a
            // block or object body, an array, a property of something.
            (b'{' | b'}', _) => (1, third("block statements")),
            (b'[' | b']', _) => (1, third("array literals")),
            (b'.', _) => (1, third("property access")),
            (b'<' | b'>', _) => (1, subset("comparison operators")),
            (b'&' | b'|' | b'^' | b'~', _) => (1, subset("bitwise operators")),
            (b'?' | b':', _) => (1, subset("conditional expressions")),
            (b'!', _) => (1, subset("the logical `!` operator")),
            (b'=', _) => (1, subset("assignment")),
            (b',', _) => (1, subset("the comma operator")),
            _ => {
                // Not ASCII punctuation the engine knows. Name the character
                // itself, decoded as a `char` so a multi-byte one prints whole.
                let c = self.src[self.pos..].chars().next().unwrap_or('\u{fffd}');
                (c.len_utf8(), subset(&format!("the character `{c}`")))
            }
        };
        self.pos += len;
        kind
    }

    fn eat_while(&mut self, mut accept: impl FnMut(u8) -> bool) {
        while self.peek().is_some_and(&mut accept) {
            self.pos += 1;
        }
    }
}

/// The two boundaries the parser also reports, kept in one place so the lexer
/// and the parser cannot drift apart in their wording.
pub(crate) const OUT_OF_I32_RANGE: &str = "integers outside the signed 32-bit range";
pub(crate) const TOO_MANY_ARGUMENTS: &str = "more than 64 call arguments";

fn unlowered(boundary: Boundary, phrase: &str) -> TokenKind {
    TokenKind::Unsupported(Unlowered {
        boundary,
        phrase: phrase.to_string(),
    })
}

fn subset(phrase: &str) -> TokenKind {
    unlowered(Boundary::Subset, phrase)
}

fn full_js(phrase: &str) -> TokenKind {
    unlowered(Boundary::FullJs, phrase)
}

fn third(phrase: &str) -> TokenKind {
    unlowered(Boundary::ThirdBinding, phrase)
}

/// ECMA-262 reserved words plus the contextual ones a reader would expect to
/// be named as keywords. Anything not listed is an ordinary identifier, and
/// identifiers are their own boundary ("variable references").
fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "await"
            | "async"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "let"
            | "new"
            | "of"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}
