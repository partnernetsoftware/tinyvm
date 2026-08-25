//! Adversarial inputs: things a caller can write that this compiler gets wrong.
//!
//! Every other suite in this crate asks "does the feature work". This one asks
//! the opposite question -- what input makes the compiler produce wrong wasm,
//! panic, abort, hang, or emit bytes tinyvm refuses -- and it asks it of the
//! same public surface a host uses, [`compile_qjs_m1`] plus tinyvm's load gate.
//!
//! Two kinds of test live here and they are labelled:
//!
//! * **FINDING** -- a defect. The test asserts what ECMA-262 (or the crate's own
//!   stated contract) says should happen, so it *fails today*. It is the bug
//!   report, in executable form; fixing `src/**` is what turns it green.
//! * **GUARD** -- an attack the engine survived. It passes, and it is here so
//!   that the next change to the lowering cannot quietly lose the property. The
//!   label-depth guards are the important ones: a branch depth that is one off
//!   miscompiles silently rather than failing to load, so the only evidence that
//!   can settle it is an executed loop nest with an arithmetic answer.
//!
//! A rejection carrying a capability diagnostic is correct behaviour and is
//! never a finding here. A panic, an abort, a silently wrong number, or a module
//! the load gate refuses always is.

use tinyvm::{Limits, WasmError, WasmModule};
use tinyvm_qjs::{Boundary, CompileError, Names, Options, Value, compile_qjs_m1};

// =========================================================================
// Harness
// =========================================================================

/// The gate's whole answer, as one sentence: these cases are tabulated by the
/// message they must produce, so that is what comes out.
fn gate(wasm: &[u8]) -> Result<WasmModule, &'static str> {
    WasmModule::from_bytes_with(wasm, Limits::default()).map_err(|e: WasmError| e.message())
}

/// Compile, load, instantiate, call `main`. Every failure along the way comes
/// back as a string so a test can say *which* stage refused the input.
fn run(source: &str) -> Result<Value, String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compile: {e}"))?;
    let module = gate(&wasm).map_err(|m| format!("load gate: {m}"))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiate: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .map_err(|e| format!("invoke: {}", e.message()))?;
    Value::returned(&vals)
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(run(source), Ok(Value::Number(want)), "{source:?}");
}

/// Compile and push the bytes through the load gate, without running them. The
/// question several findings ask is whether *bytes that compiled* are bytes
/// tinyvm will take.
fn compile_and_load(source: &str) -> Result<(), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compile: {e}"))?;
    gate(&wasm)
        .map(|_| ())
        .map_err(|m| format!("load gate: {m}"))
}

#[track_caller]
fn refuse(source: &str) -> CompileError {
    match compile_qjs_m1(source) {
        Ok(_) => panic!("{source:?}: compiled; expected a capability diagnostic"),
        Err(e) => e,
    }
}

// =========================================================================
// FINDING 1 -- a named function expression with parameters miscompiles
// =========================================================================

/// `function_body` declares the function-expression's own name into the
/// function's *own* scope before the parameters, so it takes binding slot 0 and
/// shifts every parameter's slot by one. The lowering maps `slot * WIDTH` to a
/// local index, so each parameter is read from the local one JS value past the
/// one the caller filled.
///
/// In a debug build the `debug_assert!` in `emit::m1::Lower::new` catches it and
/// panics. In a release build nothing catches it: the call returns `undefined`,
/// or `NaN` once arithmetic touches it. Both are worse than a diagnostic, and
/// the input is ordinary JavaScript.
#[test]
fn finding_1_a_named_function_expression_may_take_parameters() {
    number("const f = function g(a) { return a; }; return f(7);", 7.0);
}

#[test]
fn finding_1_the_same_defect_through_an_iife() {
    number("return (function g(a) { return a; })(7);", 7.0);
}

/// Two parameters instead of one: the second reads the scratch local the
/// lowering added for the shifted slot, so the answer is `NaN` rather than
/// merely the wrong argument.
#[test]
fn finding_1_two_parameters_produce_nan() {
    number(
        "const f = function g(a, b) { return a * 10 + b; }; return f(1, 2);",
        12.0,
    );
}

