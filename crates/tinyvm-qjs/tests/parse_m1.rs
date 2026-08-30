//! The M1 front end: statements, declarations and their scoping, functions,
//! the whole precedence ladder, and the resolution pass that turns names into
//! bindings.
//!
//! `parse` is a private module of the crate, so this file compiles the four
//! modules it needs a second time rather than reaching through the public API.
//! That is the same reason `tests/lex_m1.rs` gives: every assertion here is
//! about the *tree*, and routing them through `compile_qjs` could only observe
//! whatever the lowering currently does with that tree -- which is the thing
//! that changes underneath a front-end milestone. `src/emit.rs` still consumes
//! the M0 tree and lives in another lane, so the M1 tree is `ast::m1` and the
//! M1 parser is `parse::m1` until that lane catches up.

#![allow(dead_code)]

#[path = "../src/ast.rs"]
mod ast;
#[path = "../src/diag.rs"]
mod diag;
#[path = "../src/lex.rs"]
mod lex;
/// `src/parse.rs` reads `Names` and `Options` from `crate::opts`. Under
/// `#[path]` the crate root is this file, so the real module is pulled in here
/// too -- one file, rather than the hand-copied twin this shim used to be.
#[path = "../src/opts.rs"]
mod opts;
#[path = "../src/parse.rs"]
mod parse;

use ast::m1::{
    BindingKind, Expr, ExprKind, LogicalOp, MemberKey, Program, Res, Stmt, StmtKind, UnaryOp,
    UpdateOp,
};
use diag::CompileError;

use opts::{Names, Options};

// -- harness -----------------------------------------------------------------

fn compile(source: &str, names: Names) -> Result<Program, CompileError> {
    let tokens = lex::tokenize(source)?;
    parse::m1::parse(tokens, Options { names })
}

/// Parse with no host table: a name must be declared in the source.
fn program(source: &str) -> Program {
    compile(source, Names::Unbound).unwrap_or_else(|e| panic!("parsing {source:?}: {e}"))
}

/// Parse with the host import table, where a free name is a `js.<name>` import.
fn hosted(source: &str) -> Program {
    compile(source, Names::HostImport).unwrap_or_else(|e| panic!("parsing {source:?}: {e}"))
}

fn err(source: &str) -> CompileError {
    match compile(source, Names::Unbound) {
        Ok(_) => panic!("{source:?} parsed; expected a refusal"),
        Err(e) => e,
    }
}

fn refuse(source: &str) -> String {
    err(source).message
}

fn body(program: &Program) -> &[Stmt] {
    &program.script().body
}

fn only_expr(program: &Program) -> &Expr {
    match body(program) {
        [
            Stmt {
                kind: StmtKind::Expr(e),
                ..
            },
        ] => e,
        other => panic!("expected one expression statement, got {other:?}"),
    }
}

