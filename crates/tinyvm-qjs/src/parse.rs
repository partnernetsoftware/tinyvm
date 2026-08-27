//! Tokens -> [`super::ast::Program`].
//!
//! Precedence climbing over a binding-power table rather than one function per
//! precedence level. The two are equivalent for the five operators M0 has; they
//! stop being equivalent at the fourteen JavaScript actually defines, where the
//! function-per-level shape becomes fourteen near-identical functions. Adding a
//! level here is one row in [`infix`].

use super::ast::{BinaryOp, Expr, Program, UnaryOp};
use super::diag::{Boundary, CompileError, host_table, malformed, unsupported};
use super::lex::{OUT_OF_I32_RANGE, TOO_MANY_ARGUMENTS, Token, TokenKind};
use crate::opts::{Names, Options};

/// Binding power of the unary minus. Above every infix level, so `-2 * 3` is
/// `(-2) * 3` and not `-(2 * 3)` -- indistinguishable here, but not once `**`
/// (right associative, binds tighter than unary) arrives.
const PREFIX_BP: u8 = 30;

/// The most arguments one call may name. `$N` becomes a wasm parameter, so an
/// unbounded index would let a two-character source demand a huge signature.
const MAX_ARGS: u64 = 64;

pub(crate) fn parse(tokens: &[Token], options: Options) -> Result<Program, CompileError> {
    let mut parser = Parser {
        tokens,
        pos: 0,
        options,
    };
    parser.program()
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    options: Options,
}

impl Parser<'_> {
    /// The token under the cursor. `tokenize` always terminates the stream
    /// with `Eof` and [`advance`](Self::advance) never steps past it, so the
    /// cursor is a fixed point at the end and this cannot run off.
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) {
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn program(&mut self) -> Result<Program, CompileError> {
        let first = self.peek();
        if first.kind == TokenKind::Eof {
            return Err(malformed(
                "needs an expression to compile; this source is empty",
                first.offset,
            ));
        }
        let expr = self.expression(0)?;
        while self.peek().kind == TokenKind::Semi {
            self.advance();
        }
        let tail = self.peek();
        match &tail.kind {
            TokenKind::Eof => Ok(Program { expr }),
            TokenKind::RParen => Err(malformed(
                "found a `)` here with no `(` to match it",
                tail.offset,
            )),
            // A second expression where the source should have ended. This
            // subset compiles one expression, so this is a capability boundary
            // and not a mistake: `1; 2` is perfectly good JavaScript. An
            // operator that got this far names itself first, though: `1 < 2`
            // is one expression this engine cannot lower, not two statements.
            _ => Err(self.cannot_use(Boundary::Subset, "multiple statements")),
        }
    }

    /// The diagnostic for a token the parser has no use for here.
    ///
    /// A token that names its own capability boundary gets to. That table
    /// lives in [`TokenKind::capability`] precisely so that the wording does
    /// not drift when a lexeme graduates out of `Unsupported` into a real kind
    /// the parser still cannot lower -- a reader sees `this engine does not
    /// support block statements yet` either way. `fallback` is what to say
    /// when the token names no boundary of its own.
    fn cannot_use(&self, boundary: Boundary, fallback: &str) -> CompileError {
        let token = self.peek();
        match &token.kind {
            TokenKind::Unsupported(u) => unsupported(u.boundary, &u.phrase, token.offset),
            other => match other.capability() {
                Some(u) => unsupported(u.boundary, &u.phrase, token.offset),
                None => unsupported(boundary, fallback, token.offset),
            },
        }
    }

    /// [`Self::cannot_use`] for a position where the missing piece is
    /// structural rather than a capability: `what` completes "this engine
    /// {what}", and a token that names a boundary still gets to name it.
    fn wanted(&self, what: &str) -> CompileError {
        let token = self.peek();
        match &token.kind {
            TokenKind::Unsupported(u) => unsupported(u.boundary, &u.phrase, token.offset),
            TokenKind::Eof => malformed(&format!("{what}; the source ends first"), token.offset),
            other => match other.capability() {
                Some(u) => unsupported(u.boundary, &u.phrase, token.offset),
                None => malformed(
                    &format!("{what}, and found {} instead", other.name()),
                    token.offset,
                ),
            },
        }
    }

    /// One expression, consuming infix operators whose left binding power is at
    /// least `min_bp`.
    fn expression(&mut self, min_bp: u8) -> Result<Expr, CompileError> {
        let mut lhs = self.prefix()?;
        while let Some((op, lbp, rbp)) = infix(&self.peek().kind) {
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.expression(rbp)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// A literal, an argument, a parenthesised group, or a unary minus.
    fn prefix(&mut self) -> Result<Expr, CompileError> {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Int(magnitude) => {
                self.advance();
                int_literal(magnitude, false, token.offset)
            }
            TokenKind::Arg(index) => {
                self.advance();
                if index >= MAX_ARGS {
                    return Err(unsupported(
                        Boundary::Subset,
                        TOO_MANY_ARGUMENTS,
                        token.offset,
                    ));
                }
                Ok(Expr::Arg(index as u32))
            }
            // M0 is one integer expression: its values are `i32`. Every
            // literal the lexer hands back as a `Num` -- a fraction, an
            // exponent, an integer past `i32` -- is the *same* boundary from
            // here, so it gets one sentence rather than three, one of which
            // would be wrong. M1 reads all of them and needs no arm at all.
            TokenKind::Num(_) => Err(unsupported(
                Boundary::Subset,
                "numbers outside the signed 32-bit integers this expression form takes",
                token.offset,
            )),
            TokenKind::Ident(name) => {
                self.advance();
                self.name(&name, token.offset)
            }
            TokenKind::Minus => {
                self.advance();
                // Fold a minus directly onto a literal. Not an optimisation:
                // `i32::MIN` has no positive counterpart, so `-2147483648` is
                // only representable if the sign reaches the literal.
                let next = self.peek().clone();
                if let TokenKind::Int(magnitude) = next.kind {
                    self.advance();
                    return int_literal(magnitude, true, next.offset);
                }
                let operand = self.expression(PREFIX_BP)?;
                Ok(Expr::Unary(UnaryOp::Neg, Box::new(operand)))
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.expression(0)?;
                if self.peek().kind == TokenKind::RParen {
                    self.advance();
                    return Ok(inner);
                }
                Err(self.wanted(&format!(
                    "needs a `)` to close the group opened at byte {}",
                    token.offset
                )))
            }
            _ => Err(self.wanted("needs an operand here")),
        }
    }

    /// A bare name, and the optional `()` that may follow it.
    ///
    /// What a name *means* is the one thing the two callers of this compiler
    /// disagree about, so it is the one thing [`Options`] chooses. The language
    /// has no bindings yet, so a name resolves to nothing and saying so is the
    /// honest answer. The [`crate::eval_qjs`] skin has exactly one binding
    /// table -- the `eval_wasm` `globals` -- so there a name is a host import,
    /// and `g` and `g()` mean the same call.
    fn name(&mut self, name: &str, offset: usize) -> Result<Expr, CompileError> {
        match &self.options.names {
            Names::Unbound => {
                return Err(unsupported(Boundary::Subset, "variable references", offset));
            }
            // A declared table's parameters are pointers into linear memory
            // and its arguments are JavaScript values. This pipeline has
            // neither: it is one `i32` expression in and one `i32` out.
            Names::Declared(_) => {
                return Err(host_table(
                    "cannot reach a declared host table from this pipeline, which compiles one `i32` expression; `compile_qjs_m1_with` is the entry point that can",
                    offset,
                ));
            }
            Names::HostImport => {}
        }
        if self.peek().kind == TokenKind::LParen {
            self.advance();
            let close = self.peek().clone();
            if close.kind != TokenKind::RParen {
                // Passing an argument would need a third world: the host door
                // is a table of zero-argument imports, and nothing carries a
                // value across it.
                return Err(unsupported(
                    Boundary::ThirdBinding,
                    "host calls with arguments",
                    close.offset,
                ));
            }
            self.advance();
        }
        Ok(Expr::Host(name.to_string()))
    }
}

/// Range-check a decimal magnitude and apply the sign. `negative` widens the
/// accepted range by exactly one, which is the whole reason the sign is folded
/// here instead of lowered as an operator.
fn int_literal(magnitude: u64, negative: bool, offset: usize) -> Result<Expr, CompileError> {
    let limit = if negative {
        i32::MIN.unsigned_abs() as u64
    } else {
        i32::MAX as u64
    };
    if magnitude > limit {
        return Err(unsupported(Boundary::Subset, OUT_OF_I32_RANGE, offset));
    }
    let value = if negative {
        (magnitude as i64).wrapping_neg() as i32
    } else {
        magnitude as i32
    };
    Ok(Expr::Int(value))
}

/// The binding-power table: `(operator, left power, right power)`.
///
/// `right = left + 1` is what makes a level left associative -- the loop in
/// [`Parser::expression`] refuses to re-enter at the same power. A future
/// right-associative level (`**`, assignment) uses `right = left` instead.
fn infix(kind: &TokenKind) -> Option<(BinaryOp, u8, u8)> {
    match kind {
        TokenKind::Plus => Some((BinaryOp::Add, 10, 11)),
        TokenKind::Minus => Some((BinaryOp::Sub, 10, 11)),
        TokenKind::Star => Some((BinaryOp::Mul, 20, 21)),
        TokenKind::Slash => Some((BinaryOp::Div, 20, 21)),
        TokenKind::Percent => Some((BinaryOp::Rem, 20, 21)),
        _ => None,
    }
}

/// Tokens -> [`crate::ast::m1::Program`]: the M1 front end.
///
/// Nested rather than replacing the parser above because [`super::emit`] still
/// consumes the M0 tree and lives in another lane. Integration is one move:
/// delete the M0 parser, un-nest this module.
///
/// Same precedence-climbing loop as M0, eight rungs instead of two; the table
/// is this module's `infix` and adding a level is still one row. What is new is the
/// second half of a front end, and it is the half that earns its keep:
///
/// * **Scoping lives here**, not in the lowering. wasm locals are indexed, so
///   somebody has to turn a name into an index; doing it in two places is how
///   `var` hoisting and `let` block scoping drift apart. The parser records
///   declarations as it reads them and resolves every *reference* afterwards,
///   in one pass over a flat list -- which is what makes hoisting fall out
///   rather than needing a pre-scan: by the time anything is resolved, every
///   declaration in the program is already recorded.
/// * **Diagnostics stay in the front end.** Because resolution happens here,
///   an unresolved name, a name bound twice, a write to a `const` and a
///   capture across a function boundary are all named here, with a span, and
///   the lowering keeps its promise that it cannot fail.
///
/// The subset is strict mode (ECMA-262 11.2.2: a module is always strict), so
/// a function declaration in a block is block-scoped and there is no legacy
/// octal -- which the lexer already refuses.
#[allow(
    dead_code,
    reason = "the M1 front end is complete before the lowering lane consumes it"
)]
pub(crate) mod m1 {
    use crate::ast::m1::BinaryOp;
    use crate::ast::m1::{
        Binding, BindingId, BindingKind, Catch, Declarator, Expr, ExprKind, FuncId, Function, JSON,
        LogicalOp, MemberKey, Name, Program, Property, Res, Span, Stmt, StmtKind, UnaryOp,
        UpdateOp,
    };
    use crate::diag::{Boundary, CompileError, malformed, unsupported};
    use crate::lex::{
        OUT_OF_I32_RANGE, TOO_MANY_ARGUMENTS, Token, TokenKind, semicolon_is_implied,
    };
    use crate::opts::{Names, Options};