/// The neighbouring case that *does* work, so the finding is pinned to the
/// parameters and not to named function expressions as such.
#[test]
fn guard_a_named_function_expression_without_parameters_is_fine() {
    number("const f = function g() { return 7; }; return f();", 7.0);
}

/// ECMA-262 15.2.5: the binding for a function expression's own name lives in a
/// function environment that the parameter list shadows, so a parameter may
/// reuse the name. This engine puts both in one scope and reports the script as
/// malformed -- "cannot bind `f` twice in one scope" -- which blames an author
/// who wrote valid JavaScript. Same root cause as the three tests above.
#[test]
fn finding_1_a_parameter_may_shadow_the_function_expression_name() {
    number("const g = function f(f) { return f; }; return g(1);", 1.0);
}

// =========================================================================
// FINDING 2 -- string literals overrun the one page the module declares
// =========================================================================

/// `emit::m1` declares `MEMORY_MIN_PAGES = 1` unconditionally while the string
/// pool grows with the source. tinyvm checks every active data segment against
/// the *declared minimum* at load time, so past 65 524 bytes of pooled literals
/// the compiler emits bytes it has already decided are wasm and the gate throws
/// them out. Nothing in the pipeline says a word about it.
///
/// The fix is either to size the declared minimum from `pool.heap_start()` or to
/// refuse with a capability diagnostic. What must not happen is a successful
/// compile whose output cannot be loaded.
#[test]
fn finding_2_one_long_string_literal_must_not_defeat_the_load_gate() {
    // 8 (DATA_ORIGIN) + 4 (length header) + len must stay inside one 64 KiB
    // page; 65 528 is the first multiple of four past that line.
    let source = format!("return \"{}\";", "x".repeat(65_528));
    assert_eq!(compile_and_load(&source), Ok(()));
}

/// Not an exotic-input problem: two thousand ordinary literals reach the same
/// wall, and no single one of them is remarkable.
#[test]
fn finding_2_many_ordinary_string_literals_reach_the_same_wall() {
    let literals: String = (0..2_000)
        .map(|i| format!("\"literal number {i} padded out a bit\";"))
        .collect();
    assert_eq!(compile_and_load(&format!("{literals}return 1;")), Ok(()));
}

#[test]
fn guard_a_literal_just_inside_the_page_still_loads() {
    let source = format!("return \"{}\";", "x".repeat(65_524));
    assert_eq!(compile_and_load(&source), Ok(()));
}

// =========================================================================
// FINDING 3 -- nested syntax aborts the process
// =========================================================================

/// `parse::m1::{expression, unary, postfix, primary}`, `parse::m1::fill_expr`,
/// `emit::m1::host_expr` and `emit::m1::Lower::expr` all recurse on the tree
/// with no depth counter, so nesting depth is bounded only by the native stack.
/// A stack overflow is not a `Result`: the runtime aborts the whole process
/// (SIGABRT), which for an embedder compiling untrusted `.qjs` is the worst
/// possible failure mode -- worse than a wrong answer, because there is no
/// caller left to hear about it.
///
/// Measured on this machine with libtest's default thread stack: `(((…1…)))`
/// aborts at depth 300, `{ … }` at 500, `f(f(…))` at 500, and `1+1+…+1` at
/// 2 000 terms. With `RUST_MIN_STACK=67108864` the paren case survives 4 000 and
/// still aborts at 40 000 -- so there is no stack size that makes this safe, only
/// a depth limit in the compiler.
///
/// The limit is `parse::m1::MAX_FRAMES`, counted in frames of recursive descent
/// rather than in nesting levels because a `(` and a `+` do not cost the same.
/// This test ran `#[ignore]`d while it aborted the binary; it does not any more.
#[test]
fn finding_3_deep_nesting_aborts_the_process() {
    for source in [
        format!("return {}1{};", "(".repeat(10_000), ")".repeat(10_000)),
        format!("{}return 1;{}", "{".repeat(10_000), "}".repeat(10_000)),
        format!("return {};", vec!["1"; 10_000].join("+")),
    ] {
        // Any answer is acceptable here except crashing: a value, or a
        // diagnostic naming the depth the engine stops at.
        let outcome = compile_qjs_m1(&source);
        assert!(
            outcome.is_err(),
            "expected a capability diagnostic naming a nesting limit"
        );
    }
}

