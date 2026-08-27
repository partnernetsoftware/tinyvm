//! Source text -> tokens.
//!
//! The lexer recognises far more than the parser can lower, and that is
//! deliberate. A tokenizer that stopped at the first unknown byte could only
//! ever say "unexpected character"; this one reads the whole `0x10`, the whole
//! `` `t${x}` ``, and hands the parser a token that already knows how to name
//! itself. That is what makes the capability diagnostics in [`crate::diag`]
//! possible.
//!
//! Out-of-subset lexemes become [`TokenKind::Unsupported`] carrying the noun
//! phrase for the diagnostic and the [`Boundary`] it ran into. As milestones
//! land, those lexemes graduate into real token kinds one at a time; nothing
//! else about the lexer changes.
//!
//! Graduating a lexeme moves its diagnostic from here to the parser, so the
//! *phrase* stays here: [`TokenKind::capability`] is the same table the
//! `Unsupported` tokens are built from, keyed by the graduated kind. A reader
//! sees `this engine does not support block statements yet` whether `{` is
//! still a lexer refusal or already a `LBrace` the parser has no use for.
//!
//! Automatic semicolon insertion (ECMA-262 12.10) is split where the spec
//! splits it -- see the section at the bottom of this file.

use crate::diag::{Boundary, CompileError, malformed};