    /// The binding-power ladder, loosest first, spaced by two so that a
    /// left-associative row can spell `right = left + 1` and a
    /// right-associative one `right = left` without the two colliding.
    ///
    /// The rungs JavaScript defines between these are absent because their
    /// tokens are still [`TokenKind::Unsupported`] -- the comma operator, the
    /// bitwise and shift levels, `??`, `**`. Each arrives as one row.
    ///
    /// `?:` is the exception: it has a rung here and no row in [`infix`],
    /// because its middle operand sits *between* two tokens and the table has
    /// no shape for that. [`Parser::expression`] reads it directly, at
    /// [`BP_CONDITIONAL`].
    const BP_ASSIGN: u8 = 2;
    /// ECMA-262 13.14, between assignment and the short-circuit operators:
    /// `ConditionalExpression : ShortCircuitExpression ? AssignmentExpression
    /// : AssignmentExpression`. Landing it is what pushed every rung above it
    /// up by two; the numbers themselves mean nothing but their order.
    const BP_CONDITIONAL: u8 = 4;
    const BP_OR: u8 = 6;
    const BP_AND: u8 = 8;
    const BP_EQUALITY: u8 = 10;
    const BP_RELATIONAL: u8 = 12;
    const BP_ADDITIVE: u8 = 14;
    const BP_MULTIPLICATIVE: u8 = 16;
    /// Unary and update, above every infix level: `-a * 2` is `(-a) * 2`.
    const BP_PREFIX: u8 = 18;

    /// How much native stack one script's syntax may spend, counted in frames
    /// of recursive descent.
    ///
    /// Recursive descent runs on the native stack, and a stack overflow is not
    /// a `Result`: the runtime aborts the whole process. For a host compiling
    /// untrusted `.qjs` that is the worst failure mode there is -- worse than a
    /// wrong answer, because no caller is left to hear about it. So the depth
    /// has to be a number the parser keeps, and it has to be checked before the
    /// frame is pushed rather than after.
    ///
    /// Counted in *frames* and not in nesting levels because the two are not
    /// proportional: one `(` costs four frames of descent, one prefix `!` two,
    /// one `{` two, and one operator in a left-associative chain costs none
    /// here but one in every consumer that walks the tree afterwards. A limit
    /// expressed in levels would have to be tuned to whichever shape happens to
    /// be deepest and would be far too generous for the rest.
    ///
    /// The number is measured, not guessed. At this budget the worst shape --
    /// forty nested function expressions -- reaches about 1 MiB of native
    /// stack in an unoptimised build, against the 2 MiB Rust gives a thread by
    /// default; every other shape is under that. The budget is deliberately
    /// about the *engine*, not about the script, and the diagnostic says so.
    const MAX_FRAMES: u32 = 448;

    pub(crate) fn parse(tokens: &[Token], options: Options) -> Result<Program, CompileError> {
        let mut parser = Parser {
            tokens,
            pos: 0,
            options,
            functions: Vec::new(),
            bindings: Vec::new(),
            scopes: Vec::new(),
            open: Vec::new(),
            funcs: Vec::new(),
            pending: Vec::new(),
            arg_count: 0,
            frames: 0,
        };
        parser.script()
    }

    // -- the parser ----------------------------------------------------------