/// The shallow end of the same shapes, to pin that the recursion is otherwise
/// correct and that a fix must not change these answers.
#[test]
fn guard_shallow_nesting_is_lowered_correctly() {
    let depth = 64;
    number(
        &format!("return {}1{};", "(".repeat(depth), ")".repeat(depth)),
        1.0,
    );
    number(
        &format!("{}return 1;{}", "{".repeat(depth), "}".repeat(depth)),
        1.0,
    );
    number(&format!("return {};", vec!["1"; 256].join("+")), 256.0);
    // An even number of `!` is ToBoolean, not the identity.
    assert_eq!(
        run(&format!("return {}1;", "!".repeat(200))),
        Ok(Value::Bool(true))
    );
}

// =========================================================================
// FINDING 4 -- legacy octal literals read as decimal
// =========================================================================

/// `lex::Lexer::number` special-cases only `0x`, `0o` and `0b`; anything else
/// starting with `0` falls through to `parse::<u64>()`. So `0777` becomes 777.
///
/// There is no JavaScript in which that is the value. ECMA-262 12.9.3 makes
/// `0777` a LegacyOctalIntegerLiteral -- 511 in sloppy mode -- and a SyntaxError
/// in strict mode; `parse::m1`'s own header says this subset *is* strict mode
/// and that "there is no legacy octal -- which the lexer already refuses". It
/// does not refuse it. This is the quietest bug in the file: no trap, no
/// diagnostic, just a different number.
#[test]
fn finding_4_a_legacy_octal_literal_must_not_read_as_decimal() {
    let error = refuse("return 0777;");
    assert_eq!(error.boundary, Boundary::Subset, "{}", error.message);
}

/// ECMA-262 12.9.3 NonOctalDecimalIntegerLiteral: `08` and `09` are also strict
/// -mode SyntaxErrors. Here `08` is 8 -- which happens to match sloppy mode, so
/// this one is only a missing refusal, not a wrong value.
#[test]
fn finding_4_a_non_octal_decimal_literal_must_be_refused() {
    refuse("return 08;");
}

#[test]
fn finding_4_leading_zero_changes_the_value() {
    // Sloppy JavaScript: 8. Strict JavaScript: SyntaxError. This engine: 10.
    assert_ne!(
        run("return 010;"),
        Ok(Value::Number(10.0)),
        "`010` is 8 or a SyntaxError, never 10"
    );
}

#[test]
fn guard_the_other_radix_prefixes_name_their_boundary() {
    for source in ["return 0x1f;", "return 0o17;", "return 0b11;", "return 1n;"] {
        let error = refuse(source);
        assert!(
            error.message.starts_with("this engine does not support"),
            "{source:?}: {}",
            error.message
        );
    }
}

// =========================================================================
// FINDING 5 -- ASI supplies a `for` header semicolon
// =========================================================================

/// ECMA-262 12.10: "a semicolon is never inserted automatically if the
/// semicolon would then be parsed as one of the two semicolons in the header of
/// a `for` statement." `lex::insert_restricted_semicolons` applies rule 3 to the
/// whole token stream with no idea of grammar position, and `lex.rs`'s own
/// header says so -- "that is a grammar position and belongs to the caller".
/// The caller, `parse::m1::for_parts`, calls `expect(&TokenKind::Semi, …)`,
/// which eats an inserted semicolon as happily as a written one; `Token::inserted`
/// is never read by anything.
///
/// The result is not a rejected program made worse: it is a program that is not
/// JavaScript at all being compiled into a *different* program. A one-semicolon
/// `for` header is a SyntaxError in every engine; here it silently becomes a
/// three-part header whose test is the `--a` the author wrote as part of the
/// initialiser.
#[test]
fn finding_5_asi_must_not_supply_a_for_header_semicolon() {
    // Written: `for (a = a \n --a; a)`. One semicolon, so: SyntaxError.
    // Compiled as: `for (a = a; --a; a)`, which runs and answers 0.
    refuse("let a = 1;\nfor (a = a\n--a; a) { }\nreturn a;");
}

