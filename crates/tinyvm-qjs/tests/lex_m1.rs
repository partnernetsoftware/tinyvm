//! The M1 lexer: the tokens that graduated out of `Unsupported`, the string
//! literal reader, and automatic semicolon insertion.
//!
//! `lex` is a private module of the crate, so this file compiles the two
//! modules it needs a second time rather than reaching through the public API.
//! That is deliberate: every assertion here is about the *token stream*, and
//! routing them through `compile_qjs` would only be able to observe whatever
//! the parser currently does with those tokens -- which is the thing that
//! changes underneath a lexer milestone. A one-line `pub mod lex` in `lib.rs`
//! would be tidier and belongs to whoever owns that file.

#![allow(dead_code)]

#[path = "../src/diag.rs"]
mod diag;
#[path = "../src/lex.rs"]
mod lex;

use diag::Boundary;
use lex::{Token, TokenKind, tokenize};

fn lex(source: &str) -> Vec<Token> {
    tokenize(source).unwrap_or_else(|e| panic!("lexing {source:?}: {e}"))
}

/// Every kind in the stream, `Eof` dropped -- the shape most assertions want.
fn kinds(source: &str) -> Vec<TokenKind> {
    let mut kinds: Vec<TokenKind> = lex(source).into_iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds.pop(),
        Some(TokenKind::Eof),
        "stream must end with Eof"
    );
    kinds
}

/// The diagnostic for a source the lexer cannot finish reading.
fn refuse(source: &str) -> String {
    match tokenize(source) {
        Ok(tokens) => panic!(
            "{source:?} lexed to {} tokens; expected a refusal",
            tokens.len()
        ),
        Err(e) => e.message,
    }
}

/// The phrase a token names itself with when the rest of the pipeline cannot
/// lower it yet.
fn capability(source: &str) -> (Boundary, String) {
    let tokens = lex(source);
    let named = tokens
        .iter()
        .find_map(|t| match &t.kind {
            TokenKind::Unsupported(u) => Some(u.clone()),
            _ => None,
        })
        .or_else(|| tokens.iter().find_map(|t| t.kind.capability()))
        .unwrap_or_else(|| panic!("nothing in {source:?} names a capability boundary"));
    (named.boundary, named.phrase)
}

// -- graduated tokens --------------------------------------------------------

#[test]
fn keywords_are_real_tokens() {
    use TokenKind::*;
    let expected = [
        ("function", Function),
        ("return", Return),
        ("if", If),
        ("else", Else),
        ("while", While),
        ("for", For),
        ("let", Let),
        ("const", Const),
        ("var", Var),
        ("typeof", Typeof),
        ("true", True),
        ("false", False),
        ("null", Null),
        ("undefined", Undefined),
    ];
    for (source, kind) in expected {
        assert_eq!(kinds(source), vec![kind], "{source:?}");
    }
}

#[test]
fn a_keyword_is_not_an_identifier_and_an_identifier_is_not_a_keyword() {
    use TokenKind::*;
    // Longest-word matching, not prefix matching.
    assert_eq!(kinds("returned"), vec![Ident("returned".into())]);
    assert_eq!(kinds("iffy"), vec![Ident("iffy".into())]);
    assert_eq!(kinds("trueish"), vec![Ident("trueish".into())]);
    assert_eq!(kinds("_let"), vec![Ident("_let".into())]);
}

#[test]
fn punctuation_is_real_tokens() {
    use TokenKind::*;
    assert_eq!(kinds("{}"), vec![LBrace, RBrace]);
    assert_eq!(
        kinds("(a, b)"),
        vec![LParen, Ident("a".into()), Comma, Ident("b".into()), RParen]
    );
    assert_eq!(kinds("x = 1;"), vec![Ident("x".into()), Eq, Int(1), Semi]);
}

#[test]
fn operators_are_real_tokens() {
    use TokenKind::*;
    let expected = [
        ("==", EqEq),
        ("!=", BangEq),
        ("===", EqEqEq),
        ("!==", BangEqEq),
        ("<", Lt),
        ("<=", LtEq),
        (">", Gt),
        (">=", GtEq),
        ("+", Plus),
        ("-", Minus),
        ("*", Star),
        ("/", Slash),
        ("%", Percent),
        ("!", Bang),
        ("&&", AmpAmp),
        ("||", PipePipe),
        ("++", PlusPlus),
        ("--", MinusMinus),
        ("+=", PlusEq),
        ("-=", MinusEq),
        ("*=", StarEq),
        ("/=", SlashEq),
        ("%=", PercentEq),
    ];
    for (source, kind) in expected {
        assert_eq!(kinds(source), vec![kind], "{source:?}");
    }
}

