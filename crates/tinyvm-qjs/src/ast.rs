//! The syntax tree the parser produces and the lowering consumes.
//!
//! Deliberately a tree and not a byte stream. A single-pass parser that emitted
//! wasm as it went would be shorter today and unusable at M1: constant folding,
//! scope resolution, and control flow all need to look at a node more than
//! once. The tree is the seam that lets those passes exist.
//!
//! M0 nodes carry no source spans. Every diagnostic this milestone can produce
//! is raised during lexing or parsing, where the token offset is in hand; the
//! first lowering-time diagnostic (M1, an unresolved name) is what should add
//! them, rather than carrying unread fields until then.

/// A whole compilation unit. M0 accepts exactly one expression; the struct
/// exists so M1 can grow a statement list without changing the pipeline shape.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Program {
    pub(crate) expr: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    /// A signed 32-bit integer. The parser has already applied any leading
    /// minus and checked the range, so lowering cannot see an unrepresentable
    /// literal.
    Int(i32),
    /// `$N` -- the Nth argument of this call.
    Arg(u32),
    /// A bare name resolved against the host import table: `g` and `g()` both
    /// mean "call the zero-argument import `js.g`". Only produced under
    /// [`crate::Names::HostImport`]; the language itself has no bindings yet.
    Host(String),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// The M1 tree: statements, functions, and names already resolved to bindings.
///
/// Nested rather than replacing the items above because [`super::emit`] still
/// consumes the M0 tree and lives in another lane. Integration is one move:
/// delete the M0 items, un-nest this module.
///
/// Three things distinguish it from an ordinary hand-rolled JavaScript AST, and
/// each is load-bearing:
///
/// * every node carries a [`m1::Span`], because M1 is the first milestone whose
///   diagnostics are raised *after* parsing (an unresolved name), where no
///   token offset is in hand;
/// * `&&`/`||` are [`m1::ExprKind::Logical`] and not [`m1::ExprKind::Binary`],
///   because
///   they are control flow -- see that variant;
/// * a name is a [`m1::Res`], not a string. wasm locals are indexed, so the
///   name-to-index question has to be answered somewhere; it is answered here,
///   once, by the parser's resolution pass, and lowering never sees a name it
///   has to look up.
///
/// The whole compilation unit is [`m1::Program::SCRIPT`], an ordinary
/// [`m1::Function`]
/// with no name. That is not a convenience: `$N` already means "this call's
/// arguments", so the unit *is* a function body, and `return` in it returns.
#[allow(
    dead_code,
    reason = "the M1 tree is complete before the lowering lane consumes it"
)]
pub(crate) mod m1 {
    /// Where a node starts, in bytes from the start of the source.
    ///
    /// One offset and not a range: [`crate::diag::CompileError`] carries one
    /// offset, and a [`crate::lex::Token`] carries no length, so an end would
    /// have to be invented rather than measured. A newtype anyway, so that the
    /// day a token knows its length `end` is a field here instead of a second
    /// argument at every call site.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Span(pub(crate) usize);

    impl Span {
        pub(crate) fn offset(self) -> usize {
            self.0
        }
    }

    /// Index into [`Program::functions`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub(crate) struct FuncId(pub(crate) u32);

    /// Index into [`Program::bindings`]: one declared name, program-wide.
    ///
    /// Flat rather than a `(function, slot)` pair so that a [`Res`] is one
    /// number and the two views a consumer wants -- "which binding is this"
    /// and "where does it live" -- do not have to agree by construction.
    /// [`Binding::func`] and [`Binding::slot`] are the second view.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub(crate) struct BindingId(pub(crate) u32);

    /// A whole compilation unit: every function it defines and every name it
    /// binds, with the statements hanging off [`Program::SCRIPT`].
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Program {
        pub(crate) functions: Vec<Function>,
        pub(crate) bindings: Vec<Binding>,
        /// How many `$N` the script names: one past the highest index used, so
        /// `$2` alone still means three and a source naming none means zero.
        pub(crate) arg_count: u32,
    }

    impl Program {
        /// The compilation unit itself. Always the first function, so a
        /// consumer can tell "module storage" from "frame slot" by comparing
        /// [`Binding::func`] against it.
        pub(crate) const SCRIPT: FuncId = FuncId(0);

        pub(crate) fn script(&self) -> &Function {
            self.func(Self::SCRIPT)
        }