/// The tree as an s-expression. Precedence and associativity are claims about
/// *shape*, and a shape is far easier to read as text than as nested `matches!`.
fn sexpr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Int(n) => n.to_string(),
        // Rust's `{}` for `f64` is shortest-round-tripping, which is the same
        // property ECMA-262 6.1.6.1.20 asks for -- close enough for a shape
        // test, and this file is about shape.
        ExprKind::Num(x) => format!("{x}"),
        ExprKind::Str(s) => format!("{s:?}"),
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Null => "null".to_string(),
        ExprKind::Undefined => "undefined".to_string(),
        ExprKind::Arg(n) => format!("${n}"),
        ExprKind::Name(name) => name.text.clone(),
        ExprKind::Function(id) => format!("fn{}", id.0),
        ExprKind::Object(properties) => {
            let mut out = "(object".to_string();
            for property in properties {
                out.push_str(&format!(" {:?}:{}", property.key, sexpr(&property.value)));
            }
            out + ")"
        }
        ExprKind::Array(elements) => {
            let mut out = "(array".to_string();
            for element in elements {
                out.push(' ');
                out.push_str(&sexpr(element));
            }
            out + ")"
        }
        // The two spellings print differently on purpose: they are two
        // ECMA-262 productions (13.3.2 and 13.3.3), and a shape test that
        // could not tell them apart could not state that.
        ExprKind::Member { object, key } => match key {
            MemberKey::Static(name) => format!("(. {} {name})", sexpr(object)),
            MemberKey::Computed(key) => format!("(index {} {})", sexpr(object), sexpr(key)),
        },
        ExprKind::Call { callee, args } => {
            let mut out = format!("(call {}", sexpr(callee));
            for arg in args {
                out.push(' ');
                out.push_str(&sexpr(arg));
            }
            out + ")"
        }
        ExprKind::Unary(op, operand) => {
            let op = match op {
                UnaryOp::Neg => "-",
                UnaryOp::Plus => "+",
                UnaryOp::Not => "!",
                UnaryOp::BitNot => "~",
                UnaryOp::TypeOf => "typeof",
            };
            format!("({op} {})", sexpr(operand))
        }
        ExprKind::Update { op, prefix, target } => {
            let op = match op {
                UpdateOp::Inc => "++",
                UpdateOp::Dec => "--",
            };
            let fix = if *prefix { "pre" } else { "post" };
            format!("({fix}{op} {})", sexpr(target))
        }
        ExprKind::Conditional { test, then, alt } => {
            format!("(?: {} {} {})", sexpr(test), sexpr(then), sexpr(alt))
        }
        ExprKind::Binary(op, l, r) => format!("({} {} {})", op.symbol(), sexpr(l), sexpr(r)),
        ExprKind::Logical(op, l, r) => {
            let op = match op {
                LogicalOp::And => "&&",
                LogicalOp::Or => "||",
            };
            format!("({op} {} {})", sexpr(l), sexpr(r))
        }
        ExprKind::Assign { op, target, value } => {
            let op = match op {
                None => "=".to_string(),
                Some(op) => format!("{}=", op.symbol()),
            };
            format!("({op} {} {})", sexpr(target), sexpr(value))
        }
    }
}

/// The shape of one expression, with `prelude` in front of it to declare any
/// names it uses.
fn shape_after(prelude: &str, source: &str) -> String {
    let text = format!("{prelude}{source};");
    let program = program(&text);
    match body(&program).last() {
        Some(Stmt {
            kind: StmtKind::Expr(e),
            ..
        }) => sexpr(e),
        other => panic!("expected {text:?} to end in an expression statement, got {other:?}"),
    }
}

fn shape(source: &str) -> String {
    shape_after("", source)
}

/// What the one name in this expression resolved to.
fn resolution(e: &Expr) -> &Res {
    match &e.kind {
        ExprKind::Name(name) => &name.res,
        other => panic!("expected a name, got {other:?}"),
    }
}

// -- statements --------------------------------------------------------------

#[test]
fn a_script_is_a_list_of_statements() {
    let p = program("1; 2; 3;");
    assert_eq!(body(&p).len(), 3);
    assert_eq!(sexpr(&shape_expr(&p, 2)), "3");
}

fn shape_expr(p: &Program, index: usize) -> Expr {
    match &body(p)[index].kind {
        StmtKind::Expr(e) => e.clone(),
        other => panic!("statement {index} is {other:?}, not an expression"),
    }
}

#[test]
fn a_semicolon_may_be_left_out_at_a_line_break() {
    // ASI rules 1 and 2 -- the half the lexer cannot decide alone.
    let p = program("var x = 1\nvar y = 2\nx + y");
    assert_eq!(body(&p).len(), 3);
}

#[test]
fn an_empty_statement_is_a_statement() {
    let p = program(";;1;");
    assert_eq!(body(&p).len(), 3);
    assert!(matches!(body(&p)[0].kind, StmtKind::Empty));
}

#[test]
fn a_block_holds_its_own_statement_list() {
    let p = program("{ 1; 2; }");
    match &body(&p)[0].kind {
        StmtKind::Block(inner) => assert_eq!(inner.len(), 2),
        other => panic!("expected a block, got {other:?}"),
    }
}