/// The same defect turning a non-program into a non-terminating one: this
/// compiles to `for (a; ++b; b < 3)`, whose update is a comparison and whose
/// test never goes falsy, so the run ends on tinyvm's step budget.
#[test]
fn finding_5_the_invented_header_can_be_an_infinite_loop() {
    refuse("let a = 1;\nlet b = 0;\nfor (a\n++b; b < 3) { }\nreturn b;");
}

/// Outside a `for` header the same rule 3 insertion is exactly right, so a fix
/// must be positional and not a retreat from ASI.
#[test]
fn guard_restricted_productions_still_insert_their_semicolon() {
    // `return` [no LineTerminator here] Expression.
    assert_eq!(run("return\n1;"), Ok(Value::Undefined));
    // LeftHandSideExpression [no LineTerminator here] `++`: `b` is `a`, not
    // `a++`, and the `++a` that follows is a prefix update.
    number("let a = 1;\nlet b = a\n++a\nreturn b;", 1.0);
    // Rules 1 and 2, which the parser owns.
    number("let a = 1\nlet b = 2\nreturn a + b;", 3.0);
}

// =========================================================================
// FINDING 6 -- no temporal dead zone
// =========================================================================

/// ECMA-262 8.2.4 and 13.1.3: a `let`/`const` binding is uninitialised until its
/// declaration is evaluated, and reading it before then throws a ReferenceError.
/// Here the storage is a zeroed local or global, and `TAG_UNDEFINED` is 0, so
/// the read succeeds and yields `undefined`.
///
/// That is the fabricated value `emit`'s own header refuses for division by
/// zero: "a wrong number that flows on is indistinguishable from a real one".
/// The statically visible case below wants a diagnostic; the dynamic one wants
/// the same `unreachable` the runtime already uses for an unimplemented
/// conversion.
#[test]
fn finding_6_a_read_before_initialisation_must_not_yield_undefined() {
    // `let x = x` reads `x` inside its own initialiser: always a ReferenceError.
    refuse("let x = x; return 1;");
}

#[test]
fn finding_6_a_hoisted_let_is_not_undefined() {
    assert_ne!(
        run("return x; let x = 1;"),
        Ok(Value::Undefined),
        "reading a `let` before its declaration is a ReferenceError, not undefined"
    );
}

/// `var` really is `undefined` before its declaration, so the fix must not
/// sweep this up with it.
#[test]
fn guard_a_hoisted_var_is_undefined() {
    assert_eq!(run("var a; return a;"), Ok(Value::Undefined));
}

// =========================================================================
// FINDING 7 -- a declaration is accepted as a statement body
// =========================================================================

/// ECMA-262 14.6 (`if`) and 14.7 (iteration statements) take a `Statement`, and
/// `Statement` is not `Declaration`. `parse::m1::statement` is called for the
/// body position with no restriction, so both of these compile.
///
/// Lower severity than the rest -- these accept a non-program rather than
/// mis-running one -- but they are the same class as finding 5 and the same
/// shape of fix: the grammar position has to reach the callee.
#[test]
fn finding_7_a_lexical_declaration_is_not_an_if_body() {
    refuse("if (1) let x = 1; return 2;");
}

#[test]
fn finding_7_a_function_declaration_is_not_a_loop_body() {
    refuse("while (0) function f() {} return 1;");
}

// =========================================================================
// FINDING 8 -- numeric separators get a diagnostic that blames the author
// =========================================================================

/// `1_000` is ES2021 and perfectly good JavaScript. `lex::Lexer::number` ends
/// the digit run at `_`, so the source lexes as `1` followed by the identifier
/// `_000`, and what the author is told is "this engine needs a `;` to end the
/// statement, and found a name instead".
///
/// That is the failure `diag.rs` exists to prevent: the sentence describes a
/// mistake the author did not make and never names the boundary. Every other
/// numeric form the lexer cannot lower -- hex, octal, binary, BigInt, exponent,
/// fraction -- names itself; this one does not.
#[test]
fn finding_8_numeric_separators_must_name_their_own_boundary() {
    let error = refuse("return 1_000;");
    assert!(
        error.message.contains("numeric separator"),
        "expected the boundary to be named, got: {}",
        error.message
    );
}

// =========================================================================
// FINDING 9 -- the guest heap ceiling is a bare `unreachable`
// =========================================================================