#[test]
fn the_longest_operator_wins() {
    use TokenKind::*;
    // Each of these is one token, not its prefix followed by the rest.
    assert_eq!(kinds("==="), vec![EqEqEq]);
    assert_eq!(kinds("== ="), vec![EqEq, Eq]);
    assert_eq!(kinds("!=="), vec![BangEqEq]);
    assert_eq!(kinds("<="), vec![LtEq]);
    assert_eq!(kinds("a<-1"), vec![Ident("a".into()), Lt, Minus, Int(1)]);
    // `<<` is still out of subset, and must not read as two `<`.
    assert!(matches!(kinds("<<")[..], [TokenKind::Unsupported(_)]));
    assert_eq!(kinds("++ +"), vec![PlusPlus, Plus]);
}

#[test]
fn the_corpus_that_motivated_this_milestone_lexes_without_one_unsupported_token() {
    // The value-representation experiment could not run this crate's lexer over
    // its corpus: `function`, `{`, `return`, `while`, `=`, `==`, `<` and every
    // string literal came back as `Unsupported`. This is that corpus.
    let sources = [
        "function main() {\n  let a = 7;\n  let b = 3;\n  return (a + b) * (a - b) / 2;\n}",
        "function fib(n) {\n  if (n < 2) { return n; }\n  return fib(n - 1) + fib(n - 2);\n}",
        "function classify(x) {\n  if (x < 0) { return -1; }\n  if (x == 0) { return 0; }\n  return 1;\n}",
        "function main() {\n  let sum = 0;\n  while (sum < 20) { sum = sum + 1; }\n  return sum;\n}",
        "function main() {\n  let s = \"\";\n  while (1) { s = s + \"ab\"; }\n  return s;\n}",
        "function main() {\n  if (k != 3) { total = total + 1; } else { total = total - 1; }\n}",
        "function main() { return len(\"hello\"); }",
    ];
    for source in sources {
        let leftovers: Vec<_> = lex(source)
            .into_iter()
            .filter(|t| matches!(t.kind, TokenKind::Unsupported(_)))
            .collect();
        assert!(
            leftovers.is_empty(),
            "{source:?} still lexes {leftovers:?} as unsupported"
        );
    }
}

// -- string literals ---------------------------------------------------------

fn text(source: &str) -> String {
    match &lex(source)[0].kind {
        TokenKind::Str(s) => s.clone(),
        other => panic!("{source:?} lexed to {other:?}, not a string literal"),
    }
}

#[test]
fn string_literals_carry_their_value() {
    assert_eq!(text("\"hello\""), "hello");
    assert_eq!(text("''"), "");
    assert_eq!(text("'it \"is\" fine'"), "it \"is\" fine");
    assert_eq!(text("\"caf\u{e9} \u{1f600}\""), "caf\u{e9} \u{1f600}");
    // A quote of the other kind is not a terminator.
    assert_eq!(text("\"a'b\""), "a'b");
}