#[test]
fn if_else_while_and_for_are_statements() {
    let p = program("if (1) { 2; } else { 3; }");
    match &body(&p)[0].kind {
        StmtKind::If { alt: Some(_), .. } => {}
        other => panic!("expected an if/else, got {other:?}"),
    }
    let p = program("while (1) { 2; }");
    assert!(matches!(body(&p)[0].kind, StmtKind::While { .. }));
    let p = program("for (let i = 0; i < 3; i++) { i; }");
    match &body(&p)[0].kind {
        StmtKind::For {
            init: Some(init),
            test: Some(_),
            update: Some(_),
            ..
        } => assert!(matches!(init.kind, StmtKind::Decl(_))),
        other => panic!("expected a three-part for, got {other:?}"),
    }
    // Every part of a `for` header is optional.
    let p = program("for (;;) { 1; }");
    assert!(matches!(
        body(&p)[0].kind,
        StmtKind::For {
            init: None,
            test: None,
            update: None,
            ..
        }
    ));
}

#[test]
fn a_dangling_else_binds_to_the_nearest_if() {
    let p = program("if (1) if (0) 2; else 3;");
    match &body(&p)[0].kind {
        StmtKind::If {
            alt: None,
            then: inner,
            ..
        } => assert!(matches!(inner.kind, StmtKind::If { alt: Some(_), .. })),
        other => panic!("the outer if took the else: {other:?}"),
    }
}

#[test]
fn return_is_allowed_at_the_top_level() {
    // The compilation unit *is* a function body -- it becomes `main`, and `$N`
    // is already "this call's arguments". So `return` there is the explicit
    // form of what falling off the end does implicitly.
    let p = program("return 1;");
    assert!(matches!(body(&p)[0].kind, StmtKind::Return(Some(_))));
    let p = program("return;");
    assert!(matches!(body(&p)[0].kind, StmtKind::Return(None)));
}

// -- the precedence ladder ---------------------------------------------------

#[test]
fn multiplicative_binds_tighter_than_additive() {
    assert_eq!(shape("1 + 2 * 3"), "(+ 1 (* 2 3))");
    assert_eq!(shape("1 * 2 + 3"), "(+ (* 1 2) 3)");
    assert_eq!(shape("1 + 2 % 3"), "(+ 1 (% 2 3))");
}

#[test]
fn the_whole_ladder_is_in_order() {
    // One expression per rung, each proving it binds looser than the rung
    // below: assignment < || < && < equality < relational < additive.
    assert_eq!(shape_after("var a;", "a = 1 || 2"), "(= a (|| 1 2))");
    assert_eq!(shape("1 || 2 && 3"), "(|| 1 (&& 2 3))");
    assert_eq!(shape("1 && 2 == 3"), "(&& 1 (== 2 3))");
    assert_eq!(shape("1 == 2 < 3"), "(== 1 (< 2 3))");
    assert_eq!(shape("1 < 2 + 3"), "(< 1 (+ 2 3))");
}

#[test]
fn comparison_and_equality_are_four_operators_each() {
    assert_eq!(shape("1 < 2"), "(< 1 2)");
    assert_eq!(shape("1 <= 2"), "(<= 1 2)");
    assert_eq!(shape("1 > 2"), "(> 1 2)");
    assert_eq!(shape("1 >= 2"), "(>= 1 2)");
    assert_eq!(shape("1 == 2"), "(== 1 2)");
    assert_eq!(shape("1 != 2"), "(!= 1 2)");
    assert_eq!(shape("1 === 2"), "(=== 1 2)");
    assert_eq!(shape("1 !== 2"), "(!== 1 2)");
}