    struct Parser<'a> {
        tokens: &'a [Token],
        pos: usize,
        options: Options,
        functions: Vec<Function>,
        bindings: Vec<Binding>,
        /// Every scope ever opened, kept after it closes: a pending reference
        /// names the scope it was written in, and it is resolved long after
        /// that scope has been left.
        scopes: Vec<Scope>,
        /// The scopes currently open, innermost last.
        open: Vec<usize>,
        /// The functions currently being parsed, innermost last.
        funcs: Vec<FuncId>,
        pending: Vec<Pending>,
        arg_count: u32,
        /// Frames of recursive descent currently on the native stack, plus the
        /// depth of the tree built so far -- see [`MAX_FRAMES`].
        frames: u32,
    }

    struct Scope {
        parent: Option<usize>,
        func: FuncId,
        /// A function scope is where `var` lands and where the search for a
        /// `var`'s conflicting neighbours stops.
        is_function: bool,
        /// The names this scope binds. A `Vec` and not a map: a scope holds a
        /// handful of names, and a linear scan over one is faster than hashing
        /// as well as being deterministic.
        names: Vec<(String, BindingId)>,
    }

    /// One name occurrence, waiting for the whole program to be read.
    struct Pending {
        name: String,
        offset: usize,
        scope: usize,
        func: FuncId,
        role: Role,
    }

    /// What the occurrence does to the binding. The three answers differ:
    /// only a write can hit a `const`, and only a call can be lowered as a
    /// *direct* call when the binding turns out to name a known function. A
    /// read of such a binding is the same function as a value, which is why
    /// the two share one [`Res`].
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Role {
        Read,
        Write,
        Call,
    }

    impl Parser<'_> {
        // -- cursor ----------------------------------------------------------

        fn peek(&self) -> &Token {
            &self.tokens[self.pos]
        }

        fn kind(&self) -> &TokenKind {
            &self.peek().kind
        }

        fn at(&self, kind: &TokenKind) -> bool {
            self.kind() == kind
        }

        fn advance(&mut self) {
            if self.pos + 1 < self.tokens.len() {
                self.pos += 1;
            }
        }

        fn eat(&mut self, kind: &TokenKind) -> bool {
            let found = self.at(kind);
            if found {
                self.advance();
            }
            found
        }

        fn expect(&mut self, kind: &TokenKind, what: &str) -> Result<(), CompileError> {
            if self.eat(kind) {
                return Ok(());
            }
            Err(self.cannot_use(what))
        }

        /// The diagnostic for a token the parser has no use for here.
        ///
        /// A token whose capability this pipeline genuinely lacks names it;
        /// that table lives in the lexer so the wording does not drift as
        /// lexemes graduate. Everything else is a structural refusal, phrased
        /// as what the engine was looking for.
        ///
        /// # Why the phrase is not simply taken whenever the lexer has one
        ///
        /// [`TokenKind::capability`] serves two pipelines. M0's expression
        /// compiler really lacks every capability in that table, so for it the
        /// phrase is always true. M1 has almost all of them, and reaching for
        /// the phrase here disclaimed capabilities this engine demonstrably
        /// has: `else { }` was refused with "this engine does not support the
        /// `else` keyword yet" while `if (0) { } else { }` compiled and ran,
        /// and the milestone that landed `?:` and `throw` added two more of
        /// exactly that shape. Disclaiming a capability the engine has is the
        /// same lie as blaming the author with the sign flipped, and it is
        /// worse to act on: it sends the reader hunting for a workaround to a
        /// feature that shipped.
        ///
        /// So this position trusts the phrase for exactly the tokens M1 cannot
        /// lower **anywhere**, which is [`unlowered_by_m1`], and says what it
        /// wanted for the rest. That is the same rule [`Parser::semicolon`]
        /// already reached on its own; this generalises it rather than
        /// repeating it. The remaining debt is narrower and is recorded in
        /// `control_conformance.rs`: a phrase that is *true* can still be the
        /// wrong answer for the position, as `catch (e, f)`'s "comma operator"
        /// is.
        fn cannot_use(&self, what: &str) -> CompileError {
            let token = self.peek();
            match &token.kind {
                TokenKind::Unsupported(u) => unsupported(u.boundary, &u.phrase, token.offset),
                TokenKind::Eof => {
                    malformed(&format!("{what}; the source ends first"), token.offset)
                }
                other => match other.capability().filter(|_| unlowered_by_m1(other)) {
                    Some(u) => unsupported(u.boundary, &u.phrase, token.offset),
                    None => malformed(
                        &format!("{what}, and found {} instead", other.name()),
                        token.offset,
                    ),
                },
            }
        }

        /// The end of a statement: a written `;`, or one ECMA-262 12.10 rules
        /// 1 and 2 say is there. The lexer settles the half that is a fact
        /// about the token stream; this is the half that needs a parser --
        /// nothing here can use the token, which is the rest of rule 1.
        ///
        /// # Why this does not reach for a capability phrase
        ///
        /// [`Parser::cannot_use`] prefers the token's own capability phrase,
        /// and at this position that phrase is usually a lie. The statement
        /// dispatch's last arm takes *any* token as the start of an expression
        /// statement, so a token standing here is the next statement's first
        /// token and not one this position failed to lower.
        /// `o.m = function (x) { return x; } function rec() {}` -- two
        /// statements, one line, no `;` -- was refused with "this engine does
        /// not support the `function` keyword yet", and the engine has
        /// supported that keyword since M1. Disclaiming a capability the
        /// engine *has* is the same lie as blaming the author with the sign
        /// flipped, and it is worse to act on: it sends the reader looking for
        /// a workaround for function declarations instead of at the missing
        /// `;`. `tests/function_conformance.rs` is where it was caught.
        ///
        /// Two kinds of token keep their phrase, because for them it is true.
        /// A `,` is a JavaScript operator that would have *continued* the
        /// expression and that this engine does not lower, so the expression
        /// stopped for the reason the phrase gives. A `:` used to be in that
        /// sentence for the same reason -- it was the second half of a `?:`
        /// -- and the milestone that landed the conditional took that meaning
        /// away; what a `:` here now means is a *label*, which is the phrase
        /// the lexer keeps for it and which is equally true at this position.
        /// And the lexer's `Unsupported` bucket is beyond the engine whatever
        /// the lexeme is -- `**` and `class` alike -- so naming it is never a
        /// lie.
        fn semicolon(&mut self) -> Result<(), CompileError> {
            if self.eat(&TokenKind::Semi) || semicolon_is_implied(self.tokens, self.pos) {
                return Ok(());
            }
            let token = self.peek();
            if matches!(
                token.kind,
                TokenKind::Unsupported(_) | TokenKind::Colon | TokenKind::Comma
            ) {
                return Err(self.cannot_use("needs a `;` to end the statement"));
            }
            let what = "needs a `;` to end the statement";
            match &token.kind {
                TokenKind::Eof => Err(malformed(
                    &format!("{what}; the source ends first"),
                    token.offset,
                )),
                other => Err(malformed(
                    &format!(
                        "{what}, and found {} instead; ECMA-262 12.10 supplies one only across a line break",
                        other.name()
                    ),
                    token.offset,
                )),
            }
        }

        /// A `;` that was written rather than inserted.
        ///
        /// ECMA-262 12.10: "a semicolon is never inserted automatically if the
        /// semicolon would then be parsed as one of the two semicolons in the
        /// header of a `for` statement". The lexer applies rule 3 to the whole
        /// token stream and says in its own header that this override is a
        /// grammar position and belongs to the caller. This is the caller.
        fn at_written_semi(&self) -> bool {
            self.at(&TokenKind::Semi) && !self.peek().inserted
        }

        /// One of the two `;` in a `for` header. An inserted one is not one of
        /// these, so the header is short a semicolon and the statement is not
        /// a `for` at all -- which is a very different thing from a `for` whose
        /// parts the engine read differently.
        fn header_semicolon(&mut self, what: &str) -> Result<(), CompileError> {
            if self.at(&TokenKind::Semi) && self.peek().inserted {
                return Err(malformed(
                    &format!(
                        "{what}; the line break here does not supply one, because ECMA-262 12.10 never inserts a `for` header's semicolons"
                    ),
                    self.peek().offset,
                ));
            }
            self.expect(&TokenKind::Semi, what)
        }

        // -- native stack ----------------------------------------------------

        /// Charge `frames` of descent against [`MAX_FRAMES`], refusing when the
        /// budget runs out. Paired with [`Parser::shallower`].
        ///
        /// A failing path does not have to restore the count: the first error
        /// ends the whole parse, and there is nothing left to charge against
        /// it. Only the success paths give their frames back.
        fn deeper(&mut self, frames: u32) -> Result<(), CompileError> {
            self.frames += frames;
            if self.frames > MAX_FRAMES {
                return Err(unsupported(
                    Boundary::Subset,
                    &format!("syntax nested deeper than this engine's {MAX_FRAMES}-frame budget"),
                    self.peek().offset,
                ));
            }
            Ok(())
        }

        fn shallower(&mut self, frames: u32) {
            self.frames -= frames;
        }

        // -- scopes ----------------------------------------------------------

        fn scope(&self) -> usize {
            *self.open.last().expect("a scope is always open")
        }

        fn func(&self) -> FuncId {
            *self.funcs.last().expect("a function is always open")
        }

        fn enter(&mut self, is_function: bool, func: FuncId) {
            let parent = self.open.last().copied();
            self.scopes.push(Scope {
                parent,
                func,
                is_function,
                names: Vec::new(),
            });
            self.open.push(self.scopes.len() - 1);
        }

        fn leave(&mut self) {
            self.open.pop();
        }

        /// Bind `name`, in the scope its declaration form belongs to: the
        /// nearest function scope for a `var`, the current one for everything
        /// else. Returns the binding, which for a repeated `var` is the one
        /// that is already there.
        fn declare(
            &mut self,
            name: &str,
            kind: BindingKind,
            span: Span,
        ) -> Result<BindingId, CompileError> {
            let target = if kind == BindingKind::Var {
                self.enclosing_function_scope()
            } else {
                self.scope()
            };

            // A `var` may be declared again, and only a `var` may: ECMA-262
            // 14.3.2. Anything else in the way -- including a `let` in a block
            // the `var` reaches out of -- is a collision.
            let mut scan = Some(self.scope());
            while let Some(id) = scan {
                if let Some(found) = self.lookup(id, name) {
                    let existing = &self.bindings[found.0 as usize];
                    if kind == BindingKind::Var && existing.kind == BindingKind::Var {
                        return Ok(found);
                    }
                    return Err(malformed(
                        &format!(
                            "cannot bind `{name}` twice in one scope; it is already bound at byte {}",
                            existing.span.offset()
                        ),
                        span.offset(),
                    ));
                }
                if id == target {
                    break;
                }
                scan = self.scopes[id].parent;
            }

            let func = self.scopes[target].func;
            let slot = self.functions[func.0 as usize].bindings.len() as u32;
            let id = BindingId(self.bindings.len() as u32);
            self.bindings.push(Binding {
                name: name.to_string(),
                kind,
                span,
                // Filled in by `declaration` for the two forms that have a
                // dead zone; everything else is initialised on scope entry.
                initialised: None,
                func,
                slot,
                // Set by `resolve_one` the first time a nested function reads
                // it. Nothing else may set it: a binding is captured exactly
                // when an occurrence resolves from inside another function.
                captured: false,
            });
            self.functions[func.0 as usize].bindings.push(id);
            self.scopes[target].names.push((name.to_string(), id));
            Ok(id)
        }

        fn lookup(&self, scope: usize, name: &str) -> Option<BindingId> {
            self.scopes[scope]
                .names
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, id)| *id)
        }

        fn enclosing_function_scope(&self) -> usize {
            let mut id = self.scope();
            while !self.scopes[id].is_function {
                id = self.scopes[id]
                    .parent
                    .expect("a function scope encloses every scope");
            }
            id
        }

        /// Record one name occurrence. Nothing is resolved yet: a name may
        /// bind to a declaration that has not been read, so resolution waits
        /// for the whole program.
        fn occurrence(&mut self, text: &str, offset: usize, role: Role) -> Name {
            self.pending.push(Pending {
                name: text.to_string(),
                offset,
                scope: self.scope(),
                func: self.func(),
                role,
            });
            Name {
                text: text.to_string(),
                res: Res::Unresolved,
                occurrence: (self.pending.len() - 1) as u32,
            }
        }

        /// Re-label an occurrence once the parser has seen what surrounds it:
        /// the `(` that makes a name a callee, the `=` that makes it a target.
        fn relabel(&mut self, expr: &Expr, role: Role) -> bool {
            match &expr.kind {
                ExprKind::Name(name) => {
                    self.pending[name.occurrence as usize].role = role;
                    true
                }
                _ => false,
            }
        }

        /// Is this expression a target an assignment or an update may write
        /// to? ECMA-262 13.15.1: an AssignmentTargetType of ~simple~, which
        /// for this subset is an IdentifierReference or a MemberExpression.
        ///
        /// A name is relabelled on the way past, because writing to one asks
        /// resolution a different question than reading it -- a `const`, a
        /// function binding and a host import are all readable and none of
        /// them is writable. A property is not relabelled: what a property may
        /// hold is not a question resolution can answer, and the object it
        /// hangs off is an ordinary read.
        fn assignable(&mut self, expr: &Expr) -> bool {
            match &expr.kind {
                ExprKind::Name(_) => self.relabel(expr, Role::Write),
                ExprKind::Member { .. } => true,
                _ => false,
            }
        }

        // -- statements ------------------------------------------------------

        fn script(&mut self) -> Result<Program, CompileError> {
            if self.at(&TokenKind::Eof) {
                return Err(malformed(
                    "needs a statement to compile; this source is empty",
                    self.peek().offset,
                ));
            }
            let id = self.reserve(None, Span(0));
            debug_assert_eq!(id, Program::SCRIPT);
            self.funcs.push(id);
            self.enter(true, id);
            let body = self.statements_until(&TokenKind::Eof, "the source")?;
            self.leave();
            self.funcs.pop();
            self.functions[id.0 as usize].body = body;

            let resolutions = self.resolve()?;
            let mut program = Program {
                functions: std::mem::take(&mut self.functions),
                bindings: std::mem::take(&mut self.bindings),
                arg_count: self.arg_count,
            };
            for function in &mut program.functions {
                fill_stmts(&mut function.body, &resolutions);
            }
            Ok(program)
        }

        fn statements_until(
            &mut self,
            end: &TokenKind,
            opened: &str,
        ) -> Result<Vec<Stmt>, CompileError> {
            self.deeper(1)?;
            let mut body = Vec::new();
            while !self.at(end) {
                if self.at(&TokenKind::Eof) {
                    return Err(self.cannot_use(&format!("needs a `}}` to close {opened}")));
                }
                body.push(self.statement()?);
            }
            self.advance();
            self.shallower(1);
            Ok(body)
        }

        fn statement(&mut self) -> Result<Stmt, CompileError> {
            // Two, for one frame: this is the parser's largest, holding the
            // whole statement match and the diagnostics it formats, and a
            // nested block spends it on every level.
            self.deeper(2)?;
            let span = Span(self.peek().offset);
            let kind = match self.kind().clone() {
                TokenKind::Semi => {
                    self.advance();
                    StmtKind::Empty
                }
                TokenKind::LBrace => {
                    self.advance();
                    self.enter(false, self.func());
                    let body = self.statements_until(
                        &TokenKind::RBrace,
                        &format!("the block opened at byte {}", span.offset()),
                    );
                    self.leave();
                    StmtKind::Block(body?)
                }
                TokenKind::Let | TokenKind::Const | TokenKind::Var => {
                    let decls = self.declaration()?;
                    self.semicolon()?;
                    StmtKind::Decl(decls)
                }
                TokenKind::Function => self.function_declaration(span)?,
                TokenKind::If => self.if_statement()?,
                TokenKind::Try => self.try_statement()?,
                TokenKind::Throw => {
                    self.advance();
                    // ECMA-262 12.10 rule 3 makes `throw` a restricted
                    // production, and the lexer has already inserted the `;`
                    // a line break after it puts there. Unlike `return` there
                    // is no shorter production to fall back on, so a `;` here
                    // is not a `throw undefined`.
                    let token = self.peek().clone();
                    if token.kind == TokenKind::Semi {
                        return Err(malformed(
                            if token.inserted {
                                "needs the value to throw on the same line as the `throw`; ECMA-262 12.10 puts a `;` at that line break, so this reads as a `throw` with nothing to throw"
                            } else {
                                "needs a value after `throw`; ECMA-262 14.14 has no `throw` without one"
                            },
                            token.offset,
                        ));
                    }
                    let value = self.expression(0)?;
                    self.semicolon()?;
                    StmtKind::Throw(value)
                }
                TokenKind::While => self.while_statement()?,
                TokenKind::For => self.for_statement()?,
                TokenKind::Return => {
                    self.advance();
                    // The lexer has already inserted the `;` that ECMA-262
                    // 12.10 rule 3 puts after a lone `return`, so a value here
                    // really is on the same line.
                    let value = if self.at(&TokenKind::Semi)
                        || semicolon_is_implied(self.tokens, self.pos)
                    {
                        None
                    } else {
                        Some(self.expression(0)?)
                    };
                    self.semicolon()?;
                    StmtKind::Return(value)
                }
                _ => {
                    let expr = self.expression(0)?;
                    self.semicolon()?;
                    StmtKind::Expr(expr)
                }
            };
            self.shallower(2);
            Ok(Stmt { kind, span })
        }

        /// The `Statement` a body position takes: the `then` and `else` of an
        /// `if`, and the body of a `while` or a `for`.
        ///
        /// ECMA-262 14.6 and 14.7 spell those positions `Statement`, and a
        /// `Declaration` is not one. `if (c) let x = 1;` would bind a name that
        /// nothing could ever read, and `while (c) function f() {}` is Annex B
        /// sloppy-mode grammar -- this subset is strict mode, where it is not
        /// grammar at all. `var` is a `VariableStatement` and stays allowed.
        fn body_statement(&mut self, position: &str) -> Result<Stmt, CompileError> {
            let declaration = match self.kind() {
                TokenKind::Let | TokenKind::Const => "a lexical declaration",
                TokenKind::Function => "a function declaration",
                _ => {
                    self.deeper(1)?;
                    let out = self.statement();
                    self.shallower(1);
                    return out;
                }
            };
            Err(malformed(
                &format!(
                    "cannot read {declaration} as the body of {position}; only a statement belongs there, and a declaration needs a block of its own"
                ),
                self.peek().offset,
            ))
        }

        /// `try`/`catch`/`finally`, ECMA-262 14.15.
        ///
        /// All three parts are spelled `Block` in the grammar -- there is no
        /// `try if (c) x;` -- so each is read by [`Parser::braced_block`] and
        /// none of them goes through [`Parser::body_statement`].
        fn try_statement(&mut self) -> Result<StmtKind, CompileError> {
            self.deeper(1)?;
            let at = self.peek().offset;
            self.advance();
            let block = self.braced_block("the `try` block")?;
            let handler = if self.at(&TokenKind::Catch) {
                Some(self.catch_clause()?)
            } else {
                None
            };
            let finalizer = if self.eat(&TokenKind::Finally) {
                Some(self.braced_block("the `finally` block")?)
            } else {
                None
            };
            if handler.is_none() && finalizer.is_none() {
                return Err(malformed(
                    "needs a `catch` or a `finally` after the `try` block; ECMA-262 14.15 has no `try` with neither, and one with neither could only mean the block itself",
                    at,
                ));
            }
            self.shallower(1);
            Ok(StmtKind::Try {
                block,
                handler,
                finalizer,
            })
        }

        /// A `{ ... }` with a scope of its own, read as a statement list.
        fn braced_block(&mut self, what: &str) -> Result<Vec<Stmt>, CompileError> {
            let open = self.peek().offset;
            self.expect(&TokenKind::LBrace, &format!("needs a `{{` to open {what}"))?;
            self.enter(false, self.func());
            let body =
                self.statements_until(&TokenKind::RBrace, &format!("{what} opened at byte {open}"));
            self.leave();
            body
        }

        /// One `catch` clause.
        ///
        /// The CatchParameter gets a scope of its own that the block's scope
        /// nests inside, which is ECMA-262 14.15.4's two environments: it is
        /// what makes the parameter shadow an outer name of the same
        /// spelling, and what keeps a `let` of that name inside the block
        /// from colliding with it.
        fn catch_clause(&mut self) -> Result<Catch, CompileError> {
            self.deeper(1)?;
            self.advance();
            self.enter(false, self.func());
            let out = self.catch_rest();
            self.leave();
            self.shallower(1);
            out
        }

        fn catch_rest(&mut self) -> Result<Catch, CompileError> {
            // 14.15 makes the parameter optional: `catch { }` is a catch
            // clause that does not name what it caught.
            let param = if self.eat(&TokenKind::LParen) {
                let token = self.peek().clone();
                let TokenKind::Ident(name) = token.kind else {
                    return Err(self.cannot_use("needs a name for the `catch` parameter"));
                };
                self.advance();
                // A `let`-like binding: writable, and initialised on entry to
                // the clause rather than by a declarator, so it has no
                // temporal dead zone and `declare` leaves `initialised` unset.
                let id = self.declare(&name, BindingKind::Let, Span(token.offset))?;
                self.expect(
                    &TokenKind::RParen,
                    "needs a `)` to close the `catch` parameter",
                )?;
                Some(id)
            } else {
                None
            };
            let body = self.braced_block("the `catch` block")?;
            Ok(Catch { param, body })
        }

        fn if_statement(&mut self) -> Result<StmtKind, CompileError> {
            self.deeper(1)?;
            self.advance();
            self.expect(&TokenKind::LParen, "needs a `(` after `if`")?;
            let test = self.expression(0)?;
            self.expect(
                &TokenKind::RParen,
                "needs a `)` to close the `if` condition",
            )?;
            let then = Box::new(self.body_statement("an `if`")?);
            // The dangling `else` binds to the nearest `if`, which is what
            // taking it here -- before returning to the outer statement --
            // means.
            let alt = if self.eat(&TokenKind::Else) {
                Some(Box::new(self.body_statement("an `else`")?))
            } else {
                None
            };
            self.shallower(1);
            Ok(StmtKind::If { test, then, alt })
        }

        fn while_statement(&mut self) -> Result<StmtKind, CompileError> {
            self.deeper(1)?;
            self.advance();
            self.expect(&TokenKind::LParen, "needs a `(` after `while`")?;
            let test = self.expression(0)?;
            self.expect(
                &TokenKind::RParen,
                "needs a `)` to close the `while` condition",
            )?;
            let body = Box::new(self.body_statement("a `while`")?);
            self.shallower(1);
            Ok(StmtKind::While { test, body })
        }

        /// A three-part `for`. The header gets its own scope, so `for (let i
        /// = 0; ...)` binds an `i` the body can see and the statement after
        /// the loop cannot.
        ///
        /// The `;` in a header is never inserted: ECMA-262 12.10 says so, and
        /// it is why this reads them itself instead of calling
        /// [`Parser::semicolon`].
        fn for_statement(&mut self) -> Result<StmtKind, CompileError> {
            // Two, for this frame and `for_parts`.
            self.deeper(2)?;
            self.advance();
            self.expect(&TokenKind::LParen, "needs a `(` after `for`")?;
            self.enter(false, self.func());
            let parts = self.for_parts();
            self.leave();
            self.shallower(2);
            parts
        }

        fn for_parts(&mut self) -> Result<StmtKind, CompileError> {
            let span = Span(self.peek().offset);
            let init = if self.at_written_semi() {
                self.advance();
                None
            } else {
                let kind = match self.kind() {
                    TokenKind::Let | TokenKind::Const | TokenKind::Var => {
                        StmtKind::Decl(self.declaration()?)
                    }
                    _ => StmtKind::Expr(self.expression(0)?),
                };
                self.header_semicolon("needs a `;` after the first part of the `for` header")?;
                Some(Box::new(Stmt { kind, span }))
            };
            let test = if self.at_written_semi() {
                self.advance();
                None
            } else {
                let test = self.expression(0)?;
                self.header_semicolon("needs a `;` after the condition of the `for` header")?;
                Some(test)
            };
            let update = if self.at(&TokenKind::RParen) {
                None
            } else {
                Some(self.expression(0)?)
            };
            self.expect(&TokenKind::RParen, "needs a `)` to close the `for` header")?;
            let body = Box::new(self.body_statement("a `for`")?);
            Ok(StmtKind::For {
                init,
                test,
                update,
                body,
            })
        }

        /// `let`/`const`/`var`, one [`Declarator`] per name.
        ///
        /// The initialiser is parsed *before* the name is bound, because a
        /// `const f = function(){}` is a known call target and the binding has
        /// to say so. It makes no difference to what the initialiser can see:
        /// nothing is resolved until the whole program is read.
        fn declaration(&mut self) -> Result<Vec<Declarator>, CompileError> {
            let keyword = self.kind().clone();
            let kind = match keyword {
                TokenKind::Let => BindingKind::Let,
                TokenKind::Const => BindingKind::Const,
                _ => BindingKind::Var,
            };
            // `var` is initialised to `undefined` when its scope is entered;
            // the other two are not initialised until their declarator runs.
            let lexical = kind != BindingKind::Var;
            let word = keyword.name();
            self.advance();
            let mut out = Vec::new();
            loop {
                let token = self.peek().clone();
                let TokenKind::Ident(text) = token.kind else {
                    return Err(self.cannot_use(&format!("needs a name after {word}")));
                };
                self.advance();
                let span = Span(token.offset);
                let init = if self.eat(&TokenKind::Eq) {
                    Some(self.expression(BP_ASSIGN)?)
                } else {
                    if kind == BindingKind::Const {
                        return Err(malformed(
                            &format!(
                                "needs a value for the `const` binding `{text}`; a `const` can never be assigned one later"
                            ),
                            span.offset(),
                        ));
                    }
                    None
                };
                // A `const` bound directly to a function expression is a name
                // that holds one known function and can never be reassigned:
                // exactly what a declaration is, so it is callable on the same
                // terms. A `let` or `var` is not, because a later assignment
                // could put anything there.
                let kind = match (&init, kind) {
                    (
                        Some(Expr {
                            kind: ExprKind::Function(id),
                            ..
                        }),
                        BindingKind::Const,
                    ) => BindingKind::Function(*id),
                    _ => kind,
                };
                let binding = self.declare(&text, kind, span)?;
                // ECMA-262 8.2.4 and 13.1.3: a `let` or `const` exists from
                // the top of its scope but holds no value until its declarator
                // finishes, and every read before that point is a
                // ReferenceError. The cursor is exactly at that point now, so
                // this is where the dead zone ends; `classify` compares an
                // occurrence against it.
                if lexical {
                    self.bindings[binding.0 as usize].initialised = Some(self.peek().offset);
                }
                out.push(Declarator {
                    binding,
                    init,
                    span,
                });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(out)
        }

        // -- functions -------------------------------------------------------

        /// Reserve a [`FuncId`] before the body is read, so a recursive call
        /// and a `const f = function(){}` can both name the function that is
        /// still being parsed.
        fn reserve(&mut self, name: Option<String>, span: Span) -> FuncId {
            let id = FuncId(self.functions.len() as u32);
            self.functions.push(Function {
                name,
                params: Vec::new(),
                bindings: Vec::new(),
                body: Vec::new(),
                span,
                captures: Vec::new(),
            });
            id
        }

        fn function_declaration(&mut self, span: Span) -> Result<StmtKind, CompileError> {
            self.advance();
            let token = self.peek().clone();
            let TokenKind::Ident(name) = token.kind else {
                return Err(self.cannot_use("needs a name for the function declared here"));
            };
            self.advance();
            let func = self.reserve(Some(name.clone()), span);
            // The name binds in the *enclosing* scope, and it binds before the
            // body is read, which is the whole of hoisting for a declaration.
            let binding = self.declare(&name, BindingKind::Function(func), Span(token.offset))?;
            self.function_rest(func, None)?;
            Ok(StmtKind::Func { binding, func })
        }

        fn function_expression(&mut self, span: Span) -> Result<ExprKind, CompileError> {
            self.advance();
            let name = match self.kind().clone() {
                TokenKind::Ident(name) => {
                    let at = Span(self.peek().offset);
                    self.advance();
                    Some((name, at))
                }
                _ => None,
            };
            let func = self.reserve(name.as_ref().map(|(n, _)| n.clone()), span);
            // ECMA-262 15.2.5: a named function expression can see its own
            // name, and only from inside, so the binding goes in the
            // function's own scope rather than the enclosing one.
            self.function_rest(func, name)?;
            Ok(ExprKind::Function(func))
        }

        /// Parameters and body, with the function's scope open around both.
        fn function_rest(
            &mut self,
            func: FuncId,
            self_name: Option<(String, Span)>,
        ) -> Result<(), CompileError> {
            self.funcs.push(func);
            self.enter(true, func);
            let parsed = self.function_body(func, self_name);
            self.leave();
            self.funcs.pop();
            parsed
        }

        fn function_body(
            &mut self,
            func: FuncId,
            self_name: Option<(String, Span)>,
        ) -> Result<(), CompileError> {
            self.expect(&TokenKind::LParen, "needs a `(` to open the parameter list")?;
            let mut params = Vec::new();
            while !self.eat(&TokenKind::RParen) {
                let token = self.peek().clone();
                let TokenKind::Ident(name) = token.kind else {
                    return Err(self.cannot_use("needs a parameter name here"));
                };
                self.advance();
                params.push(self.declare(&name, BindingKind::Param, Span(token.offset))?);
                if !self.eat(&TokenKind::Comma) {
                    self.expect(
                        &TokenKind::RParen,
                        "needs a `,` or a `)` in the parameter list",
                    )?;
                    break;
                }
            }
            // The function expression's own name, after the parameters and
            // never before them. ECMA-262 15.2.5 puts that binding in a
            // function environment the parameter list *shadows*, so a
            // parameter of the same name wins and is not a collision; and the
            // lowering reads a parameter out of the local its slot names, so a
            // self-name holding slot 0 would shift every parameter by one and
            // silently read the wrong argument.
            if let Some((name, at)) = self_name
                && self.lookup(self.scope(), &name).is_none()
            {
                self.declare(&name, BindingKind::Function(func), at)?;
            }
            let open = self.peek().offset;
            self.expect(&TokenKind::LBrace, "needs a `{` to open the function body")?;
            let body = self.statements_until(
                &TokenKind::RBrace,
                &format!("the function body opened at byte {open}"),
            )?;
            let function = &mut self.functions[func.0 as usize];
            function.params = params;
            function.body = body;
            Ok(())
        }

        // -- expressions -----------------------------------------------------

        /// One expression, consuming infix operators whose left binding power
        /// is at least `min_bp`.
        fn expression(&mut self, min_bp: u8) -> Result<Expr, CompileError> {
            self.deeper(1)?;
            let mut lhs = self.unary()?;
            // A left-associative chain costs no frame *here* -- it is this
            // loop and not recursion -- but it builds a tree that leans one
            // node deeper to the left per operator, and every consumer of that
            // tree walks it recursively. So the chain is charged too, and the
            // charge is released with this frame's.
            let mut chain = 0;
            loop {
                // 13.14 has no row in `infix`: its middle operand sits
                // between two tokens rather than after one, and the table is
                // `(what it builds, left power, right power)` and nothing
                // else. It takes its rung of the same ladder all the same.
                if self.at(&TokenKind::Question) {
                    if BP_CONDITIONAL < min_bp {
                        break;
                    }
                    self.deeper(1)?;
                    chain += 1;
                    self.advance();
                    // Both branches are an AssignmentExpression. That is what
                    // makes `a ? b : c ? d : e` group to the right: BP_ASSIGN
                    // is below BP_CONDITIONAL, so a second `?` is read inside
                    // the first one's else branch rather than beside it.
                    let then = self.expression(BP_ASSIGN)?;
                    self.expect(
                        &TokenKind::Colon,
                        "needs a `:` between the two branches of the conditional",
                    )?;
                    let alt = self.expression(BP_ASSIGN)?;
                    let span = lhs.span;
                    lhs = Expr {
                        kind: ExprKind::Conditional {
                            test: Box::new(lhs),
                            then: Box::new(then),
                            alt: Box::new(alt),
                        },
                        span,
                    };
                    continue;
                }
                let Some((what, lbp, rbp)) = infix(self.kind()) else {
                    break;
                };
                if lbp < min_bp {
                    break;
                }
                self.deeper(1)?;
                chain += 1;
                let at = self.peek().offset;
                self.advance();
                let rhs = self.expression(rbp)?;
                let span = lhs.span;
                let kind = match what {
                    Infix::Binary(op) => ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    Infix::Logical(op) => ExprKind::Logical(op, Box::new(lhs), Box::new(rhs)),
                    Infix::Assign(op) => {
                        if !self.assignable(&lhs) {
                            return Err(malformed(
                                "needs a name or a property on the left of an assignment; this engine has nothing else to assign to yet",
                                at,
                            ));
                        }
                        ExprKind::Assign {
                            op,
                            target: Box::new(lhs),
                            value: Box::new(rhs),
                        }
                    }
                };
                lhs = Expr { kind, span };
            }
            self.shallower(1 + chain);
            Ok(lhs)
        }

        fn unary(&mut self) -> Result<Expr, CompileError> {
            self.deeper(1)?;
            let token = self.peek().clone();
            let span = Span(token.offset);
            let op = match token.kind {
                TokenKind::Minus => {
                    self.advance();
                    // Fold the minus onto a literal. Not an optimisation:
                    // `i32::MIN` has no positive counterpart, so
                    // `-2147483648` is only representable if the sign reaches
                    // the literal.
                    //
                    // A zero magnitude is the one case the fold must decline.
                    // The folded literal is an `i32` and `i32` has one zero,
                    // so folding would hand the lowering `Int(0)` and `-0`
                    // would come out as `+0` -- a Number ECMA-262 6.1.6.1
                    // distinguishes from `0`, and the only Number whose
                    // reciprocal is `-Infinity`. Left unfolded it is
                    // `Unary(Neg, Int(0))`, and 13.5.5's sign flip on a
                    // Number is exactly what produces `-0`.
                    if let TokenKind::Int(magnitude) = self.kind().clone()
                        && magnitude != 0
                    {
                        let at = self.peek().offset;
                        self.advance();
                        let kind = int_literal(magnitude, true, at)?;
                        self.shallower(1);
                        return Ok(Expr { kind, span });
                    }
                    UnaryOp::Neg
                }
                TokenKind::Plus => {
                    self.advance();
                    UnaryOp::Plus
                }
                TokenKind::Bang => {
                    self.advance();
                    UnaryOp::Not
                }
                // 13.5.3. A keyword rather than punctuation, but the same
                // rung of the ladder as `!`, `+` and `-`, so it parses here
                // and its operand is a UnaryExpression.
                TokenKind::Typeof => {
                    self.advance();
                    UnaryOp::TypeOf
                }
                TokenKind::PlusPlus | TokenKind::MinusMinus => {
                    let op = if token.kind == TokenKind::PlusPlus {
                        UpdateOp::Inc
                    } else {
                        UpdateOp::Dec
                    };
                    self.advance();
                    let target = self.unary()?;
                    let out = self.update(op, true, target, span);
                    self.shallower(1);
                    return out;
                }
                _ => {
                    let out = self.postfix();
                    self.shallower(1);
                    return out;
                }
            };
            let operand = self.expression(BP_PREFIX)?;
            self.shallower(1);
            Ok(Expr {
                kind: ExprKind::Unary(op, Box::new(operand)),
                span,
            })
        }

        fn update(
            &mut self,
            op: UpdateOp,
            prefix: bool,
            target: Expr,
            span: Span,
        ) -> Result<Expr, CompileError> {
            if !self.assignable(&target) {
                return Err(malformed(
                    "needs a name or a property to increment or decrement; this engine has nothing else to write back to yet",
                    span.offset(),
                ));
            }
            Ok(Expr {
                kind: ExprKind::Update {
                    op,
                    prefix,
                    target: Box::new(target),
                },
                span,
            })
        }

        /// A primary expression and whatever trails it: a member access, a
        /// call, an update.
        ///
        /// Each link of the chain is charged a frame even though the loop is
        /// iterative, for the reason [`Parser::expression`] charges its infix
        /// chain: `a.b.c.d` leans one node deeper per link, and every consumer
        /// of the tree walks it recursively.
        fn postfix(&mut self) -> Result<Expr, CompileError> {
            self.deeper(1)?;
            let mut expr = self.primary()?;
            let mut chain = 0;
            loop {
                match self.kind() {
                    // 13.3.2: `o.a`, where `a` is an IdentifierName -- which
                    // includes every keyword, so `o.for` is legal JavaScript.
                    TokenKind::Dot => {
                        self.deeper(1)?;
                        chain += 1;
                        let span = expr.span;
                        self.advance();
                        let token = self.peek().clone();
                        let Some(name) = identifier_name(&token.kind) else {
                            return Err(
                                self.not_a_property_name("needs a property name after the `.`")
                            );
                        };
                        self.advance();
                        expr = Expr {
                            kind: ExprKind::Member {
                                object: Box::new(expr),
                                key: MemberKey::Static(name),
                            },
                            span,
                        };
                    }
                    // 13.3.3: `o[e]`, where `e` is a full Expression.
                    TokenKind::LBracket => {
                        self.deeper(1)?;
                        chain += 1;
                        let span = expr.span;
                        let open = self.peek().offset;
                        self.advance();
                        let key = self.expression(0)?;
                        self.expect(
                            &TokenKind::RBracket,
                            &format!(
                                "needs a `]` to close the property access opened at byte {open}"
                            ),
                        )?;
                        expr = Expr {
                            kind: ExprKind::Member {
                                object: Box::new(expr),
                                key: MemberKey::Computed(Box::new(key)),
                            },
                            span,
                        };
                    }
                    TokenKind::LParen => {
                        let span = expr.span;
                        // A *name* followed by `(` asks resolution a different
                        // question than a name read for its value: only a call
                        // can reach a known function directly. Every other
                        // callee -- a property, a call's result, a
                        // parenthesised function expression -- is an ordinary
                        // value expression, and the lowering calls it through
                        // the table. Nothing is rejected here any more: what a
                        // value turns out to hold is not a question the text
                        // can answer, and ECMA-262 13.3.6.1 makes it a
                        // run-time TypeError rather than a syntax one.
                        self.relabel(&expr, Role::Call);
                        let args = self.arguments()?;
                        expr = Expr {
                            kind: ExprKind::Call {
                                callee: Box::new(expr),
                                args,
                            },
                            span,
                        };
                    }
                    TokenKind::PlusPlus | TokenKind::MinusMinus => {
                        let op = if self.at(&TokenKind::PlusPlus) {
                            UpdateOp::Inc
                        } else {
                            UpdateOp::Dec
                        };
                        let span = expr.span;
                        self.advance();
                        expr = self.update(op, false, expr, span)?;
                    }
                    _ => break,
                }
            }
            self.shallower(1 + chain);
            Ok(expr)
        }

        fn arguments(&mut self) -> Result<Vec<Expr>, CompileError> {
            self.deeper(1)?;
            let open = self.peek().offset;
            self.advance();
            let mut args = Vec::new();
            while !self.eat(&TokenKind::RParen) {
                // An argument is an AssignmentExpression, which is what stops
                // the comma separating them from being the comma operator.
                args.push(self.expression(BP_ASSIGN)?);
                if !self.eat(&TokenKind::Comma) {
                    self.expect(
                        &TokenKind::RParen,
                        &format!("needs a `,` or a `)` in the argument list opened at byte {open}"),
                    )?;
                    break;
                }
            }
            self.shallower(1);
            Ok(args)
        }

        fn primary(&mut self) -> Result<Expr, CompileError> {
            self.deeper(1)?;
            let token = self.peek().clone();
            let span = Span(token.offset);
            let kind = match token.kind {
                TokenKind::Int(magnitude) => {
                    self.advance();
                    int_literal(magnitude, false, token.offset)?
                }
                TokenKind::Num(value) => {
                    self.advance();
                    ExprKind::Num(value)
                }
                TokenKind::Str(value) => {
                    self.advance();
                    ExprKind::Str(value)
                }
                TokenKind::True => {
                    self.advance();
                    ExprKind::Bool(true)
                }
                TokenKind::False => {
                    self.advance();
                    ExprKind::Bool(false)
                }
                TokenKind::Null => {
                    self.advance();
                    ExprKind::Null
                }
                TokenKind::Undefined => {
                    self.advance();
                    ExprKind::Undefined
                }
                TokenKind::Arg(index) => {
                    self.advance();
                    if index >= super::MAX_ARGS {
                        return Err(unsupported(
                            Boundary::Subset,
                            TOO_MANY_ARGUMENTS,
                            token.offset,
                        ));
                    }
                    // `$N` is a parameter of the script, so naming one from
                    // inside a function is a capture and not an argument.
                    if self.func() != Program::SCRIPT {
                        return Err(unsupported(
                            Boundary::FullJs,
                            "an argument reference inside a nested function",
                            token.offset,
                        ));
                    }
                    self.arg_count = self.arg_count.max(index as u32 + 1);
                    ExprKind::Arg(index as u32)
                }
                TokenKind::Ident(name) => {
                    self.advance();
                    ExprKind::Name(self.occurrence(&name, token.offset, Role::Read))
                }
                TokenKind::Function => self.function_expression(span)?,
                TokenKind::LParen => {
                    self.advance();
                    let inner = self.expression(0)?;
                    self.expect(
                        &TokenKind::RParen,
                        &format!(
                            "needs a `)` to close the group opened at byte {}",
                            token.offset
                        ),
                    )?;
                    self.shallower(1);
                    return Ok(inner);
                }
                // In statement position a `{` opens a Block, and `statement`
                // has already taken it (ECMA-262 14.2 wins over 13.2.5 there,
                // which is why `{}` alone is an empty block and `({})` is the
                // object). Here it can only be an ObjectLiteral.
                TokenKind::LBrace => self.object_literal()?,
                // 13.2.4. In expression position a `[` can only open an
                // ArrayLiteral: the other thing a `[` spells is a computed
                // member access, and that one is read by the postfix loop
                // after an operand, never here.
                TokenKind::LBracket => self.array_literal()?,
                _ => return Err(self.cannot_use("needs an operand here")),
            };
            self.shallower(1);
            Ok(Expr { kind, span })
        }

        // -- array literals --------------------------------------------------

        /// An ArrayLiteral, ECMA-262 13.2.4. The `[` is under the cursor.
        ///
        /// **Elisions are refused, not represented.** `[1, , 3]` is a
        /// three-element array whose middle element is a *hole*, and a hole is
        /// distinguishable from `undefined` only through `in`,
        /// `hasOwnProperty`, `Object.keys` and the iteration methods that skip
        /// it -- none of which this engine has. Reading it as `undefined`
        /// would therefore be unobservably wrong today and silently wrong the
        /// day one of those arrives, which is the worse of the two failures.
        /// Refusing by name costs a script nothing: nobody writes an elision
        /// on purpose.
        ///
        /// A trailing comma is not an elision (12.9.6) and is accepted, the
        /// same way [`Parser::object_literal`] accepts one.
        fn array_literal(&mut self) -> Result<ExprKind, CompileError> {
            self.deeper(2)?;
            let open = self.peek().offset;
            self.advance();
            let mut elements = Vec::new();
            while !self.eat(&TokenKind::RBracket) {
                if self.peek().kind == TokenKind::Comma {
                    return Err(unsupported(
                        Boundary::FullJs,
                        "elisions in an array literal",
                        self.peek().offset,
                    ));
                }
                elements.push(self.expression(BP_ASSIGN)?);
                if !self.eat(&TokenKind::Comma) {
                    self.expect(
                        &TokenKind::RBracket,
                        &format!("needs a `,` or a `]` in the array literal opened at byte {open}"),
                    )?;
                    break;
                }
            }
            self.shallower(2);
            Ok(ExprKind::Array(elements))
        }

        // -- object literals -------------------------------------------------

        /// An ObjectLiteral, ECMA-262 13.2.5. The `{` is under the cursor.
        ///
        /// Two frames: this one and [`Parser::property`], which a nested
        /// literal spends again at every level.
        fn object_literal(&mut self) -> Result<ExprKind, CompileError> {
            self.deeper(2)?;
            let open = self.peek().offset;
            self.advance();
            let mut properties = Vec::new();
            while !self.eat(&TokenKind::RBrace) {
                properties.push(self.property()?);
                if !self.eat(&TokenKind::Comma) {
                    // ECMA-262 12.9.6 allows the trailing comma, which is why
                    // the `}` is looked for after the separator and not
                    // instead of it.
                    self.expect(
                        &TokenKind::RBrace,
                        &format!(
                            "needs a `,` or a `}}` in the object literal opened at byte {open}"
                        ),
                    )?;
                    break;
                }
            }
            self.shallower(2);
            Ok(ExprKind::Object(properties))
        }

        /// One PropertyDefinition.
        ///
        /// The forms this engine reads are `key: value` and the shorthand
        /// `key`. Every other form 13.2.5 defines -- a method, an accessor, a
        /// ComputedPropertyName, a spread -- is refused by name here rather
        /// than being read as one of the two and failing somewhere else. A
        /// half-implemented accessor is worse than an absent one: it would
        /// store the function under the name `get` and never call it.
        fn property(&mut self) -> Result<Property, CompileError> {
            let token = self.peek().clone();
            let span = Span(token.offset);
            if token.kind == TokenKind::LBracket {
                return Err(unsupported(
                    Boundary::FullJs,
                    "computed property keys",
                    token.offset,
                ));
            }
            // 13.2.5.1: a PropertyName is a String, whichever of the three
            // ways it is written. Only the third can also be a shorthand.
            let mut shorthand = false;
            let key = match &token.kind {
                TokenKind::Str(text) => {
                    let text = text.clone();
                    self.advance();
                    text
                }
                // A fractional key is refused by name rather than
                // approximated: 13.2.5.1 makes the key the *String* of the
                // Number, which for `1.5` means running ECMA-262 6.1.6.1.20 at
                // compile time. That algorithm exists in this engine
                // (`__num_to_string`, Dragon4) and it runs in the *guest*;
                // reimplementing it in the compiler would be a second answer
                // to one question, and the two would eventually disagree.
                TokenKind::Num(_) => {
                    return Err(unsupported(
                        Boundary::Subset,
                        "fractional property keys",
                        token.offset,
                    ));
                }
                TokenKind::Int(magnitude) => {
                    let magnitude = *magnitude;
                    if magnitude > i32::MAX as u64 {
                        return Err(unsupported(
                            Boundary::Subset,
                            OUT_OF_I32_RANGE,
                            token.offset,
                        ));
                    }
                    self.advance();
                    // The String of the Number the literal denotes -- which for
                    // an integer is its decimal digits, the same answer
                    // `__to_key` computes at run time for `o[1]`.
                    magnitude.to_string()
                }
                _ => {
                    let Some(name) = identifier_name(&token.kind) else {
                        return Err(
                            self.not_a_property_name("needs a property name in the object literal")
                        );
                    };
                    self.advance();
                    shorthand = matches!(token.kind, TokenKind::Ident(_));
                    name
                }
            };

            // `{ f() {} }`. Named before the `:` is looked for, because
            // otherwise the reader is told a `:` is missing from something
            // that never wanted one.
            if self.at(&TokenKind::LParen) {
                return Err(unsupported(
                    Boundary::FullJs,
                    "methods in object literals",
                    self.peek().offset,
                ));
            }
            // `{ get a() {} }` and `{ set a(v) {} }`: two property names in a
            // row is an accessor and nothing else.
            if (key == "get" || key == "set") && self.at_property_name() {
                return Err(unsupported(
                    Boundary::FullJs,
                    "getters and setters in object literals",
                    span.offset(),
                ));
            }
            // ECMA-262 B.3.1: `__proto__: v` in an object literal sets the
            // prototype instead of creating a property. This engine has no
            // prototypes, so reading it as an ordinary property would be
            // silently wrong rather than merely absent. The shorthand
            // `{ __proto__ }` is an ordinary property even in B.3.1, so it is
            // deliberately not refused here.
            if key == "__proto__" && self.at(&TokenKind::Colon) {
                return Err(unsupported(
                    Boundary::FullJs,
                    "the `__proto__` property",
                    span.offset(),
                ));
            }

            if self.eat(&TokenKind::Colon) {
                let value = self.expression(BP_ASSIGN)?;
                return Ok(Property { key, value, span });
            }
            // 13.2.5: the shorthand is an IdentifierReference, so only a plain
            // identifier may stand alone -- `{ if }` is not a property.
            if shorthand && (self.at(&TokenKind::Comma) || self.at(&TokenKind::RBrace)) {
                let occurrence = self.occurrence(&key, span.offset(), Role::Read);
                let value = Expr {
                    kind: ExprKind::Name(occurrence),
                    span,
                };
                return Ok(Property { key, value, span });
            }
            Err(self.cannot_use("needs a `:` after the property name"))
        }

        /// Is the cursor on something that could be a PropertyName? Used only
        /// to tell an accessor from a property called `get`.
        fn at_property_name(&self) -> bool {
            matches!(self.kind(), TokenKind::Str(_) | TokenKind::Int(_))
                || identifier_name(self.kind()).is_some()
        }

        /// The refusal for a token that cannot be read as a property name.
        ///
        /// A reserved word *is* an IdentifierName in ECMA-262, so `o.delete`
        /// and `{ class: 1 }` are legal JavaScript. Letting the generic
        /// diagnostic speak would answer "does not support the `delete`
        /// keyword", which is true of a construct that is not the one written.
        fn not_a_property_name(&self, what: &str) -> CompileError {
            let token = self.peek();
            if let TokenKind::Unsupported(u) = &token.kind
                && u.boundary == Boundary::FullJs
                && u.phrase.ends_with(" keyword")
            {
                return unsupported(
                    Boundary::FullJs,
                    "a property named with a reserved word",
                    token.offset,
                );
            }
            self.cannot_use(what)
        }

        // -- resolution ------------------------------------------------------

        /// Every recorded occurrence, now that every declaration is in.
        fn resolve(&mut self) -> Result<Vec<Res>, CompileError> {
            let resolved: Vec<Res> = self
                .pending
                .iter()
                .map(|p| self.resolve_one(p))
                .collect::<Result<_, _>>()?;
            self.record_captures(&resolved);
            Ok(resolved)
        }

        /// Turn every [`Res::Captured`] into an environment layout.
        ///
        /// Runs after resolution and not during it, because a capture is a
        /// fact about an occurrence *and* about every function between the
        /// occurrence and the binding's owner -- and the second half is only
        /// knowable once the first is settled. `resolve_one` stays pure; this
        /// is the one pass that writes back.
        ///
        /// **Flat closures.** A function three levels below the owner holds
        /// the cell directly, and so does every function between: each one
        /// captures the binding so it can hand the cell to the next. That
        /// costs one entry per level in the layout and one load per read, at
        /// any depth -- against a parent chain, which costs one load per level
        /// on every read, forever, to save a word once.
        ///
        /// The walk goes up *scopes*, not functions, because scopes are what
        /// carry the parent link; distinct `func` values along that chain are
        /// the function nesting.
        fn record_captures(&mut self, resolved: &[Res]) {
            for (p, res) in self.pending.iter().zip(resolved) {
                let Res::Captured(id) = res else { continue };
                let owner = self.bindings[id.0 as usize].func;
                self.bindings[id.0 as usize].captured = true;

                let mut scope = Some(p.scope);
                while let Some(at) = scope {
                    let func = self.scopes[at].func;
                    if func == owner {
                        break;
                    }
                    let captures = &mut self.functions[func.0 as usize].captures;
                    if !captures.contains(id) {
                        captures.push(*id);
                    }
                    scope = self.scopes[at].parent;
                }
            }
        }

        fn resolve_one(&self, p: &Pending) -> Result<Res, CompileError> {
            let mut scope = Some(p.scope);
            while let Some(id) = scope {
                if let Some(binding) = self.lookup(id, &p.name) {
                    return self.classify(p, binding);
                }
                scope = self.scopes[id].parent;
            }
            // The engine's own `JSON`, ECMA-262 25.5. Reached only after the
            // scope walk above found nothing, so a script's own declaration of
            // the name shadows it outright and there is nothing privileged to
            // lose to. An embedder that *declares* a host function of the same
            // name is being explicit and wins: a declaration table is a
            // deliberate act, where `Names::HostImport`'s "any free name is an
            // import" is a default, and a default must not quietly take a name
            // the engine implements.
            if p.name == JSON && !declared(&self.options.names, &p.name) {
                return match p.role {
                    Role::Write => Err(malformed(
                        &format!(
                            "cannot assign to `{}`, which is this engine's own binding; declare a binding of that name if the script wants one of its own",
                            p.name
                        ),
                        p.offset,
                    )),
                    _ => Ok(Res::Json),
                };
            }
            match &self.options.names {
                // Either host table resolves a free name the same way. Which
                // *shape* the call takes -- V1 pairs into `js.<name>`, or the
                // raw signature an embedder declared -- is the lowering's
                // question, not resolution's, and `emit` is where the
                // declaration is checked against the call.
                Names::HostImport | Names::Declared(_) if p.role != Role::Write => {
                    Ok(Res::Host(p.name.clone()))
                }
                // The host table is a table of imports, and an import is not
                // a place a value can be put.
                Names::HostImport | Names::Declared(_) => Err(unsupported(
                    Boundary::ThirdBinding,
                    "assigning to a host name",
                    p.offset,
                )),
                // "no global bindings" was true until `JSON` arrived, and
                // saying it now would be the engine disclaiming something it
                // has -- the same lie [`Parser::cannot_use`] was fixed for.
                // One name is not a scope, and the sentence says which.
                Names::Unbound => Err(malformed(
                    &format!(
                        "finds no declaration of `{}`; `{JSON}` is the only name this engine binds, so any other has to be declared in the source",
                        p.name
                    ),
                    p.offset,
                )),
            }
        }

        fn classify(&self, p: &Pending, id: BindingId) -> Result<Res, CompileError> {
            let binding = &self.bindings[id.0 as usize];
            // The temporal dead zone, in the one shape a compiler can settle
            // without running the program: the occurrence is in the very
            // function that declares the binding, and it is written before the
            // declarator that initialises it. An occurrence inside a *nested*
            // function is not decided here -- `function f() { return x; } let
            // x = 1; f();` is legal, because the call is what runs, not the
            // text -- so this deliberately catches only the definite cases and
            // leaves the rest reading `undefined` until there is a runtime
            // flag to test. Storage is a zeroed local or global and
            // `TAG_UNDEFINED` is 0, which is why the value would otherwise be
            // a fabricated `undefined` rather than an error.
            if let Some(at) = binding.initialised
                && binding.func == p.func
                && p.offset < at
            {
                return Err(malformed(
                    &format!(
                        "reads `{}` before the declaration at byte {} initialises it; a `let` or `const` binding holds no value until its declarator has run",
                        binding.name,
                        binding.span.offset()
                    ),
                    p.offset,
                ));
            }
            match (p.role, binding.kind) {
                // A name bound to a known function names that function and
                // nothing else -- it has no storage, it can never be
                // reassigned, and it is the same function from every depth. So
                // both a call and a read reach it out of any depth and neither
                // is a capture: the call is direct, and the read is the
                // constant function value.
                (Role::Call | Role::Read, BindingKind::Function(_)) => Ok(Res::Callee(id)),
                (Role::Write, BindingKind::Const) => Err(malformed(
                    &format!(
                        "cannot assign to `{}`, which is declared `const` at byte {}",
                        binding.name,
                        binding.span.offset()
                    ),
                    p.offset,
                )),
                (Role::Write, BindingKind::Function(_)) => Err(malformed(
                    &format!(
                        "cannot assign to `{}`, which is bound to a function at byte {}",
                        binding.name,
                        binding.span.offset()
                    ),
                    p.offset,
                )),
                _ if binding.func == p.func => Ok(Res::Local(id)),
                // The script's bindings outlive every frame, so a function may
                // read one. Any other enclosing function's would have to be
                // captured, and a captured binding needs an environment this
                // engine does not build.
                _ if binding.func == Program::SCRIPT => Ok(Res::Global(id)),
                // A binding of an enclosing function. This used to be
                // `unsupported("closures that capture a variable")`; it is a
                // capture now, and `record_captures` is the pass that turns
                // this answer into an environment layout.
                _ => Ok(Res::Captured(id)),
            }
        }
    }

    /// Whether M1 lowers nothing this token can spell.
    ///
    /// The complement of the list is long and the list itself is short, which
    /// is the point: M1 reads every keyword, every operator rung, blocks,
    /// object literals, property access and the conditional, so a refusal that
    /// names one of those as missing is false. What is left is genuinely ahead
    /// of the engine wherever it appears.
    ///
    /// [`TokenKind::Unsupported`] is not here because it never reaches this
    /// function's second arm -- it carries its own phrase and is answered
    /// first.
    fn unlowered_by_m1(kind: &TokenKind) -> bool {
        matches!(
            kind,
            // A `:` that neither an object literal nor a `?:` consumed is
            // a label.
            TokenKind::Colon
                // The comma operator. A `,` in an argument list, a parameter
                // list, a declarator list or an object literal is consumed
                // where it belongs and never reaches here.
                | TokenKind::Comma
        )
    }

    /// Whether the embedder's declaration table names this function.
    ///
    /// Only [`Names::Declared`] has a table; the other two modes never take a
    /// name away from the engine's own -- see [`Parser::resolve_one`].
    fn declared(names: &Names, name: &str) -> bool {
        match names {
            Names::Declared(decls) => decls.iter().any(|d| d.name == name),
            Names::HostImport | Names::Unbound => false,
        }
    }

    /// Apply the sign to a decimal magnitude. `negative` widens the `i32`
    /// range by exactly one, which is the whole reason the sign is folded here
    /// instead of lowered as an operator.
    ///
    /// # An integer past `i32` is a Number, not a refusal
    ///
    /// It used to be `unsupported(OUT_OF_I32_RANGE)`, and that stopped being
    /// defensible the moment the lexer learned fractions: `1e30` would have
    /// been a literal this engine reads while `2147483648` was not, which is
    /// an absurdity the same change created. There is one numeric type here
    /// and it is binary64, so every decimal integer literal denotes a double
    /// (ECMA-262 6.1.6.1) and this hands back the double.
    ///
    /// `ExprKind::Int` is kept for the ones that fit, and not merged into
    /// `Num`, because the two lower identically and the `i32` form is what a
    /// property key is written with -- `{ 1: v }` is the key `"1"`, spelled
    /// from the magnitude. Widening that path would mean spelling a key from a
    /// double, which is `Num`'s own refusal one arm below.
    ///
    /// Precision is ECMA-262's, not a promise this makes: past 2^53 a literal
    /// denotes the nearest double, so `9007199254740993` reads back as
    /// `9007199254740992` here exactly as it does in any engine.
    fn int_literal(
        magnitude: u64,
        negative: bool,
        _offset: usize,
    ) -> Result<ExprKind, CompileError> {
        let limit = if negative {
            i32::MIN.unsigned_abs() as u64
        } else {
            i32::MAX as u64
        };
        if magnitude > limit {
            let value = magnitude as f64;
            return Ok(ExprKind::Num(if negative { -value } else { value }));
        }
        let value = if negative {
            (magnitude as i64).wrapping_neg() as i32
        } else {
            magnitude as i32
        };
        Ok(ExprKind::Int(value))
    }

    /// The spelling of a token that may stand as an ECMA-262 IdentifierName:
    /// a plain identifier, or one of the keywords this engine spells.
    ///
    /// IdentifierName is not Identifier: it admits every reserved word, which
    /// is why `o.for` and `{ null: 1 }` are grammar. The words this lexer
    /// still refuses outright never reach here, and
    /// [`Parser::not_a_property_name`] is what names *them*.
    fn identifier_name(kind: &TokenKind) -> Option<String> {
        let word = match kind {
            TokenKind::Ident(name) => return Some(name.clone()),
            TokenKind::Function => "function",
            TokenKind::Return => "return",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::While => "while",
            TokenKind::For => "for",
            TokenKind::Let => "let",
            TokenKind::Const => "const",
            TokenKind::Var => "var",
            TokenKind::Typeof => "typeof",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Null => "null",
            TokenKind::Undefined => "undefined",
            TokenKind::Try => "try",
            TokenKind::Catch => "catch",
            TokenKind::Finally => "finally",
            TokenKind::Throw => "throw",
            _ => return None,
        };
        Some(word.to_string())
    }

    /// What an infix token builds.
    enum Infix {
        Binary(BinaryOp),
        Logical(LogicalOp),
        /// `=` and the compound forms; `None` is a plain `=`.
        Assign(Option<BinaryOp>),
    }

    /// The binding-power table: `(what it builds, left power, right power)`.
    ///
    /// `right = left + 1` is what makes a level left associative -- the loop
    /// in [`Parser::expression`] refuses to re-enter at the same power.
    /// Assignment is `right = left`, which is right associativity: not
    /// `left - 1`, which would also work here and would stop working the
    /// moment two right-associative levels sit next to each other.
    fn infix(kind: &TokenKind) -> Option<(Infix, u8, u8)> {
        use TokenKind as T;
        let (what, lbp) = match kind {
            T::Eq => (Infix::Assign(None), BP_ASSIGN),
            T::PlusEq => (Infix::Assign(Some(BinaryOp::Add)), BP_ASSIGN),
            T::MinusEq => (Infix::Assign(Some(BinaryOp::Sub)), BP_ASSIGN),
            T::StarEq => (Infix::Assign(Some(BinaryOp::Mul)), BP_ASSIGN),
            T::SlashEq => (Infix::Assign(Some(BinaryOp::Div)), BP_ASSIGN),
            T::PercentEq => (Infix::Assign(Some(BinaryOp::Rem)), BP_ASSIGN),
            T::PipePipe => (Infix::Logical(LogicalOp::Or), BP_OR),
            T::AmpAmp => (Infix::Logical(LogicalOp::And), BP_AND),
            T::EqEq => (Infix::Binary(BinaryOp::Eq), BP_EQUALITY),
            T::BangEq => (Infix::Binary(BinaryOp::Ne), BP_EQUALITY),
            T::EqEqEq => (Infix::Binary(BinaryOp::StrictEq), BP_EQUALITY),
            T::BangEqEq => (Infix::Binary(BinaryOp::StrictNe), BP_EQUALITY),
            T::Lt => (Infix::Binary(BinaryOp::Lt), BP_RELATIONAL),
            T::LtEq => (Infix::Binary(BinaryOp::Le), BP_RELATIONAL),
            T::Gt => (Infix::Binary(BinaryOp::Gt), BP_RELATIONAL),
            T::GtEq => (Infix::Binary(BinaryOp::Ge), BP_RELATIONAL),
            T::Plus => (Infix::Binary(BinaryOp::Add), BP_ADDITIVE),
            T::Minus => (Infix::Binary(BinaryOp::Sub), BP_ADDITIVE),
            T::Star => (Infix::Binary(BinaryOp::Mul), BP_MULTIPLICATIVE),
            T::Slash => (Infix::Binary(BinaryOp::Div), BP_MULTIPLICATIVE),
            T::Percent => (Infix::Binary(BinaryOp::Rem), BP_MULTIPLICATIVE),
            _ => return None,
        };
        let rbp = match what {
            Infix::Assign(_) => lbp,
            _ => lbp + 1,
        };
        Some((what, lbp, rbp))
    }

    // -- writing the answers back --------------------------------------------
    //
    // Resolution produced one answer per occurrence, in source order. The tree
    // stores each occurrence's index, so this is a walk with no order to get
    // right -- which is why function bodies can live in a flat list.

    fn fill_stmts(stmts: &mut [Stmt], res: &[Res]) {
        for stmt in stmts {
            match &mut stmt.kind {
                StmtKind::Empty | StmtKind::Func { .. } => {}
                StmtKind::Expr(e) => fill_expr(e, res),
                StmtKind::Decl(decls) => {
                    for decl in decls {
                        if let Some(init) = &mut decl.init {
                            fill_expr(init, res);
                        }
                    }
                }
                StmtKind::Block(body) => fill_stmts(body, res),
                StmtKind::If { test, then, alt } => {
                    fill_expr(test, res);
                    fill_stmts(std::slice::from_mut(then), res);
                    if let Some(alt) = alt {
                        fill_stmts(std::slice::from_mut(alt), res);
                    }
                }
                StmtKind::While { test, body } => {
                    fill_expr(test, res);
                    fill_stmts(std::slice::from_mut(body), res);
                }
                StmtKind::For {
                    init,
                    test,
                    update,
                    body,
                } => {
                    if let Some(init) = init {
                        fill_stmts(std::slice::from_mut(init), res);
                    }
                    if let Some(test) = test {
                        fill_expr(test, res);
                    }
                    if let Some(update) = update {
                        fill_expr(update, res);
                    }
                    fill_stmts(std::slice::from_mut(body), res);
                }
                StmtKind::Return(value) => {
                    if let Some(value) = value {
                        fill_expr(value, res);
                    }
                }
                StmtKind::Throw(value) => fill_expr(value, res),
                StmtKind::Try {
                    block,
                    handler,
                    finalizer,
                } => {
                    fill_stmts(block, res);
                    if let Some(catch) = handler {
                        fill_stmts(&mut catch.body, res);
                    }
                    if let Some(finalizer) = finalizer {
                        fill_stmts(finalizer, res);
                    }
                }
            }
        }
    }

    fn fill_expr(expr: &mut Expr, res: &[Res]) {
        match &mut expr.kind {
            ExprKind::Int(_)
            | ExprKind::Num(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Undefined
            | ExprKind::Arg(_)
            | ExprKind::Function(_) => {}
            ExprKind::Name(name) => name.res = res[name.occurrence as usize].clone(),
            ExprKind::Object(properties) => {
                for property in properties {
                    fill_expr(&mut property.value, res);
                }
            }
            ExprKind::Array(elements) => {
                for element in elements {
                    fill_expr(element, res);
                }
            }
            ExprKind::Member { object, key } => {
                fill_expr(object, res);
                if let MemberKey::Computed(key) = key {
                    fill_expr(key, res);
                }
            }
            ExprKind::Call { callee, args } => {
                fill_expr(callee, res);
                for arg in args {
                    fill_expr(arg, res);
                }
            }
            ExprKind::Conditional { test, then, alt } => {
                fill_expr(test, res);
                fill_expr(then, res);
                fill_expr(alt, res);
            }
            ExprKind::Unary(_, operand) => fill_expr(operand, res),
            ExprKind::Update { target, .. } => fill_expr(target, res),
            ExprKind::Binary(_, lhs, rhs) | ExprKind::Logical(_, lhs, rhs) => {
                fill_expr(lhs, res);
                fill_expr(rhs, res);
            }
            ExprKind::Assign { target, value, .. } => {
                fill_expr(target, res);
                fill_expr(value, res);
            }
        }
    }
}