#[test]
fn single_character_escapes() {
    assert_eq!(text(r#""a\nb""#), "a\nb");
    assert_eq!(text(r#""\b\f\n\r\t\v""#), "\u{8}\u{c}\n\r\t\u{b}");
    assert_eq!(text(r#""\\""#), "\\");
    assert_eq!(text(r#""\"""#), "\"");
    assert_eq!(text(r"'\''"), "'");
    assert_eq!(text(r#""\0""#), "\0");
    // NonEscapeCharacter: anything else is itself.
    assert_eq!(text(r#""\q\/""#), "q/");
}

#[test]
fn numeric_escapes() {
    assert_eq!(text(r#""\x41\x7a""#), "Az");
    assert_eq!(text(r#""\u0041\u00e9""#), "A\u{e9}");
    assert_eq!(text(r#""\u{1F600}""#), "\u{1f600}");
    assert_eq!(text(r#""\u{0}""#), "\0");
    // A surrogate pair is one code point, as UTF-16 source would spell it.
    assert_eq!(text(r#""\uD83D\uDE00""#), "\u{1f600}");
}

#[test]
fn a_line_continuation_contributes_nothing() {
    assert_eq!(text("\"a\\\nb\""), "ab");
    assert_eq!(text("\"a\\\r\nb\""), "ab");
    assert_eq!(text("\"a\\\u{2028}b\""), "ab");
}

#[test]
fn a_string_the_engine_cannot_finish_reading_says_what_is_missing() {
    for source in [
        "\"abc",
        "'abc",
        "\"a\nb\"",
        "\"\\x4\"",
        "\"\\u00\"",
        "\"\\u{110000}\"",
    ] {
        let message = refuse(source);
        assert!(
            message.starts_with("this engine "),
            "{source:?} gave {message:?}, which does not speak for the engine"
        );
        let lowered = message.to_lowercase();
        assert!(
            !lowered.contains("syntax error") && !lowered.contains("invalid"),
            "{source:?} gave {message:?}, which is the vague wording this engine forbids"
        );
    }
    assert!(refuse("\"abc").contains("the source ends first"));
    assert!(refuse("\"a\nb\"").contains("the line ends first"));
}

#[test]
fn escapes_beyond_the_subset_name_themselves_and_do_not_leak_into_code() {
    // The lexeme is still consumed whole, so `+ 1` after it is not swallowed.
    let stream = kinds(r#""\uD800" + 1"#);
    assert!(
        matches!(&stream[0], TokenKind::Unsupported(u) if u.phrase.contains("surrogate")),
        "{stream:?}"
    );
    assert_eq!(stream[1..], [TokenKind::Plus, TokenKind::Int(1)]);

    let stream = kinds(r#""\101""#);
    assert!(
        matches!(&stream[0], TokenKind::Unsupported(u) if u.phrase.contains("octal")),
        "{stream:?}"
    );
}

// -- automatic semicolon insertion (ECMA-262 12.10) --------------------------

/// The stream with an `A` marking each semicolon ASI put there, rendered so a
/// failure reads as the token sequence rather than a `Vec<Token>` dump.
fn asi_shape(source: &str) -> String {
    lex(source)
        .iter()
        .map(|t| match (&t.kind, t.inserted) {
            (TokenKind::Semi, true) => "A;".to_string(),
            (TokenKind::Eof, _) => "$".to_string(),
            (kind, _) => format!("{kind:?}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn inserted_count(source: &str) -> usize {
    lex(source).iter().filter(|t| t.inserted).count()
}

#[test]
fn newline_before_records_every_line_terminator_form() {
    let starts_a_line = |source: &str| lex(source)[1].newline_before;
    assert!(starts_a_line("a\nb"));
    assert!(starts_a_line("a\r\nb"));
    assert!(starts_a_line("a\rb"));
    assert!(starts_a_line("a\u{2028}b"));
    assert!(starts_a_line("a\u{2029}b"));
    assert!(starts_a_line("a // done\nb"));
    // ECMA-262 12.4: a multi-line comment containing a line terminator is a
    // line terminator as far as the syntactic grammar is concerned.
    assert!(starts_a_line("a /* \n */ b"));
    assert!(!starts_a_line("a /* one line */ b"));
    assert!(!starts_a_line("a  \t b"));
}

#[test]
fn return_on_its_own_line_gets_its_semicolon() {
    // The trap: this returns undefined, it does not return `x`.
    assert_eq!(asi_shape("return\nx"), "Return A; Ident(\"x\") $");
    // And the semicolon lands on the restricted token, so a diagnostic about
    // it points at the line the author actually wrote.
    let inserted = lex("return\nx").into_iter().find(|t| t.inserted).unwrap();
    assert_eq!(inserted.offset, 7, "the offset of `x`");

    // Same statement, one line: no insertion, `x` is the returned expression.
    assert_eq!(inserted_count("return x"), 0);
    // A comment carrying the line terminator is still a line terminator.
    assert_eq!(inserted_count("return /*\n*/ x"), 1);
    assert_eq!(inserted_count("return /* here */ x"), 0);
    // Already terminated: an inserted semicolon would be an empty statement,
    // which the spec's override forbids.
    assert_eq!(inserted_count("return\n;"), 0);
}

#[test]
fn a_line_that_opens_with_a_bracket_continues_the_statement() {
    // The other half of the trap, and the reason this cannot be "insert a
    // semicolon at every newline": `(` and `[` are allowed by a production
    // here (call, member access), so rule 1 does not fire.
    assert_eq!(inserted_count("a = b\n(c)"), 0);
    assert_eq!(inserted_count("a = b\n[c]"), 0);
    // Nor may an infix operator be cut off from its left operand.
    assert_eq!(inserted_count("a = b\n+ c"), 0);
    assert_eq!(inserted_count("a = b\n. c"), 0);
    assert_eq!(inserted_count("a\n= b"), 0);
}

#[test]
fn the_update_operators_cannot_cross_a_line_terminator() {
    // `a\n++b` is `a; ++b`, never `a++; b`.
    assert_eq!(
        asi_shape("a\n++b"),
        "Ident(\"a\") A; PlusPlus Ident(\"b\") $"
    );
    assert_eq!(
        asi_shape("a\n--b"),
        "Ident(\"a\") A; MinusMinus Ident(\"b\") $"
    );
    // On one line it is the postfix operator and nothing is inserted.
    assert_eq!(inserted_count("a++"), 0);
    assert_eq!(inserted_count("a++\nb"), 0);
    // With no left-hand side before it, `++` is the prefix operator and the
    // restricted production is not in play at all.
    assert_eq!(inserted_count("a =\n++b"), 0);
    assert_eq!(inserted_count("(\n++b)"), 0);
    // `return` wins over the update rule: `return\n++x` returns undefined.
    assert_eq!(
        asi_shape("return\n++x"),
        "Return A; PlusPlus Ident(\"x\") $"
    );
}

#[test]
fn rule_one_and_two_are_offered_to_the_parser_rather_than_guessed_at() {
    // Rule 1 needs "no production allows this token", which only the parser
    // knows. The lexer answers the part that is a fact about the stream.
    let tokens = lex("a = b\nc");
    assert!(
        !lex::semicolon_is_implied(&tokens, 1),
        "`=` on the same line"
    );
    assert!(
        lex::semicolon_is_implied(&tokens, 3),
        "`c` on the next line"
    );
    assert!(
        lex::semicolon_is_implied(&tokens, 4),
        "rule 2: the end of the stream"
    );

    // Rule 1(b): before a `}`, whatever the line layout is.
    let tokens = lex("{ a = b }");
    let brace = tokens
        .iter()
        .position(|t| t.kind == TokenKind::RBrace)
        .unwrap();
    assert!(lex::semicolon_is_implied(&tokens, brace));
}

// -- the anti-drift lock -----------------------------------------------------

#[test]
fn what_is_still_out_of_subset_still_names_itself() {
    let cases = [
        ("2 ** 3", Boundary::Subset, "exponentiation"),
        ("1 & 2", Boundary::Subset, "bitwise operators"),
        ("1 << 2", Boundary::Subset, "bitwise operators"),
        ("1 ? 2 : 3", Boundary::Subset, "conditional expressions"),
        ("`t`", Boundary::Subset, "template literals"),
        // `1.5` stood here until the lexer learned the whole DecimalLiteral
        // grammar; a separator is the numeric form still ahead of it.
        (
            "1_000",
            Boundary::Subset,
            "numeric separators in number literals",
        ),
        ("[1]", Boundary::ThirdBinding, "array literals"),
        ("a.b", Boundary::ThirdBinding, "property access"),
        ("() => 1", Boundary::FullJs, "arrow functions"),
        ("eval", Boundary::FullJs, "the `eval` function"),
        ("class C {}", Boundary::FullJs, "the `class` keyword"),
        ("throw e", Boundary::FullJs, "the `throw` keyword"),
        ("new C", Boundary::FullJs, "the `new` keyword"),
        (
            "a ?? b",
            Boundary::Subset,
            "the nullish coalescing operator",
        ),
    ];
    for (source, boundary, phrase) in cases {
        let got = capability(source);
        assert_eq!(got, (boundary, phrase.to_string()), "{source:?}");
    }
}

#[test]
fn a_graduated_token_still_knows_the_phrase_that_named_it() {
    // Graduating a lexeme moves the capability diagnostic from the lexer to
    // the parser. The wording may not move with it, so it stays here.
    let cases = [
        ("{", Boundary::ThirdBinding, "block statements"),
        ("<", Boundary::Subset, "comparison operators"),
        ("==", Boundary::Subset, "comparison operators"),
        ("=", Boundary::Subset, "assignment"),
        ("+=", Boundary::Subset, "assignment"),
        (",", Boundary::Subset, "the comma operator"),
        ("!", Boundary::Subset, "the logical `!` operator"),
        ("&&", Boundary::Subset, "logical operators"),
        (
            "++",
            Boundary::Subset,
            "the increment and decrement operators",
        ),
        ("\"s\"", Boundary::Subset, "string literals"),
        ("true", Boundary::Subset, "boolean literals"),
        ("null", Boundary::Subset, "the `null` literal"),
        ("undefined", Boundary::FullJs, "the `undefined` value"),
        ("function", Boundary::FullJs, "the `function` keyword"),
        ("let", Boundary::FullJs, "the `let` keyword"),
        ("while", Boundary::FullJs, "the `while` keyword"),
    ];
    for (source, boundary, phrase) in cases {
        assert_eq!(
            capability(source),
            (boundary, phrase.to_string()),
            "{source:?}"
        );
    }

    // The tokens this engine does lower name no boundary at all.
    for source in ["1", "$0", "+", "(", ";", "x"] {
        assert!(
            lex(source)[0].kind.capability().is_none(),
            "{source:?} should not name a capability boundary"
        );
    }
}