/// `emit::m1::MEMORY_MAX_PAGES` is 16, so a compiled module can never hold more
/// than 1 MiB however generous the host's [`Limits`] is -- the default allows
/// 256 pages. `runtime`'s `alloc` says the opposite in its own comment ("the
/// host's `tinyvm::Limits` is what actually bounds it, which is where the bound
/// belongs"), and when the ceiling is hit it raises `Unreachable`, which reaches
/// the host as the same "unreachable executed" a type error raises.
///
/// So a host cannot tell an out-of-memory guest from a broken one, and cannot
/// raise the ceiling either.
#[test]
fn finding_9_running_out_of_guest_heap_is_indistinguishable_from_a_type_error() {
    // 8 bytes doubled eighteen times is 2 MiB, past the module's own maximum.
    let outcome =
        run("let s = \"abcdefgh\"; for (let i = 0; i < 18; i = i + 1) { s = s + s; } return s;");
    assert_ne!(
        outcome,
        Err("invoke: unreachable executed".to_string()),
        "an exhausted guest heap must not report as `unreachable executed`"
    );
}

#[test]
fn guard_concatenation_below_the_ceiling_works() {
    // 8 bytes doubled fourteen times is 128 KiB: two page growths, no trap.
    match run("let s = \"ab\"; for (let i = 0; i < 14; i = i + 1) { s = s + s; } return s;") {
        Ok(Value::String(_)) => {}
        other => panic!("expected a String, got {other:?}"),
    }
}

// =========================================================================
// GUARDS -- attacks the engine held against
// =========================================================================

/// Branch depth, the thing that miscompiles silently rather than failing to
/// load. Three loop nests with an `if`/`else` in the innermost: a `br` that
/// left one loop too many would skip the `n = n + 100`, one too few would spin,
/// and either way the arithmetic changes. 2 x (3 x ((2 x 1) + (2 x 10)) + 100)
/// per outer pass, plus 1000.
#[test]
fn guard_three_nested_loops_branch_to_the_right_labels() {
    number(
        "
        let n = 0;
        for (let i = 0; i < 2; i = i + 1) {
            let j = 0;
            while (j < 3) {
                for (let k = 0; k < 4; k = k + 1) {
                    if (k < 2) { n = n + 1; } else { n = n + 10; }
                }
                n = n + 100;
                j = j + 1;
            }
            n = n + 1000;
        }
        return n;
        ",
        2732.0,
    );
}

/// The `if`/`else` form opens two blocks and branches to both of them. Two
/// hundred of them in sequence, each one's answer feeding the next, so a wrong
/// depth anywhere in the chain lands on 9999.
#[test]
fn guard_a_long_if_else_chain_keeps_its_depths() {
    let mut source = String::from("let n = 0;\n");
    for i in 0..200 {
        source.push_str(&format!(
            "if (n == {i}) {{ n = n + 1; }} else {{ n = 9999; }}\n"
        ));
    }
    source.push_str("return n;");
    number(&source, 200.0);
}

/// A loop whose test opens an `if` block of its own (`&&` lowers through one),
/// which is the case where the loop's `br_if 1` would be counting the wrong
/// frames if the logical operator left its block open.
#[test]
fn guard_a_logical_operator_in_a_loop_test_does_not_shift_the_depths() {
    number(
        "let i = 0; while (i < 3 && true) { i = i + 1; } return i;",
        3.0,
    );
    number(
        "let i = 0; while (i < 3 || false) { i = i + 1; } return i;",
        3.0,
    );
    number(
        "let i = 0; for (; i < 3 && true; i = i + 1) { } return i;",
        3.0,
    );
    number("let i = 0; while (!(i == 3)) { i = i + 1; } return i;", 3.0);
}

/// Integer literals at and past the range the subset accepts. `-2147483648` is
/// the one that only works because the sign is folded onto the magnitude.
#[test]
fn guard_integer_literals_at_the_range_boundary() {
    number("return 2147483647;", 2147483647.0);
    number("return -2147483648;", -2147483648.0);
    for source in [
        "return 2147483648;",
        "return -2147483649;",
        "return 9007199254740993;",
        "return 18446744073709551616;",
        "return 99999999999999999999999999999999;",
    ] {
        assert_eq!(refuse(source).boundary, Boundary::Subset, "{source:?}");
    }
}