        pub(crate) fn func(&self, id: FuncId) -> &Function {
            &self.functions[id.0 as usize]
        }

        pub(crate) fn binding(&self, id: BindingId) -> &Binding {
            &self.bindings[id.0 as usize]
        }
    }

    /// One function: the script, a declaration, or a function expression.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Function {
        /// The declared name, or `None` for the script and for an anonymous
        /// function expression.
        pub(crate) name: Option<String>,
        /// Parameters in order. Also the first entries of `bindings`.
        pub(crate) params: Vec<BindingId>,
        /// Every binding whose storage this function owns, parameters first
        /// and then in declaration order across every block. Block structure
        /// is deliberately gone by here: a wasm local is function-scoped, and
        /// two `let x` in sibling blocks are already two distinct bindings.
        pub(crate) bindings: Vec<BindingId>,
        pub(crate) body: Vec<Stmt>,
        pub(crate) span: Span,
        /// Bindings of an *enclosing* function that this one reads, in the
        /// order they became its environment.
        ///
        /// Empty for every function that captures nothing, which is every
        /// function in a program with no closure -- so the environment
        /// machinery is absent from such a program rather than present and
        /// unused. The order is this function's environment layout: the
        /// creator fills a cell vector in exactly this order, and a read
        /// inside resolves to that index.
        ///
        /// **Flat, not a chain.** A function three levels down that reads a
        /// binding from the top holds that cell directly rather than walking
        /// two parents at run time. Its creator has the cell -- in its own
        /// locals if it declared it, in its own environment if it captured it
        /// too -- so building the vector costs one copy per entry and reading
        /// one costs one load, at any depth.
        pub(crate) captures: Vec<BindingId>,
    }

    /// One declared name.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Binding {
        pub(crate) name: String,
        pub(crate) kind: BindingKind,
        /// Where the declaration is, for a diagnostic that has to point back
        /// at it ("...already bound at byte N").
        pub(crate) span: Span,
        /// Where this binding stops being in its temporal dead zone, in bytes
        /// from the start of the source, or `None` for a binding that already
        /// holds a value when its scope is entered.
        ///
        /// ECMA-262 8.2.4: a `let` or `const` exists from the top of its scope
        /// but is *uninitialised* until its declarator finishes, and reading it
        /// before then is a ReferenceError. A `var`, a parameter and a hoisted
        /// `function f(){}` are initialised on entry -- to `undefined`, to the
        /// argument, to the function -- so they have no dead zone and this is
        /// `None`. See `parse::m1::Parser::classify`, which is where the
        /// comparison is made.
        pub(crate) initialised: Option<usize>,
        /// The function that owns the storage.
        pub(crate) func: FuncId,
        /// Position in that function's [`Function::bindings`]. What a wasm
        /// local index is computed *from*; not the index itself, because how
        /// many wasm locals one binding costs is the representation's business
        /// and not the front end's.
        pub(crate) slot: u32,
        /// Whether some nested function reads this binding.
        ///
        /// A captured binding cannot live in a wasm local: the frame dies and
        /// the closure outlives it. It moves to a one-word heap cell, and the
        /// declaring function reads and writes *through that cell too* -- the
        /// classic bug is the declaring function keeping its local and the two
        /// diverging on the first assignment. ECMA-262 closes over the
        /// binding, not its value, so `let a = 1; …; a = 2; …` must be visible
        /// through the closure.
        ///
        /// Only captured bindings are boxed. Boxing every local would charge a
        /// closure-free script an allocation per local per call, which is the
        /// cost `plan/design-closure-milestone.md` §1.2 forbids.
        pub(crate) captured: bool,
        /// Whether this declaration sits inside a loop body, so it can run
        /// more than once.
        ///
        /// ECMA-262 14.3.1 makes each execution of a lexical declaration
        /// create a **new** binding, and that is only observable when the
        /// declaration executes twice -- which needs a loop. A declaration
        /// that runs once has one binding whatever the storage, so this is the
        /// flag that says whether changing the storage buys anything.
        ///
        /// Only the script consults it. A captured binding inside a function
        /// is a heap cell already, so allocating it per declaration is free
        /// and needs no test. A script binding is two module globals, and
        /// turning one into a cell costs the whole closure apparatus at every
        /// site that reads it -- 99 bytes, measured in `closures_m3.rs` -- so
        /// that conversion has to be asked for rather than applied to every
        /// script binding a nested function happens to read.
        pub(crate) in_loop: bool,
    }

    /// How a name was declared. The distinction outlives scoping: `Const` says
    /// the binding can never be written, and `Function` says its value is one
    /// statically known function -- which is what lets a call to it be a
    /// direct `call` rather than a `call_indirect` through the table, and what
    /// lets a *read* of it be a constant rather than storage.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum BindingKind {
        Param,
        Var,
        Let,
        Const,
        /// A `function f(){}` declaration, or a `const f = function(){}`. Both
        /// are a name that holds one known function and can never be
        /// reassigned. Everything else is callable too, now that a function is
        /// a value -- this kind is what makes the *direct* call possible, and
        /// what makes the binding cost no storage.
        Function(FuncId),
    }

    /// What a name occurrence turned out to mean.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum Res {
        /// The parser has not run its resolution pass over this occurrence
        /// yet. Never present in a [`Program`] the parser returned.
        Unresolved,
        /// A binding of the very function this occurrence appears in.
        Local(BindingId),
        /// A binding of the script, read from inside a function. Its storage
        /// has to outlive a frame; which module-level storage that is, is the
        /// lowering's choice.
        Global(BindingId),
        /// A binding of an **enclosing function**, read from inside a nested
        /// one: a capture.
        ///
        /// Distinct from [`Local`](Self::Local) because the storage is
        /// different -- a captured binding is boxed into a heap cell that
        /// outlives the frame -- and distinct from [`Global`](Self::Global)
        /// because a cell is reached through this function's environment
        /// rather than at a module-level address. The index into that
        /// environment is [`Function::captures`]'s position, which the
        /// lowering looks up rather than the parser storing twice.
        Captured(BindingId),
        /// A name bound to a known function, whether it is being called or
        /// read. It names the function rather than storage, which is why it
        /// resolves from any depth and captures nothing either way: a call to
        /// it is direct, and a read of it is the constant function value.
        /// Always a binding whose kind is [`BindingKind::Function`].
        Callee(BindingId),
        /// A free name, resolved against the host import table. Only produced
        /// under [`crate::Names::HostImport`].
        Host(String),
        /// The one name this engine binds itself: [`JSON`].
        ///
        /// Not a global scope, and the distinction is the whole reason this is
        /// a `Res` variant rather than a binding the parser injects. There is
        /// no environment record here, nothing enumerates it, and a
        /// declaration of the same name in the source shadows it outright --
        /// the scope walk runs first and this arm is only reached when the
        /// walk found nothing. What it *is* is a name the lowering knows how
        /// to build a value for, which is exactly what
        /// `JSON.parse(text)` needs and nothing more.
        Json,
    }

    /// The name [`Res::Json`] answers to.
    ///
    /// One name, spelled once. A second intrinsic would make this a table and
    /// that is the point at which "no global scope" stops being true; until
    /// then the singular is the honest shape.
    pub(crate) const JSON: &str = "JSON";

    /// One occurrence of a name.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Name {
        pub(crate) text: String,
        pub(crate) res: Res,
        /// Which occurrence this is, counting from the start of the source.
        ///
        /// The parser records every occurrence in one list and resolves that
        /// list only after the whole program is read -- hoisting means a name
        /// can bind to a declaration nobody has parsed yet -- then writes the
        /// answers back through this index. Of no use downstream.
        pub(crate) occurrence: u32,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Stmt {
        pub(crate) kind: StmtKind,
        pub(crate) span: Span,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum StmtKind {
        /// A lone `;`. Kept rather than dropped so a statement list's length
        /// is the source's, which is what a span-based diagnostic assumes.
        Empty,
        Expr(Expr),
        /// `let`/`const`/`var`, one [`Declarator`] per name. Which of the three
        /// it was is on the [`Binding`], where the question is asked from.
        Decl(Vec<Declarator>),
        Block(Vec<Stmt>),
        If {
            test: Expr,
            then: Box<Stmt>,
            alt: Option<Box<Stmt>>,
        },
        While {
            test: Expr,
            body: Box<Stmt>,
        },
        /// A three-part `for`. `init` is a [`StmtKind::Decl`] or a
        /// [`StmtKind::Expr`] -- reusing `Stmt` rather than a second enum,
        /// because those are exactly the two shapes and both already exist.
        For {
            init: Option<Box<Stmt>>,
            test: Option<Expr>,
            update: Option<Expr>,
            body: Box<Stmt>,
        },
        Return(Option<Expr>),
        /// `throw e`, ECMA-262 14.14. A statement and not an expression: its
        /// completion is abrupt, so there is no value for it to be.
        Throw(Expr),
        /// `try`/`catch`/`finally`, ECMA-262 14.15.
        ///
        /// `handler` and `finalizer` are both optional and 14.15 requires at
        /// least one of them; the parser refuses a `try` with neither, so
        /// this is never `(None, None)` in a tree it returned. Each of the
        /// three parts is a statement *list* rather than a `Stmt`, because
        /// the grammar spells all three `Block` -- there is no
        /// `try if (c) x;` -- and a list is what
        /// `emit::m1::Lower::stmts` needs in order to instantiate the
        /// function declarations directly inside it.
        Try {
            block: Vec<Stmt>,
            handler: Option<Catch>,
            finalizer: Option<Vec<Stmt>>,
        },
        /// A hoisted `function f(){}`. It appears in the list where it was
        /// written, but its binding is in scope from the top of the enclosing
        /// scope, so nothing has to *run* here.
        Func {
            binding: BindingId,
            func: FuncId,
        },
    }

    /// One `catch` clause.
    ///
    /// `param` is an `Option` because ECMA-262 14.15 makes the CatchParameter
    /// optional (`catch { }`), not because the parser might fail to read one.
    /// A present one is an ordinary binding of the catch block's own scope,
    /// which is what makes it shadow an outer name of the same spelling and
    /// makes it writable.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Catch {
        pub(crate) param: Option<BindingId>,
        pub(crate) body: Vec<Stmt>,
    }

    /// One `key: value` of an ObjectLiteral.
    ///
    /// The key is a `String` and not an [`Expr`], because in ECMA-262 13.2.5 a
    /// PropertyName is not an expression: `{ a: 1 }`, `{ "a": 1 }` and
    /// `{ 1: x }` all name a String at parse time. The one form that *is* an
    /// expression -- a ComputedPropertyName, `{ [k]: 1 }` -- is refused with a
    /// capability diagnostic rather than being half-represented here.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Property {
        pub(crate) key: String,
        pub(crate) value: Expr,
        pub(crate) span: Span,
    }

    /// How a MemberExpression names its property.
    ///
    /// Two variants and not one `Expr`, because ECMA-262 gives them two
    /// algorithms: 13.3.2.1 EvaluatePropertyAccessWithIdentifierKey takes the
    /// *String value of the IdentifierName* -- there is no expression to
    /// evaluate and no ToPropertyKey to run -- while 13.3.3.1
    /// EvaluatePropertyAccessWithExpressionKey evaluates, GetValues and then
    /// ToPropertyKeys.
    ///
    /// Collapsing them into one `Expr` and then recognising a string literal
    /// in the lowering would be the same thing spelled as a static-type
    /// exemption at a call site -- the shape `RESULTS.md` L2.5 records as the
    /// disease. Spelled as two productions it is simply the grammar.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum MemberKey {
        /// `o.a`, and the String it names.
        Static(String),
        /// `o[e]`.
        Computed(Box<Expr>),
    }

    /// One name of a declaration, with its initialiser if it has one.
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Declarator {
        pub(crate) binding: BindingId,
        pub(crate) init: Option<Expr>,
        pub(crate) span: Span,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Expr {
        pub(crate) kind: ExprKind,
        pub(crate) span: Span,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum ExprKind {
        /// An integer literal. Still `i32` at M1: the parser has applied any
        /// leading minus and checked the range, so lowering cannot meet an
        /// unrepresentable one.
        Int(i32),
        /// A numeric literal with a fraction or an exponent, as the binary64
        /// value it denotes.
        ///
        /// A second variant rather than widening [`Int`](Self::Int) to `f64`,
        /// because `Int` is also what a property key is written with and the
        /// two are different questions -- see `lex::TokenKind::Num`. Lowering
        /// them is the same instruction either way: this engine has one
        /// numeric type.
        Num(f64),
        /// A string literal, escapes already resolved by the lexer.
        Str(String),
        Bool(bool),
        Null,
        Undefined,
        /// `$N` -- the Nth argument of this call. A parameter of the script,
        /// which is why it may not appear inside a nested function.
        Arg(u32),
        Name(Name),
        /// A function expression. In a call's callee position it is an
        /// immediately-invoked direct call; anywhere else it is a function
        /// *value*, which is a table element index in a V1 pair.
        Function(FuncId),
        /// An ObjectLiteral, ECMA-262 13.2.5. The properties are in source
        /// order, duplicates included: 13.2.5.5 evaluates each in turn and
        /// each is a CreateDataPropertyOrThrow, so `{ a: 1, a: 2 }` is one
        /// property written twice and not two properties.
        Object(Vec<Property>),
        /// An ArrayLiteral, ECMA-262 13.2.4. The elements are in source order.
        ///
        /// Elisions -- the hole in `[1, , 3]` -- are refused at the parser
        /// rather than represented, because this engine has no way to tell a
        /// hole from an `undefined` and would have to pick one silently. There
        /// is therefore no `Option` here: every element is an expression that
        /// was written.
        Array(Vec<Expr>),
        /// A MemberExpression, ECMA-262 13.3.2 and 13.3.3.
        Member {
            object: Box<Expr>,
            key: MemberKey,
        },
        Call {
            callee: Box<Expr>,
            args: Vec<Expr>,
        },
        Unary(UnaryOp, Box<Expr>),
        /// `++`/`--`, written before or after its target. The distinction is
        /// the *value* of the expression, not the effect, so it cannot be
        /// desugared away here.
        Update {
            op: UpdateOp,
            prefix: bool,
            target: Box<Expr>,
        },
        Binary(BinaryOp, Box<Expr>, Box<Expr>),
        /// `test ? then : alt`, ECMA-262 13.14.
        ///
        /// A node of its own and not a `Logical` with three operands, for the
        /// reason `Logical` is not a `Binary`: only one of the two branches is
        /// evaluated, and a post-order walk over a generic n-ary node would
        /// evaluate both. It shares that property with `Logical` and differs
        /// in one way that matters to the lowering -- its value is one of two
        /// *branches* rather than one of two *operands*, so there is nothing
        /// to reuse between them.
        Conditional {
            test: Box<Expr>,
            then: Box<Expr>,
            alt: Box<Expr>,
        },
        /// `&&` and `||`.
        ///
        /// A separate node from [`ExprKind::Binary`] on purpose, and this is
        /// the one AST decision that is a semantic requirement rather than a
        /// taste. Every `Binary` evaluates both operands and then applies an
        /// operator; these two evaluate the right operand only if the left
        /// says so, and their value is one of the *operands*, not a boolean.
        /// Folding them into `Binary` is precisely how an engine loses
        /// short-circuiting, because the lowering that walks `Binary` is
        /// post-order by construction.
        Logical(LogicalOp, Box<Expr>, Box<Expr>),
        /// `=` and the compound forms. `op` is `None` for a plain `=`;
        /// `Some(Add)` is `+=`, which reads the target, applies the operator,
        /// and writes back.
        Assign {
            op: Option<BinaryOp>,
            target: Box<Expr>,
            value: Box<Expr>,
        },
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum UnaryOp {
        Neg,
        /// Unary `+`: ToNumber, not a no-op.
        Plus,
        Not,
        /// `typeof`: the ECMA-262 13.5.3 name of the operand's language type,
        /// as a String. The one unary operator whose result is not a Number
        /// or a Boolean.
        TypeOf,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum UpdateOp {
        Inc,
        Dec,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum BinaryOp {
        Add,
        Sub,
        Mul,
        Div,
        Rem,
        Lt,
        Le,
        Gt,
        Ge,
        /// `==`, which converts. [`BinaryOp::StrictEq`] is `===`, which does
        /// not; they are different operators and not a flag on one.
        Eq,
        Ne,
        StrictEq,
        StrictNe,
    }

    impl BinaryOp {
        /// How the operator is written. For diagnostics and for tests that
        /// assert a tree *shape*, which is the readable way to state a
        /// precedence claim.
        pub(crate) fn symbol(self) -> &'static str {
            match self {
                Self::Add => "+",
                Self::Sub => "-",
                Self::Mul => "*",
                Self::Div => "/",
                Self::Rem => "%",
                Self::Lt => "<",
                Self::Le => "<=",
                Self::Gt => ">",
                Self::Ge => ">=",
                Self::Eq => "==",
                Self::Ne => "!=",
                Self::StrictEq => "===",
                Self::StrictNe => "!==",
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum LogicalOp {
        And,
        Or,
    }
}