#[test]
fn binary_levels_are_left_associative() {
    assert_eq!(shape("1 - 2 - 3"), "(- (- 1 2) 3)");
    assert_eq!(shape("1 / 2 / 3"), "(/ (/ 1 2) 3)");
    assert_eq!(shape("1 < 2 < 3"), "(< (< 1 2) 3)");
    assert_eq!(shape("1 && 2 && 3"), "(&& (&& 1 2) 3)");
    assert_eq!(shape("1 || 2 || 3"), "(|| (|| 1 2) 3)");
}

#[test]
fn assignment_is_right_associative() {
    // `right = left`, not `left + 1`: the loop must be able to re-enter at the
    // same power for the second `=` to end up inside the first.
    assert_eq!(shape_after("var a, b;", "a = b = 1"), "(= a (= b 1))");
}

/// ECMA-262 13.14: `ConditionalExpression : ShortCircuitExpression ?
/// AssignmentExpression : AssignmentExpression`. Two claims about shape, and
/// the s-expression is the readable way to state either.
#[test]
fn the_conditional_sits_between_assignment_and_the_short_circuit_operators() {
    assert_eq!(shape("1 ? 2 : 3"), "(?: 1 2 3)");
    // Looser than `||`, so the whole `||` is the test.
    assert_eq!(shape("1 || 2 ? 3 : 4"), "(?: (|| 1 2) 3 4)");
    assert_eq!(shape("1 ? 2 : 3 || 4"), "(?: 1 2 (|| 3 4))");
    // Tighter than assignment, so a conditional is what an `=` takes.
    assert_eq!(shape_after("var a;", "a = 1 ? 2 : 3"), "(= a (?: 1 2 3))");
    // And an AssignmentExpression is what each branch takes.
    assert_eq!(shape_after("var a;", "1 ? a = 2 : 3"), "(?: 1 (= a 2) 3)");
}

/// Right-associative, which is what makes the second `?` land inside the
/// first one's else branch rather than beside it.
#[test]
fn the_conditional_is_right_associative() {
    assert_eq!(shape("1 ? 2 : 3 ? 4 : 5"), "(?: 1 2 (?: 3 4 5))");
    assert_eq!(
        shape("1 ? 2 ? 3 : 4 : 5"),
        "(?: 1 (?: 2 3 4) 5)",
        "and it nests in the then branch too"
    );
}

#[test]
fn compound_assignment_carries_its_operator() {
    assert_eq!(shape_after("var a;", "a += 1"), "(+= a 1)");
    assert_eq!(shape_after("var a;", "a -= 1"), "(-= a 1)");
    assert_eq!(shape_after("var a;", "a *= 1"), "(*= a 1)");
    assert_eq!(shape_after("var a;", "a /= 1"), "(/= a 1)");
    assert_eq!(shape_after("var a;", "a %= 1"), "(%= a 1)");
}

#[test]
fn a_short_circuit_operator_is_not_a_binary_node() {
    // `&&`/`||` do not evaluate their right operand unconditionally, so they
    // must not share a node with the operators that do. This is the assertion
    // that stops a lowering from emitting a call for them.
    let p = program("1 && 2;");
    assert!(
        matches!(only_expr(&p).kind, ExprKind::Logical(LogicalOp::And, ..)),
        "&& must be its own node so lowering emits a branch"
    );
    let p = program("1 || 2;");
    assert!(matches!(
        only_expr(&p).kind,
        ExprKind::Logical(LogicalOp::Or, ..)
    ));
}

#[test]
fn unary_operators_bind_tighter_than_every_infix_level() {
    assert_eq!(shape("-1 + 2"), "(+ -1 2)");
    assert_eq!(shape_after("var a;", "-a * 2"), "(* (- a) 2)");
    assert_eq!(shape("!1 && 2"), "(&& (! 1) 2)");
    assert_eq!(shape("!1 + 2"), "(+ (! 1) 2)");
    assert_eq!(shape("+1 - 2"), "(- (+ 1) 2)");
    assert_eq!(shape("!!1"), "(! (! 1))");
}