/// `$N` is a wasm parameter, so an unbounded index would let a two-character
/// source demand a huge signature. 64 is the ceiling, and 64 JS arguments is
/// 128 wasm parameters, which the encoder and the load gate both take.
#[test]
fn guard_the_argument_index_ceiling_holds() {
    assert!(compile_qjs_m1("return $63;").is_ok());
    for source in ["return $64;", "return $99999999999999999999;"] {
        assert_eq!(refuse(source).boundary, Boundary::Subset, "{source:?}");
    }
    // The same ceiling from the other side: a function of 64 parameters.
    let params: Vec<String> = (0..64).map(|i| format!("p{i}")).collect();
    let args: Vec<String> = (0..64).map(|i| i.to_string()).collect();
    number(
        &format!(
            "function f({}) {{ return p63; }} return f({});",
            params.join(","),
            args.join(",")
        ),
        63.0,
    );
}

/// Source the lexer cannot finish reading. Each one has to name what it was
/// looking for rather than stopping at the first byte it did not like, and none
/// may run off the end of the buffer.
#[test]
fn guard_unterminated_lexemes_are_named_not_crashed() {
    for source in [
        "return \"abc;",
        "return 'a\nb';",
        "return /* abc;",
        "return \"\\",
        "return \"\\u{41",
        "return \"\\x4",
    ] {
        let error = refuse(source);
        assert!(
            error.message.starts_with("this engine "),
            "{source:?}: {}",
            error.message
        );
    }
    // A template is consumed whole so what follows it is read as code, and the
    // whole lexeme is one boundary.
    assert!(refuse("return `abc;").message.contains("template literals"));
}