/// One lexeme and where it starts, in bytes from the start of the source.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) offset: usize,
    /// A LineTerminator stood between this token and the one before it --
    /// written directly, or inside a multi-line comment, which ECMA-262 12.4
    /// says counts the same. This is the raw fact all of ASI is built on.
    pub(crate) newline_before: bool,
    /// This `;` was inserted by ASI rather than written. Mostly nothing
    /// downstream has to care -- an inserted semicolon *is* a semicolon -- but
    /// a `for` header is the one place ECMA-262 12.10 says an inserted one does
    /// not count, and a diagnostic that points at one can say so instead of
    /// pointing at absent text.
    pub(crate) inserted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    /// A decimal integer literal, as its unsigned magnitude. The sign belongs
    /// to the parser: `-2147483648` is a unary minus applied to a magnitude
    /// that does not fit in an `i32` on its own.
    Int(u64),
    /// A numeric literal that is **not** a plain decimal integer: it has a
    /// fraction, an exponent, or both.
    ///
    /// Separate from [`Int`](Self::Int) and not a widening of it, because the
    /// two answer different questions downstream. An `Int` is also a property
    /// key -- `{ 1: v }` is the key `"1"` -- and the digits it was written
    /// with are the key; a `Num` has no such spelling without running
    /// ECMA-262 6.1.6.1.20 at compile time, so it is refused in that position
    /// rather than approximated. Every *value* use is the same either way:
    /// this engine's numbers are binary64 and always were.
    Num(f64),
    /// `$N` -- the Nth argument of this call. Held wide so an absurd index is
    /// a bounds decision in the parser rather than a silent wrap here.
    Arg(u64),
    /// A string literal, already decoded: the escapes are resolved and the
    /// quotes are gone, so this is the value the program means.
    Str(String),
    /// A name. Whether one means anything is not the lexer's business: the
    /// language has no bindings to resolve it against, and the `eval_wasm`
    /// skin resolves it to a host import. See [`crate::Names`].
    Ident(String),

    // Keywords. Every one of these is real JavaScript the engine can read;
    // whether it can *run* one is the parser's answer, not the lexer's.
    Function,
    Return,
    If,
    Else,
    While,
    For,
    Let,
    Const,
    Var,
    Typeof,
    True,
    False,
    Null,
    Undefined,
    /// `try`, `catch`, `finally` and `throw`. Graduated out of
    /// [`is_reserved`] when the engine learned to unwind; before that a
    /// script that wrote one was refused by the lexer with the same phrase
    /// [`TokenKind::capability`] still keeps for them.
    Try,
    Catch,
    Finally,
    Throw,

    // Delimiters.
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    /// `.` -- the member access, and nothing else. A `.` inside a numeric
    /// literal never reaches here: `number` consumes the whole literal.
    Dot,
    /// `:` -- the separator of an ObjectLiteral's PropertyDefinition. It is
    /// also the second half of a conditional expression, which is why
    /// [`TokenKind::capability`] still names that when the parser has no use
    /// for one here.
    Colon,
    /// `?` -- the first half of a conditional expression, and the only thing
    /// a lone `?` spells. `??` and `?.` are their own lexemes, read before
    /// this one.
    Question,

    // Operators.
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    PlusPlus,
    MinusMinus,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    EqEq,
    BangEq,
    EqEqEq,
    BangEqEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AmpAmp,
    PipePipe,

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
            Self::Int(_) | Self::Num(_) => "a number",
            Self::Arg(_) => "an argument reference",
            Self::Str(_) => "a string literal",
            Self::Ident(_) => "a name",
            Self::Function => "the `function` keyword",
            Self::Return => "the `return` keyword",
            Self::If => "the `if` keyword",
            Self::Else => "the `else` keyword",
            Self::While => "the `while` keyword",
            Self::For => "the `for` keyword",
            Self::Let => "the `let` keyword",
            Self::Const => "the `const` keyword",
            Self::Var => "the `var` keyword",
            Self::Typeof => "the `typeof` operator",
            Self::True => "the `true` literal",
            Self::False => "the `false` literal",
            Self::Null => "the `null` literal",
            Self::Undefined => "the `undefined` value",
            Self::Try => "the `try` keyword",
            Self::Catch => "the `catch` keyword",
            Self::Finally => "the `finally` keyword",
            Self::Throw => "the `throw` keyword",
            Self::LParen => "a `(`",
            Self::RParen => "a `)`",
            Self::LBrace => "a `{`",
            Self::RBrace => "a `}`",
            Self::LBracket => "a `[`",
            Self::RBracket => "a `]`",
            Self::Dot => "a `.`",
            Self::Colon => "a `:`",
            Self::Question => "a `?`",
            Self::Semi => "a `;`",
            Self::Comma => "a `,`",
            Self::Plus => "a `+`",
            Self::Minus => "a `-`",
            Self::Star => "a `*`",
            Self::Slash => "a `/`",
            Self::Percent => "a `%`",
            Self::Bang => "a `!`",
            Self::PlusPlus => "a `++`",
            Self::MinusMinus => "a `--`",
            Self::Eq => "a `=`",
            Self::PlusEq => "a `+=`",
            Self::MinusEq => "a `-=`",
            Self::StarEq => "a `*=`",
            Self::SlashEq => "a `/=`",
            Self::PercentEq => "a `%=`",
            Self::EqEq => "a `==`",
            Self::BangEq => "a `!=`",
            Self::EqEqEq => "a `===`",
            Self::BangEqEq => "a `!==`",
            Self::Lt => "a `<`",
            Self::LtEq => "a `<=`",
            Self::Gt => "a `>`",
            Self::GtEq => "a `>=`",
            Self::AmpAmp => "a `&&`",
            Self::PipePipe => "a `||`",
            Self::Unsupported(_) => "an unsupported construct",
            Self::Eof => "the end of the source",
        }
    }

    /// The capability phrase for a token the lexer graduated but the rest of
    /// the pipeline may not lower yet, or `None` for a token it does lower.
    ///
    /// The caller decides *whether* it can lower the token; this only decides
    /// what to call it if it cannot. Keeping the phrase here is the anti-drift
    /// lock: a lexeme's diagnostic reads the same before and after the
    /// milestone that turned it from an `Unsupported` into a real kind.
    #[allow(dead_code, reason = "for the parser, which reaches these kinds next")]
    pub(crate) fn capability(&self) -> Option<Unlowered> {
        let (boundary, phrase) = match self {
            // A block or an object body is a world beyond the two bindings a
            // call has, which is the same reason `{` used to refuse here.
            Self::LBrace | Self::RBrace => (Boundary::ThirdBinding, "block statements"),
            Self::Lt | Self::LtEq | Self::Gt | Self::GtEq => {
                (Boundary::Subset, "comparison operators")
            }
            Self::EqEq | Self::BangEq | Self::EqEqEq | Self::BangEqEq => {
                (Boundary::Subset, "comparison operators")
            }
            Self::Eq
            | Self::PlusEq
            | Self::MinusEq
            | Self::StarEq
            | Self::SlashEq
            | Self::PercentEq => (Boundary::Subset, "assignment"),
            Self::PlusPlus | Self::MinusMinus => {
                (Boundary::Subset, "the increment and decrement operators")
            }
            Self::AmpAmp | Self::PipePipe => (Boundary::Subset, "logical operators"),
            // Reached only by the **M0** front end now. `[` has graduated for
            // both things it spells -- `o[k]` and the ArrayLiteral -- so M1
            // never asks this table about one: a `[` its parser cannot use is
            // an ordinary syntax error and gets the parser's own wording
            // ("needs an operand here" for a stray `]`), and an elision gets
            // its own `FullJs` refusal. M0 lowers one integer expression and
            // has neither, so for the front end that still asks, the phrase is
            // true.
            //
            // Deleting the arm was tried and was wrong: it left M0 answering
            // `[1]` with the generic "outside the expression subset", losing
            // the name of the thing. `eval_qjs::host_call_with_args_is_third_world`
            // is what said so.
            Self::LBracket | Self::RBracket => (Boundary::ThirdBinding, "array literals"),
            Self::Dot => (Boundary::ThirdBinding, "property access"),
            // `:` graduated for an ObjectLiteral, and the M1 parser now also
            // reads the one in `?:`. What is left -- a `:` that reaches a
            // position neither of those two consumed -- is a label, so that
            // is what it names. It used to say "conditional expressions",
            // which was true until the milestone that landed them and would
            // now be the engine disclaiming a capability it has.
            Self::Colon => (Boundary::FullJs, "labelled statements"),
            // `?` is lowered by the M1 parser and *not* by the M0 expression
            // pipeline, which is a live caller of this table -- so the phrase
            // stays, and `parse::Parser::program` is where M0 spends it.
            Self::Question => (Boundary::Subset, "conditional expressions"),
            Self::Bang => (Boundary::Subset, "the logical `!` operator"),
            Self::Comma => (Boundary::Subset, "the comma operator"),
            Self::Str(_) => (Boundary::Subset, "string literals"),
            Self::True | Self::False => (Boundary::Subset, "boolean literals"),
            Self::Null => (Boundary::Subset, "the `null` literal"),
            // Not reserved words, but not names either: each is a JavaScript
            // value or facility the engine would have to *implement*, so
            // resolving them like an ordinary name would answer the wrong
            // question.
            Self::Undefined => (Boundary::FullJs, "the `undefined` value"),
            Self::Typeof => (Boundary::FullJs, "the `typeof` operator"),
            Self::Function => (Boundary::FullJs, "the `function` keyword"),
            Self::Return => (Boundary::FullJs, "the `return` keyword"),
            Self::If => (Boundary::FullJs, "the `if` keyword"),
            Self::Else => (Boundary::FullJs, "the `else` keyword"),
            Self::While => (Boundary::FullJs, "the `while` keyword"),
            Self::For => (Boundary::FullJs, "the `for` keyword"),
            Self::Let => (Boundary::FullJs, "the `let` keyword"),
            Self::Const => (Boundary::FullJs, "the `const` keyword"),
            Self::Var => (Boundary::FullJs, "the `var` keyword"),
            Self::Try => (Boundary::FullJs, "the `try` keyword"),
            Self::Catch => (Boundary::FullJs, "the `catch` keyword"),
            Self::Finally => (Boundary::FullJs, "the `finally` keyword"),
            Self::Throw => (Boundary::FullJs, "the `throw` keyword"),
            // Already carrying its own phrase, or already lowered. `Ident` is
            // deliberately absent: what a name means is the one thing
            // `crate::Options` chooses, so only the parser may name it.
            _ => return None,
        };
        Some(Unlowered {
            boundary,
            phrase: phrase.to_string(),
        })
    }
}