#[test]
fn a_minus_on_a_literal_reaches_the_literal() {
    // `i32::MIN` has no positive counterpart, so the sign has to be folded in
    // for the boundary value to stay an `Int` rather than becoming a `Num`.
    assert_eq!(shape("-2147483648"), "-2147483648");
    // One past it is a Number, not a refusal: there is one numeric type here
    // and `i32` is the *representation* the parser keeps small literals in,
    // never a bound on the language. The second half of this test used to
    // assert the refusal.
    assert_eq!(shape("-2147483649"), "-2147483649");
    // A fraction is **not** folded, and that is the rule holding rather than
    // an omission: the fold exists because `i32::MIN` has no positive
    // counterpart, not to save an instruction, so a literal with no such
    // problem keeps the unary operator ECMA-262 says is there.
    assert_eq!(shape("-1.5"), "(- 1.5)");
}

#[test]
fn increment_and_decrement_are_prefix_and_postfix() {
    assert_eq!(shape_after("var a;", "a++"), "(post++ a)");
    assert_eq!(shape_after("var a;", "++a"), "(pre++ a)");
    assert_eq!(shape_after("var a;", "a--"), "(post-- a)");
    assert_eq!(shape_after("var a;", "--a"), "(pre-- a)");
}

#[test]
fn parentheses_override_precedence() {
    assert_eq!(shape("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    assert_eq!(shape("((1))"), "1");
}

// -- literals ----------------------------------------------------------------

#[test]
fn the_literals_of_the_value_representation_are_expressions() {
    assert_eq!(shape("\"hi\""), "\"hi\"");
    assert_eq!(shape("'a\\nb'"), "\"a\\nb\"");
    assert_eq!(shape("true"), "true");
    assert_eq!(shape("false"), "false");
    assert_eq!(shape("null"), "null");
    assert_eq!(shape("undefined"), "undefined");
    assert_eq!(shape("$3"), "$3");
}

#[test]
fn the_script_declares_one_parameter_per_argument_it_names() {
    assert_eq!(program("1;").arg_count, 0);
    assert_eq!(program("$0;").arg_count, 1);
    assert_eq!(program("$2;").arg_count, 3);
    assert_eq!(program("$1 + $1;").arg_count, 2);
}

// -- calls -------------------------------------------------------------------

#[test]
fn a_call_carries_real_arguments() {
    assert_eq!(sexpr(only_expr(&hosted("g(1, 2)"))), "(call g 1 2)");
    assert_eq!(sexpr(only_expr(&hosted("g()"))), "(call g)");
    assert_eq!(
        sexpr(only_expr(&hosted("g(h(1), 2 + 3)"))),
        "(call g (call h 1) (+ 2 3))"
    );
    // A trailing comma in an argument list is ES2017 and costs nothing here.
    assert_eq!(sexpr(only_expr(&hosted("g(1,)"))), "(call g 1)");
}

#[test]
fn a_free_name_is_a_host_import_only_when_the_caller_asked_for_one() {
    let p = hosted("g");
    assert!(matches!(resolution(only_expr(&p)), Res::Host(n) if n == "g"));
    assert!(refuse("g;").contains("finds no declaration of `g`"));
}

// -- declarations and scope --------------------------------------------------

#[test]
fn a_declaration_binds_a_name_the_rest_of_the_script_can_read() {
    let p = program("let x = 1; x;");
    let declared = match &body(&p)[0].kind {
        StmtKind::Decl(decls) => decls[0].binding,
        other => panic!("expected a declaration, got {other:?}"),
    };
    let read = shape_expr(&p, 1);
    assert_eq!(resolution(&read), &Res::Local(declared));
    assert_eq!(p.binding(declared).kind, BindingKind::Let);
}

#[test]
fn one_declaration_may_bind_several_names() {
    let p = program("let a = 1, b = 2, c;");
    match &body(&p)[0].kind {
        StmtKind::Decl(decls) => {
            assert_eq!(decls.len(), 3);
            assert!(decls[2].init.is_none());
        }
        other => panic!("expected a declaration, got {other:?}"),
    }
}

#[test]
fn a_let_does_not_escape_its_block() {
    program("{ let c = 1; c; }");
    assert!(refuse("{ let c = 1; } c;").contains("finds no declaration of `c`"));
}

#[test]
fn a_var_declared_in_a_block_belongs_to_the_whole_function() {
    let p = program("{ var b = 1; } b;");
    let declared = match &body(&p)[0].kind {
        StmtKind::Block(inner) => match &inner[0].kind {
            StmtKind::Decl(decls) => decls[0].binding,
            other => panic!("expected a declaration, got {other:?}"),
        },
        other => panic!("expected a block, got {other:?}"),
    };
    assert_eq!(resolution(&shape_expr(&p, 1)), &Res::Local(declared));
    assert_eq!(p.binding(declared).kind, BindingKind::Var);
}

#[test]
fn sibling_blocks_may_each_bind_the_same_name() {
    let p = program("{ let x = 1; } { let x = 2; }");
    assert_eq!(
        p.script().bindings.len(),
        2,
        "two blocks, two independent bindings"
    );
}

#[test]
fn a_name_may_not_be_bound_twice_in_one_scope() {
    assert!(refuse("let x = 1; let x = 2;").contains("twice"));
    assert!(refuse("let x = 1; const x = 2;").contains("twice"));
    assert!(refuse("let x = 1; var x = 2;").contains("twice"));
    assert!(refuse("function f(a, a) { return 1; }").contains("twice"));
    // A `var` reaches out of the block it is written in, so it collides with
    // a `let` it would otherwise never have met.
    assert!(refuse("let x = 1; { var x = 2; }").contains("twice"));
    // `var` is the exception ECMA-262 14.3.2 makes: redeclaring one is legal
    // and names the binding that is already there, through a block as well.
    let p = program("var x = 1; var x = 2;");
    assert_eq!(p.script().bindings.len(), 1);
    let p = program("var x = 1; { var x = 2; }");
    assert_eq!(p.script().bindings.len(), 1);
}

#[test]
fn an_inner_scope_may_shadow_an_outer_binding() {
    let p = program("let x = 1; { let x = 2; x; }");
    assert_eq!(p.script().bindings.len(), 2);
    // A `for` header is a scope of its own, so its body may shadow it and the
    // statement after the loop cannot see it at all.
    let p = program("for (let i = 0; i < 1; i++) { let i = 2; i; }");
    assert_eq!(p.script().bindings.len(), 2);
    assert!(refuse("for (let i = 0; i < 1; i++) { 1; } i;").contains("finds no declaration"));
}

#[test]
fn a_const_may_not_be_assigned_to() {
    // A DOCUMENTED DIVERGENCE: ECMA-262 makes this a runtime TypeError, and
    // this engine has no way to throw one yet. Silently letting the write
    // through would corrupt a value the language guarantees is fixed, so the
    // refusal is loud and early.
    let message = refuse("const x = 1; x = 2;");
    assert!(message.contains("const"), "{message}");
    assert!(message.contains('x'), "{message}");
    assert!(refuse("const x = 1; x++;").contains("const"));
    assert!(refuse("const x = 1; x += 1;").contains("const"));
}

#[test]
fn only_a_name_may_be_assigned_to() {
    assert!(!refuse("1 = 2;").is_empty());
    assert!(!refuse("1++;").is_empty());
}

// -- functions ---------------------------------------------------------------

#[test]
fn a_function_declaration_binds_its_name_and_its_parameters() {
    let p = program("function add(a, b) { return a + b; }");
    assert_eq!(p.functions.len(), 2, "the script and `add`");
    let (binding, func) = match &body(&p)[0].kind {
        StmtKind::Func { binding, func } => (*binding, *func),
        other => panic!("expected a function declaration, got {other:?}"),
    };
    assert_eq!(p.binding(binding).name, "add");
    assert_eq!(p.binding(binding).kind, BindingKind::Function(func));
    let add = p.func(func);
    assert_eq!(add.params.len(), 2);
    assert_eq!(p.binding(add.params[0]).name, "a");
    assert_eq!(p.binding(add.params[0]).kind, BindingKind::Param);
    // The parameters are the first two slots of the function's own storage.
    assert_eq!(p.binding(add.params[0]).slot, 0);
    assert_eq!(p.binding(add.params[1]).slot, 1);
    match &add.body[0].kind {
        StmtKind::Return(Some(e)) => assert_eq!(sexpr(e), "(+ a b)"),
        other => panic!("expected a return, got {other:?}"),
    }
}

#[test]
fn a_function_declaration_is_visible_before_it_is_written() {
    // Hoisting: the reference is resolved after the whole program is read, so
    // a forward call and mutual recursion both work.
    program("f(); function f() { return 1; }");
    program("function a() { return b(); } function b() { return a(); }");
}

#[test]
fn a_function_may_be_declared_inside_a_function() {
    let p = program("function outer() { function inner() { return 1; } return inner(); }");
    assert_eq!(p.functions.len(), 3);
}

#[test]
fn a_nested_function_capturing_an_enclosing_binding_resolves_and_lays_out_its_environment() {
    // This used to be `..._may_not_capture_...` and asserted a refusal. The
    // parser's job is now to *record* the capture, and both halves of that
    // record are what the lowering reads: the binding is marked so its storage
    // becomes a cell, and the reading function's `captures` is the environment
    // layout every creator of it fills in that order.
    let p =
        program("function outer() { let x = 1; function inner() { return x; } return inner(); }");

    let x = p
        .bindings
        .iter()
        .find(|b| b.name == "x")
        .expect("the binding is there");
    assert!(x.captured, "a binding something nested reads is captured");

    let inner = p
        .functions
        .iter()
        .find(|f| f.name.as_deref() == Some("inner"))
        .expect("the nested function is there");
    assert_eq!(inner.captures.len(), 1, "inner captures exactly `x`");

    let outer = p
        .functions
        .iter()
        .find(|f| f.name.as_deref() == Some("outer"))
        .expect("the enclosing function is there");
    assert!(
        outer.captures.is_empty(),
        "the function that *owns* the binding captures nothing -- it has the cell"
    );
}

#[test]
fn a_function_may_read_a_script_level_binding() {
    // The script's own bindings are the one enclosing scope a function can
    // see: they get module-level storage rather than a frame slot.
    let p = program("let total = 0; function bump() { total = total + 1; }");
    let bump = p.func(match &body(&p)[1].kind {
        StmtKind::Func { func, .. } => *func,
        other => panic!("expected a function declaration, got {other:?}"),
    });
    match &bump.body[0].kind {
        StmtKind::Expr(Expr {
            kind: ExprKind::Assign { target, .. },
            ..
        }) => assert!(matches!(resolution(target), Res::Global(_))),
        other => panic!("expected an assignment, got {other:?}"),
    }
}

#[test]
fn an_argument_reference_belongs_to_the_script() {
    let message = refuse("function f() { return $0; }");
    assert!(
        message.contains("$0") || message.contains("argument"),
        "{message}"
    );
}

#[test]
fn a_function_expression_is_an_expression() {
    let p = program("const f = function (n) { return n; }; f(1);");
    assert_eq!(p.functions.len(), 2);
    let binding = match &body(&p)[0].kind {
        StmtKind::Decl(decls) => decls[0].binding,
        other => panic!("expected a declaration, got {other:?}"),
    };
    // A `const` bound directly to a function expression is a known call
    // target, exactly as a declaration is: it can never be reassigned.
    assert!(matches!(p.binding(binding).kind, BindingKind::Function(_)));
}

/// Calling anything at all now parses and resolves. What the callee turns out
/// to hold is a property of the run, not of the text, so the front end has
/// nothing to refuse -- ECMA-262 13.3.6.1 makes a non-callable callee a
/// run-time TypeError, and `tests/function_values.rs` is where the trap is
/// shown.
#[test]
fn calling_a_value_that_is_not_a_known_function_now_resolves() {
    program("let f = 1; f();");
    program("let f = function () { return 1; }; f();");
    program("const o = {}; o.m();");
    // The one thing resolution still settles about a callee: a name that is
    // declared nowhere is still nowhere.
    let message = refuse("nope();");
    assert!(
        message.contains("finds no declaration of `nope`"),
        "{message}"
    );
}

/// A name bound to a known function resolves to the same [`Res::Callee`]
/// whether it is called or read -- the binding names the function rather than
/// storage, so neither use is a capture.
#[test]
fn a_function_used_as_a_value_resolves_to_the_function_it_names() {
    let p = program("function f() { return 1; } f;");
    let e = match &body(&p)[1].kind {
        StmtKind::Expr(e) => e,
        other => panic!("expected an expression statement, got {other:?}"),
    };
    match &e.kind {
        ExprKind::Name(name) => {
            assert!(matches!(name.res, Res::Callee(_)), "got {:?}", name.res);
        }
        other => panic!("expected a name, got {other:?}"),
    }
    // And from inside a nested function, where storage would have been a
    // capture and a function index is not.
    program("function f() { return 1; } function g() { return f; } g();");
}

// -- spans and diagnostics ---------------------------------------------------

#[test]
fn a_resolution_diagnostic_points_at_the_name() {
    // The first diagnostic that is raised *after* parsing, which is why the
    // tree carries spans at all.
    let source = "let a = 1;\n  bee;";
    let e = err(source);
    assert_eq!(e.offset, source.find("bee").unwrap());
}

#[test]
fn a_node_carries_the_offset_of_its_first_token() {
    let p = program("1 + 2;");
    let e = only_expr(&p);
    assert_eq!(e.span.offset(), 0);
    match &e.kind {
        ExprKind::Binary(_, _, rhs) => assert_eq!(rhs.span.offset(), 4),
        other => panic!("expected a binary node, got {other:?}"),
    }
}

#[test]
fn every_refusal_speaks_for_the_engine() {
    for source in [
        "",
        "1 +",
        "(1 + 2",
        "let ;",
        "if (1",
        "function () { return 1; }",
        "{ 1;",
        "let x = 1; let x = 2;",
        "const x = 1; x = 2;",
        "g;",
    ] {
        let message = refuse(source);
        let lowered = message.to_lowercase();
        assert!(
            message.starts_with("this engine "),
            "{source:?} gave {message:?}, which does not speak for the engine"
        );
        assert!(
            !lowered.contains("syntax error") && !lowered.contains("invalid"),
            "{source:?} gave {message:?}, which is the vague wording this engine forbids"
        );
    }
}

#[test]
fn what_the_front_end_cannot_read_yet_names_the_construct() {
    for (source, phrase) in [
        // `$0.x` and `({})` left this table when M3 landed property access and
        // object literals; the property forms still ahead of the engine took
        // their place, and `tests/objects_m3.rs` asserts the rest.
        ("({ [k]: 1 });", "computed property keys"),
        ("({ f() { } });", "methods in object literals"),
        // `[1];` left this table when the Array milestone landed it; the one
        // array form still ahead of the engine took its place.
        ("[1, , 2];", "elisions in an array literal"),
        ("1, 2;", "comma"),
        // `1 ? 2 : 3` left this table when the conditional landed; a `:` the
        // parser cannot use is a label now, which is what it says.
        ("a: 1;", "labelled statements"),
        ("2 ** 3;", "exponentiation"),
        // `1 & 2;` left this table when the bitwise operators landed
        // (2026-08-31); `??` is the operator still ahead of the engine.
        ("1 ?? 2;", "nullish"),
        ("for (x of y) { 1; }", "of"),
    ] {
        let message = refuse(source);
        assert!(
            message.starts_with("this engine does not support ") && message.ends_with(" yet"),
            "{source:?} gave {message:?}, which does not name an engine capability boundary"
        );
        assert!(
            message.contains(phrase),
            "{source:?} gave {message:?}, which does not name {phrase:?}"
        );
    }
}