/// Escape sequences, including the ones a Rust `String` cannot hold.
#[test]
fn guard_string_escapes() {
    assert!(run(r#"return "\u{41}";"#).is_ok());
    // Leading zeros in `\u{}` are legal and must not be mistaken for overflow.
    assert!(compile_qjs_m1(r#"return "\u{000000000041}";"#).is_ok());
    assert!(compile_qjs_m1(r#"return "\u{110000}";"#).is_err());
    assert!(compile_qjs_m1(r#"return "\xZZ";"#).is_err());
    assert!(compile_qjs_m1(r#"return "\u{}";"#).is_err());
    assert!(compile_qjs_m1(r#"return "a\0b";"#).is_ok());
    // A lone surrogate is a good JavaScript string and a bad Rust one, so it is
    // named as this engine's representation limit.
    assert!(
        refuse(r#"return "\ud800";"#)
            .message
            .contains("unpaired surrogates")
    );
    // Legacy octal escapes are a strict-mode error, and are named as one.
    assert!(refuse(r#"return "\101";"#).message.contains("octal"));
}

/// Nothing to compile, in the four spellings a caller reaches by accident.
#[test]
fn guard_an_empty_program_is_named_not_panicked() {
    for source in ["", "   ", "// nothing\n", "/* nothing */", "\u{feff}"] {
        let error = refuse(source);
        assert!(
            error.message.contains("empty"),
            "{source:?}: {}",
            error.message
        );
        assert!(error.offset <= source.len());
    }
}

/// Line terminators and whitespace outside ASCII. U+2028 ends a line for the
/// grammar exactly as LF does, which is what makes the first of these
/// `undefined` rather than 1.
#[test]
fn guard_exotic_whitespace_and_line_terminators() {
    assert_eq!(run("return\u{2028}1;"), Ok(Value::Undefined));
    number("\u{feff}return 1;", 1.0);
    number("let a = 1;\u{2029}return a;", 1.0);
    number("return 1;\r\n", 1.0);
    // A multi-line comment holding a terminator counts as one (ECMA-262 12.4).
    assert_eq!(run("return /*\n*/ 1;"), Ok(Value::Undefined));
}

/// wasm calls are arity-exact and JavaScript's are not. A surplus argument is
/// still evaluated before it is dropped, and a missing one is `undefined`.
#[test]
fn guard_argument_arity_is_reconciled_without_losing_side_effects() {
    number(
        "function f(x) { return x; } let c = 0; return f(c = 5, c = 7) * 10 + c;",
        57.0,
    );
    number(
        "function f() { return 0; } let c = 0; f(c = 5, c = 7); return c;",
        7.0,
    );
    assert_eq!(
        run("function f(a, b) { return b; } return f(1);"),
        Ok(Value::Undefined)
    );
}

/// Evaluation order, which a lowering that spills through scratch locals can
/// get wrong without producing invalid wasm.
#[test]
fn guard_evaluation_order_inside_one_expression() {
    number("let a = 1; let b = (a = 2) + a; return b;", 4.0);
    number("let a = 0; let b = 0; a = b = 3; return a * 10 + b;", 33.0);
    number("let a = 1; return a++ * 10 + a;", 12.0);
    number("let a = 1; return ++a * 10 + a;", 22.0);
    number("let a = 1; return (a += 2) * 10 + a;", 33.0);
    number("let a = 0; (a = 1) && (a = 2) && (a = 3); return a;", 3.0);
    number("let a = 0; false || (a = 1) || (a = 2); return a;", 1.0);
}

/// A script binding read and written from inside a nested function: the storage
/// is a pair of globals, and the two words have to come off the stack backwards.
#[test]
fn guard_script_bindings_survive_a_nested_frame() {
    number("let x = 1; function f() { x = 2; } f(); return x;", 2.0);
    number(
        "let x = 1; function f() { x = x + 1; return x; } f(); f(); return x;",
        3.0,
    );
    number(
        "var t = 0; function a() { t = t + 1; } function b() { a(); a(); } b(); return t;",
        2.0,
    );
}

/// Instructions after a `return` sit in an unreachable context, where wasm's
/// stack is polymorphic. Emitting a `drop` or a `local.set` there is valid and
/// has to stay valid.
#[test]
fn guard_code_after_return_still_validates() {
    number("return 1; 2;", 1.0);
    number("return 1; return 2;", 1.0);
    number("if (true) { return 1; } return 2;", 1.0);
    number("function f() { return 1; 2; } return f();", 1.0);
    number("while (true) { return 1; 2; }", 1.0);
}

/// The script's completion value (ECMA-262 14.1.1 and the `UpdateEmpty`s in
/// 14.6.7 and 14.7.1.1). A control-flow statement resets it even when its body
/// never runs, which is why the first two of these are `undefined`.
#[test]
fn guard_completion_values() {
    assert_eq!(run("1; if (false) { 2; }"), Ok(Value::Undefined));
    assert_eq!(run("1; while (false) { 2; }"), Ok(Value::Undefined));
    assert_eq!(
        run("1; for (let i = 0; i < 0; i = i + 1) { 2; }"),
        Ok(Value::Undefined)
    );
    number("1; { 2; }", 2.0);
    number("1; let x = 3;", 1.0);
}

/// A host name used at two arities has no single import to be, and a name used
/// at one arity is one import however many times it appears.
#[test]
fn guard_host_import_arity_is_settled_once() {
    let both_ways = |source: &str| {
        tinyvm_qjs::compile_qjs_m1_with(
            source,
            Options {
                names: Names::HostImport,
            },
        )
    };
    assert!(both_ways("return g() + g;").is_ok());
    assert!(both_ways("return g(1) + g(2);").is_ok());
    let error = both_ways("return g(1) + g();").expect_err("two arities cannot be one import");
    assert_eq!(error.boundary, Boundary::ThirdBinding);
    // An import is not a place a value can be put.
    assert!(both_ways("g = 1; return 1;").is_err());
}

/// Runaway recursion is the guest's problem and tinyvm's answer, not a compiler
/// crash: the call depth limit stops it.
#[test]
fn guard_runaway_guest_recursion_is_a_trap_not_a_crash() {
    assert_eq!(
        run("function f(n) { return f(n + 1); } return f(0);"),
        Err("invoke: call depth".to_string())
    );
    number(
        "function f(n) { if (n < 2) { return 1; } return n * f(n - 1); } return f(10);",
        3628800.0,
    );
}

/// A 200 000-byte identifier is a length that reaches the `name` custom section
/// and every LEB128 in the module.
#[test]
fn guard_a_very_long_identifier_encodes() {
    let name = "a".repeat(200_000);
    number(&format!("let {name} = 1; return {name};"), 1.0);
}