/// Tokenize the whole source. Always ends with exactly one [`TokenKind::Eof`].
///
/// Fails only on input the lexer cannot finish reading at all (an unclosed
/// block comment, an unclosed string); everything else it can name, it names.
pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, CompileError> {
    let mut lexer = Lexer {
        src: source,
        bytes: source.as_bytes(),
        pos: 0,
    };
    let mut tokens = Vec::new();
    loop {
        let newline_before = lexer.skip_trivia()?;
        let offset = lexer.pos;
        if offset >= lexer.bytes.len() {
            tokens.push(Token {
                kind: TokenKind::Eof,
                offset,
                newline_before,
                inserted: false,
            });
            break;
        }
        let kind = lexer.lexeme()?;
        tokens.push(Token {
            kind,
            offset,
            newline_before,
            inserted: false,
        });
    }
    insert_restricted_semicolons(&mut tokens);
    Ok(tokens)
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

    /// The whole character at the cursor. Every advance in this lexer moves by
    /// a whole character, so the cursor is always on a UTF-8 boundary.
    fn current(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// Whitespace, line terminators, and both comment forms. Reports whether
    /// any of it was a line terminator, which is what ASI runs on.
    fn skip_trivia(&mut self) -> Result<bool, CompileError> {
        let mut newline = false;
        loop {
            match self.peek() {
                Some(b'\n' | b'\r') => {
                    newline = true;
                    self.pos += 1;
                }
                Some(b' ' | b'\t' | 0x0b | 0x0c) => self.pos += 1,
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    self.pos += 2;
                    // The terminator itself is left for the next pass, so it
                    // is counted exactly once.
                    while let Some(c) = self.current() {
                        if is_line_terminator(c) {
                            break;
                        }
                        self.pos += c.len_utf8();
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let opened = self.pos;
                    self.pos += 2;
                    loop {
                        match self.current() {
                            None => {
                                return Err(malformed(
                                    &format!(
                                        "needs a `*/` to close the comment opened at byte {opened}; the source ends first"
                                    ),
                                    opened,
                                ));
                            }
                            Some('*') if self.peek_at(1) == Some(b'/') => {
                                self.pos += 2;
                                break;
                            }
                            Some(c) => {
                                // ECMA-262 12.4: a multi-line comment holding a
                                // line terminator *is* one to the grammar. Miss
                                // this and `return /*\n*/ x` returns `x`.
                                newline |= is_line_terminator(c);
                                self.pos += c.len_utf8();
                            }
                        }
                    }
                }
                // Beyond ASCII only whitespace and the two exotic line
                // terminators are trivia; anything else starts a lexeme.
                Some(0x80..) => {
                    let c = self.current().unwrap_or('\u{fffd}');
                    if is_line_terminator(c) {
                        newline = true;
                    } else if !is_whitespace(c) {
                        return Ok(newline);
                    }
                    self.pos += c.len_utf8();
                }
                _ => return Ok(newline),
            }
        }
    }

    /// One lexeme, starting at a byte that is neither whitespace nor a comment.
    fn lexeme(&mut self) -> Result<TokenKind, CompileError> {
        let byte = self.bytes[self.pos];
        match byte {
            b'0'..=b'9' => self.number(),
            // `.5` is a NumericLiteral (ECMA-262 12.9.3 DecimalLiteral's
            // second production), not a `.` followed by `5`. Without this the
            // author is told a property access needs a name.
            b'.' if matches!(self.peek_at(1), Some(b'0'..=b'9')) => self.number(),
            b'$' if matches!(self.peek_at(1), Some(b'0'..=b'9')) => Ok(self.argument()),
            b'A'..=b'Z' | b'a'..=b'z' | b'_' | b'$' => Ok(self.word()),
            b'"' | b'\'' => self.string(byte),
            b'`' => Ok(self.template()),
            _ => Ok(self.punctuation()),
        }
    }

    /// A numeric literal in any JavaScript form. Only plain decimal integers
    /// that fit an `i32` survive; the rest name themselves.
    fn number(&mut self) -> Result<TokenKind, CompileError> {
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
                return Ok(subset(phrase));
            }
            // ECMA-262 12.9.3: a `0` followed by another digit is a
            // LegacyOctalIntegerLiteral (`0777` is 511) or, once an `8` or a
            // `9` is in it, a NonOctalDecimalIntegerLiteral. Both are
            // strict-mode SyntaxErrors, and this subset is strict mode. The
            // digits are consumed whole so the token spans the literal, and
            // the boundary is named rather than the run being read as the
            // decimal it is not.
            if matches!(self.peek_at(1), Some(b'0'..=b'9')) {
                self.pos += 1;
                self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.');
                return Ok(subset("number literals written with a leading zero"));
            }
        }
        self.eat_while(|b| b.is_ascii_digit());

        // A fraction and an exponent are read, not refused: this engine's
        // numbers are binary64, so `1.5` denotes a value it has always been
        // able to compute -- `3 / 2` produced it -- and only the spelling was
        // missing. ECMA-262 12.9.3 DecimalLiteral, minus the separators and
        // the other radices, which still name themselves below.
        let mut fractional = false;
        if self.peek() == Some(b'.') {
            fractional = true;
            self.pos += 1;
            self.eat_while(|b| b.is_ascii_digit());
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            // Only when it is really an exponent: `1einvalid` is not, and
            // consuming the `e` would turn a bad literal into a worse
            // diagnostic.
            let after = match self.peek_at(1) {
                Some(b'+' | b'-') => self.peek_at(2),
                other => other,
            };
            if matches!(after, Some(b'0'..=b'9')) {
                fractional = true;
                self.pos += if matches!(self.peek_at(1), Some(b'+' | b'-')) {
                    2
                } else {
                    1
                };
                self.eat_while(|b| b.is_ascii_digit());
            }
        }

        // The suffixes that still turn a decimal literal into something this
        // engine does not read. Each is consumed whole so the token spans the
        // literal the author wrote.
        let phrase = match self.peek() {
            Some(b'n') => Some("BigInt literals"),
            // ES2021 NumericLiteralSeparator. Named here because the digit run
            // ends at the `_`, so without this the rest of the literal lexes
            // as an identifier and the author is told the statement needs a
            // `;` -- a sentence about a mistake they did not make.
            Some(b'_') => Some("numeric separators in number literals"),
            _ => None,
        };
        if let Some(phrase) = phrase {
            self.eat_while(|b| {
                b.is_ascii_alphanumeric() || b == b'.' || b == b'+' || b == b'-' || b == b'_'
            });
            return Ok(subset(phrase));
        }

        // ECMA-262 12.9.3: "The SourceCharacter immediately following a
        // NumericLiteral must not be an IdentifierStart or DecimalDigit."
        //
        // This became worth saying when exponents landed. Before, `1einvalid`
        // was consumed whole and refused as "numbers with an exponent" -- a
        // sentence that is now a lie, since exponents work. Without a rule of
        // its own the literal lexes as `1` and the name `einvalid`, and the
        // author is told their statement needs a `;`: a sentence about a
        // mistake they did not make.
        if matches!(self.peek(), Some(b'0'..=b'9'))
            || matches!(self.peek(), Some(b) if b.is_ascii_alphabetic() || b == b'_' || b == b'$')
        {
            self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'.');
            return Err(malformed(
                "needs a separator between this number and the name after it; ECMA-262 12.9.3 ends a numeric literal before any identifier character",
                start,
            ));
        }

        let text = &self.src[start..self.pos];
        if fractional {
            // `parse::<f64>` is correctly rounded and accepts exactly the
            // grammar consumed above. An out-of-range magnitude is `inf` here
            // and in ECMA-262 alike (6.1.6.1.1), so there is nothing to refuse.
            return Ok(match text.parse::<f64>() {
                Ok(value) => TokenKind::Num(value),
                Err(_) => subset("fractional numbers"),
            });
        }
        Ok(match text.parse::<u64>() {
            Ok(value) => TokenKind::Int(value),
            // Past `u64` the digits still denote a double -- ECMA-262 6.1.6.1
            // has no integer type to overflow -- so the `u64` is an artifact
            // of *this* lexer's intermediate and not a boundary of the
            // language. It used to refuse here with
            // `integers outside the signed 32-bit range`, which stopped being
            // true when a literal past `i32` became a `Num`; refusing at the
            // next arbitrary width instead would just move the same wrong
            // sentence.
            //
            // `parse::<f64>` on a run of digits is correctly rounded and
            // cannot fail, so the `unwrap_or` arm is unreachable rather than
            // lenient.
            Err(_) => TokenKind::Num(text.parse::<f64>().unwrap_or(f64::INFINITY)),
        })
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
    /// The M1 keywords are real tokens; a plain identifier becomes
    /// [`TokenKind::Ident`] and the parser decides what it can mean. The words
    /// in [`is_reserved`] can never be a name and cannot be read as one either,
    /// so they still refuse here and say which keyword they are.
    fn word(&mut self) -> TokenKind {
        let start = self.pos;
        self.eat_while(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
        let word = &self.src[start..self.pos];
        match word {
            "function" => TokenKind::Function,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "let" => TokenKind::Let,
            "const" => TokenKind::Const,
            "var" => TokenKind::Var,
            "typeof" => TokenKind::Typeof,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "null" => TokenKind::Null,
            "undefined" => TokenKind::Undefined,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "finally" => TokenKind::Finally,
            "throw" => TokenKind::Throw,
            // A function the engine would have to *implement*, so resolving it
            // like an ordinary name would answer the wrong question. `eval` is
            // a scheduled capability, not an excluded one.
            "eval" => full_js("the `eval` function"),
            _ if is_reserved(word) => full_js(&format!("the `{word}` keyword")),
            _ => TokenKind::Ident(word.to_string()),
        }
    }

    /// A template literal, consumed whole so what follows it is not mistaken
    /// for code. The substitutions inside are not scanned: nothing downstream
    /// can use them, and the whole lexeme is one capability boundary.
    fn template(&mut self) -> TokenKind {
        self.pos += 1;
        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'\\' if self.pos < self.bytes.len() => self.pos += 1,
                b'`' => break,
                _ => {}
            }
        }
        subset("template literals")
    }

    /// A string literal and its value (ECMA-262 12.9.4).
    ///
    /// The literal is always consumed to its closing quote, even when part of
    /// it is out of subset, so the text after it is read as code and not as a
    /// second string. An escape the engine cannot represent therefore yields an
    /// `Unsupported` token spanning the whole literal, not a truncated value.
    fn string(&mut self, quote: u8) -> Result<TokenKind, CompileError> {
        let opened = self.pos;
        let quote = quote as char;
        self.pos += 1;
        let mut value = String::new();
        let mut unlowered: Option<&'static str> = None;
        loop {
            let Some(c) = self.current() else {
                return Err(malformed(
                    &format!(
                        "needs a `{quote}` to close the string opened at byte {opened}; the source ends first"
                    ),
                    opened,
                ));
            };
            match c {
                _ if c == quote => {
                    self.pos += 1;
                    break;
                }
                // U+2028 and U+2029 are line terminators everywhere else but
                // are legal inside a string literal, so only these two end it.
                '\n' | '\r' => {
                    return Err(malformed(
                        &format!(
                            "needs a `{quote}` to close the string opened at byte {opened}; the line ends first"
                        ),
                        opened,
                    ));
                }
                '\\' => {
                    if let Some(phrase) = self.escape(&mut value)? {
                        unlowered = unlowered.or(Some(phrase));
                    }
                }
                _ => {
                    value.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        Ok(match unlowered {
            Some(phrase) => subset(phrase),
            None => TokenKind::Str(value),
        })
    }

    /// One escape sequence, appending what it contributes to `value`. Returns
    /// the capability phrase when the escape is real JavaScript this engine
    /// cannot represent, in which case `value` is left incomplete on purpose --
    /// the caller is going to discard it.
    fn escape(&mut self, value: &mut String) -> Result<Option<&'static str>, CompileError> {
        let at = self.pos;
        self.pos += 1;
        let Some(c) = self.current() else {
            // The caller's unterminated-string diagnostic is the better one,
            // and it is what the next loop iteration will produce.
            return Ok(None);
        };
        self.pos += c.len_utf8();
        match c {
            'b' => value.push('\u{8}'),
            'f' => value.push('\u{c}'),
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            'v' => value.push('\u{b}'),
            // `\0` is NUL only when no digit follows; with one it is a legacy
            // octal escape, which is a strict-mode error rather than a value.
            '0' if !matches!(self.peek(), Some(b'0'..=b'9')) => value.push('\0'),
            '0'..='9' => {
                self.eat_while(|b| b.is_ascii_digit());
                return Ok(Some("legacy octal escapes in string literals"));
            }
            'x' => return self.code_unit(2, at, value),
            'u' if self.peek() == Some(b'{') => return self.code_point(at, value),
            'u' => {
                let Some(phrase) = self.code_unit(4, at, value)? else {
                    return Ok(None);
                };
                return Ok(Some(phrase));
            }
            // LineContinuation: the escape and the terminator both vanish, and
            // CRLF is one terminator rather than two.
            _ if is_line_terminator(c) => {
                if c == '\r' && self.peek() == Some(b'\n') {
                    self.pos += 1;
                }
            }
            // NonEscapeCharacter -- `\"`, `\'`, `\\`, `\/`, and every other
            // character, all of which stand for themselves.
            _ => value.push(c),
        }
        Ok(None)
    }

    /// `\xHH` or `\uHHHH`: exactly `digits` hexadecimal digits, then the code
    /// unit they spell. A surrogate is only a character once it is paired.
    fn code_unit(
        &mut self,
        digits: usize,
        at: usize,
        value: &mut String,
    ) -> Result<Option<&'static str>, CompileError> {
        let unit = self.hex(digits, at)?;
        match char::from_u32(unit) {
            Some(c) => value.push(c),
            // A high surrogate is half a character; the other half must be the
            // very next escape, spelled the same way, or there is no character
            // here that a Rust `String` can hold.
            None if (0xd800..=0xdbff).contains(&unit) => {
                let resumed = self.pos;
                if self.peek() == Some(b'\\') && self.peek_at(1) == Some(b'u') {
                    self.pos += 2;
                    if let Ok(low) = self.hex(4, at)
                        && (0xdc00..=0xdfff).contains(&low)
                    {
                        let combined = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                        value.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                        return Ok(None);
                    }
                    self.pos = resumed;
                }
                return Ok(Some(UNPAIRED_SURROGATE));
            }
            None => return Ok(Some(UNPAIRED_SURROGATE)),
        }
        Ok(None)
    }

    /// `\u{...}`: a code point in braces, any number of digits.
    fn code_point(
        &mut self,
        at: usize,
        value: &mut String,
    ) -> Result<Option<&'static str>, CompileError> {
        self.pos += 1;
        let start = self.pos;
        self.eat_while(|b| b.is_ascii_hexdigit());
        let digits = &self.src[start..self.pos];
        if self.peek() != Some(b'}') || digits.is_empty() {
            return Err(malformed(
                &format!(
                    "needs hexadecimal digits and a `}}` to close the `\\u{{` escape at byte {at}"
                ),
                at,
            ));
        }
        self.pos += 1;
        let point = u32::from_str_radix(digits, 16).unwrap_or(u32::MAX);
        match char::from_u32(point) {
            Some(c) => value.push(c),
            None if point <= 0x10ffff => return Ok(Some(UNPAIRED_SURROGATE)),
            None => {
                return Err(malformed(
                    &format!(
                        "needs a code point of at most U+10FFFF in the `\\u{{}}` escape at byte {at}"
                    ),
                    at,
                ));
            }
        }
        Ok(None)
    }

    /// Exactly `digits` hexadecimal digits, as the number they spell.
    fn hex(&mut self, digits: usize, at: usize) -> Result<u32, CompileError> {
        let start = self.pos;
        for _ in 0..digits {
            if !self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                let escape = if digits == 2 { "\\x" } else { "\\u" };
                return Err(malformed(
                    &format!(
                        "needs {digits} hexadecimal digits after the `{escape}` escape at byte {at}"
                    ),
                    at,
                ));
            }
            self.pos += 1;
        }
        Ok(u32::from_str_radix(&self.src[start..self.pos], 16).unwrap_or(u32::MAX))
    }

    /// Operators and delimiters, longest form first: the arms are in
    /// descending length order, so `>>>=` never reads as `>>` `>=` and `**`
    /// never reads as two `*`.
    fn punctuation(&mut self) -> TokenKind {
        let rest = &self.bytes[self.pos..];
        let (len, kind) = match rest {
            [b'>', b'>', b'>', b'=', ..] => (4, subset(ASSIGNMENT)),

            [b'=', b'=', b'=', ..] => (3, TokenKind::EqEqEq),
            [b'!', b'=', b'=', ..] => (3, TokenKind::BangEqEq),
            [b'>', b'>', b'>', ..] => (3, subset(BITWISE)),
            [b'<', b'<', b'=', ..] | [b'>', b'>', b'=', ..] => (3, subset(ASSIGNMENT)),
            [b'*', b'*', b'=', ..] => (3, subset(ASSIGNMENT)),
            [b'&', b'&', b'=', ..] | [b'|', b'|', b'=', ..] | [b'?', b'?', b'=', ..] => {
                (3, subset("logical assignment"))
            }
            [b'.', b'.', b'.', ..] => (3, third("the spread and rest syntax")),

            [b'*', b'*', ..] => (2, subset("exponentiation")),
            [b'+', b'+', ..] => (2, TokenKind::PlusPlus),
            [b'-', b'-', ..] => (2, TokenKind::MinusMinus),
            [b'=', b'>', ..] => (2, full_js("arrow functions")),
            [b'=', b'=', ..] => (2, TokenKind::EqEq),
            [b'!', b'=', ..] => (2, TokenKind::BangEq),
            [b'<', b'<', ..] | [b'>', b'>', ..] => (2, subset(BITWISE)),
            [b'<', b'=', ..] => (2, TokenKind::LtEq),
            [b'>', b'=', ..] => (2, TokenKind::GtEq),
            [b'&', b'&', ..] => (2, TokenKind::AmpAmp),
            [b'|', b'|', ..] => (2, TokenKind::PipePipe),
            [b'?', b'?', ..] => (2, subset("the nullish coalescing operator")),
            [b'?', b'.', ..] => (2, subset("optional chaining")),
            [b'+', b'=', ..] => (2, TokenKind::PlusEq),
            [b'-', b'=', ..] => (2, TokenKind::MinusEq),
            [b'*', b'=', ..] => (2, TokenKind::StarEq),
            [b'/', b'=', ..] => (2, TokenKind::SlashEq),
            [b'%', b'=', ..] => (2, TokenKind::PercentEq),
            [b'&' | b'|' | b'^', b'=', ..] => (2, subset(ASSIGNMENT)),

            [b'+', ..] => (1, TokenKind::Plus),
            [b'-', ..] => (1, TokenKind::Minus),
            [b'*', ..] => (1, TokenKind::Star),
            [b'/', ..] => (1, TokenKind::Slash),
            [b'%', ..] => (1, TokenKind::Percent),
            [b'!', ..] => (1, TokenKind::Bang),
            [b'=', ..] => (1, TokenKind::Eq),
            [b'<', ..] => (1, TokenKind::Lt),
            [b'>', ..] => (1, TokenKind::Gt),
            [b'(', ..] => (1, TokenKind::LParen),
            [b')', ..] => (1, TokenKind::RParen),
            [b'{', ..] => (1, TokenKind::LBrace),
            [b'}', ..] => (1, TokenKind::RBrace),
            [b';', ..] => (1, TokenKind::Semi),
            [b',', ..] => (1, TokenKind::Comma),
            [b'[', ..] => (1, TokenKind::LBracket),
            [b']', ..] => (1, TokenKind::RBracket),
            [b'.', ..] => (1, TokenKind::Dot),
            [b':', ..] => (1, TokenKind::Colon),
            [b'&' | b'|' | b'^' | b'~', ..] => (1, subset(BITWISE)),
            [b'?', ..] => (1, TokenKind::Question),
            _ => {
                // Not ASCII punctuation the engine knows. Name the character
                // itself, decoded as a `char` so a multi-byte one prints whole.
                let c = self.current().unwrap_or('\u{fffd}');
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

/// Phrases used by more than one arm of [`Lexer::punctuation`], for the same
/// reason: one wording, one place.
const ASSIGNMENT: &str = "assignment";
const BITWISE: &str = "bitwise operators";

/// A JavaScript string is a sequence of UTF-16 code units, so a lone surrogate
/// is a perfectly good JavaScript string. A Rust `String` cannot hold one, and
/// that is a representation limit of this engine rather than a fact about the
/// source, so it is named as one.
const UNPAIRED_SURROGATE: &str = "unpaired surrogates in string literals";

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

/// ECMA-262 12.2 WhiteSpace, minus the line terminators, which 12.3 makes a
/// separate category because ASI depends on telling them apart.
fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{b}' | '\u{c}' | ' ' | '\u{a0}' | '\u{feff}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// ECMA-262 12.3 LineTerminator. All four, not just the two an ASCII lexer
/// notices: U+2028 and U+2029 end a line for the grammar exactly as LF does.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// ECMA-262 reserved words this engine cannot lower yet. The M1 keywords are
/// gone from this list because they are real tokens now; anything not listed
/// and not an M1 keyword is an ordinary identifier, and identifiers are their
/// own boundary ("variable references").
fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "await"
            | "async"
            | "break"
            | "case"
            | "class"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "enum"
            | "export"
            | "extends"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "of"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "void"
            | "with"
            | "yield"
    )
}

// -- automatic semicolon insertion (ECMA-262 12.10) --------------------------
//
// ASI is two different jobs wearing one name, and conflating them is how a
// hand-written lexer gets `a = b\n(c)` wrong.
//
// * Rule 3 keys on *restricted productions* -- the places the grammar writes
//   [no LineTerminator here]. Which token pairs those are is a fact about the
//   token stream, so the lexer settles them, below, by inserting the semicolon
//   the spec says is there.
//
// * Rules 1 and 2 key on "the parser cannot use this token", which the lexer
//   cannot know. `(` on a new line after an expression is a call, `[` is a
//   member access, `+` is an operator: no semicolon. A lexer that inserted one
//   at every line break would silently rewrite all three. So the lexer only
//   answers the half that is a fact -- `semicolon_is_implied` -- and the parser
//   asks it at the points where it wanted a `;` and did not find one.
//
// Two overrides ride on top of both halves: no inserted semicolon may become an
// empty statement, and none may become one of the two in a `for` header. The
// first is handled here (an already-written `;` blocks insertion); the second
// is a grammar position and belongs to the caller, as does rule 1(c), the
// `do`-`while` clause -- `do` is not in this subset yet.

/// Apply rule 3 to the whole stream.
fn insert_restricted_semicolons(tokens: &mut Vec<Token>) {
    let mut i = 1;
    while i < tokens.len() {
        if tokens[i].newline_before && is_restricted(&tokens[i - 1].kind, &tokens[i].kind) {
            let offset = tokens[i].offset;
            tokens.insert(
                i,
                Token {
                    kind: TokenKind::Semi,
                    offset,
                    newline_before: false,
                    inserted: true,
                },
            );
            i += 1;
        }
        i += 1;
    }
}

/// Would a line terminator between these two tokens break a restricted
/// production? Asked only when there is one.
fn is_restricted(prev: &TokenKind, next: &TokenKind) -> bool {
    // The override: a written `;` already terminates the statement, so an
    // inserted one could only be an empty statement.
    if matches!(next, TokenKind::Semi) {
        return false;
    }
    match prev {
        // `return` [no LineTerminator here] Expression. The classic: a
        // `return` alone on its line returns undefined, whatever follows it.
        // Tested before the update rule, because `return\n++x` is the return's
        // restricted production and not an update expression at all.
        TokenKind::Return => true,
        // `throw` [no LineTerminator here] Expression. Unlike `return` there
        // is no shorter production to fall back on, so the semicolon this
        // inserts is not a `throw undefined`: it is a `throw` with nothing to
        // throw, and the parser refuses it by name.
        TokenKind::Throw => true,
        // LeftHandSideExpression [no LineTerminator here] `++`/`--`. Only in
        // that position: with no operand to its left, `++` is the prefix
        // operator and no restricted production is in play.
        _ => matches!(next, TokenKind::PlusPlus | TokenKind::MinusMinus) && ends_lhs(prev),
    }
}

/// Can this token be the last one of a LeftHandSideExpression? That is the
/// only position where a following `++`/`--` is the postfix operator.
fn ends_lhs(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Int(_)
            | TokenKind::Arg(_)
            | TokenKind::Str(_)
            | TokenKind::Ident(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Null
            | TokenKind::Undefined
            | TokenKind::RParen
            // `o[k]` is a MemberExpression, so a `]` can end one. `}` is
            // deliberately absent: at statement position it closes a block,
            // and `{ }\n++x` is a block followed by a prefix increment.
            | TokenKind::RBracket
    )
}

/// Rules 1 and 2: the parser wanted a `;` before `tokens[pos]` and did not find
/// one. May it proceed as though a `;` were there?
///
/// The parser must have established the rest of rule 1 first -- that no
/// production of the grammar accepts this token here -- because that is the
/// part no lexer can see.
pub(crate) fn semicolon_is_implied(tokens: &[Token], pos: usize) -> bool {
    let token = &tokens[pos];
    // Rule 1(a), rule 1(b), and rule 2, in that order.
    token.newline_before
        || matches!(token.kind, TokenKind::RBrace)
        || matches!(token.kind, TokenKind::Eof)
}
