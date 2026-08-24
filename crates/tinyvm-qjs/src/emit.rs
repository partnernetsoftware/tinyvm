//! [`super::ast::Program`] -> [`super::ir::Module`].
//!
//! The whole program becomes one exported function named `main`, taking as many
//! `i32` parameters as the highest `$N` the source names, and returning `i32`.
//! Every host name the source used becomes a zero-argument `js.<name>` import,
//! in sorted order, so the same source always produces the same import table.
//! Nothing here can fail: the parser has already rejected everything this
//! subset cannot lower, and it rejected it with a diagnostic that named the
//! boundary. Once lowering *can* fail (an unresolved name, when the language
//! grows real bindings), this returns a `Result` and the AST grows the spans
//! that diagnostic needs.
//!
//! # A JavaScript divergence we chose, on purpose
//!
//! `/` and `%` lower to `i32.div_s` and `i32.rem_s`, which **trap** when the
//! divisor is zero, and `i32.div_s` also traps on `i32::MIN / -1`. JavaScript
//! has one number type, so `1/0` is `Infinity`, `1%0` is `NaN`, and
//! `-2147483648 / -1` is `2147483648`. M0 has only `i32`: none of those three
//! values exists here, so there is no way to be right, only ways to be wrong.
//!
//! The alternative was to guard every division and yield `0`. That was
//! rejected. A fabricated `0` is a wrong number that flows on into the rest of
//! the expression and is indistinguishable from a real result, which is exactly
//! the silent corruption this whole stack refuses; a trap is loud, arrives as a
//! typed `QjswasmError::Trap`, and costs nothing per division. It is also the
//! divergence that is cheapest to retire: when floats land (M3) these three
//! cases start producing `Infinity`/`NaN`/`2147483648`, and no script can have
//! come to depend on `0` in the meantime, because every such script failed
//! loudly instead of quietly computing the wrong answer.
//!
//! Locked by `div_and_rem_by_zero_trap` and `signed_division_overflow_traps`
//! in `tests/qjs_subset.rs`.

use std::collections::BTreeSet;

use super::ast::{BinaryOp, Expr, Program, UnaryOp};
use super::ir::{Export, ExportKind, Func, FuncType, Import, Ins, Module, ValType};

/// The name every compiled `.qjs` exports. One entry point, fixed, so the host
/// never has to guess what to call.
pub(crate) const ENTRY: &str = "main";

/// The module every host name is imported from. One module name, so the world
/// a guest can see is a single flat table and not a namespace to explore.
pub(crate) const HOST_MODULE: &str = "js";

pub(crate) fn lower(program: &Program) -> Module {
    let mut hosts = BTreeSet::new();
    collect_hosts(&program.expr, &mut hosts);
    let hosts: Vec<String> = hosts.into_iter().collect();

    let mut types: Vec<FuncType> = Vec::new();
    // Import types first: imported functions come first in the index space, so
    // emitting their type first is what keeps a name/ops source byte-identical
    // to what the previous single-pass encoder produced.
    let host_type = if hosts.is_empty() {
        0
    } else {
        intern(
            &mut types,
            FuncType {
                params: Vec::new(),
                results: vec![ValType::I32],
            },
        )
    };
    let arity = argument_count(&program.expr);
    let main_type = intern(
        &mut types,
        FuncType {
            params: vec![ValType::I32; arity as usize],
            results: vec![ValType::I32],
        },
    );

    let mut body = Vec::new();
    emit(&program.expr, &hosts, &mut body);

    Module {
        types,
        imports: hosts
            .iter()
            .map(|name| Import {
                module: HOST_MODULE.to_string(),
                name: name.clone(),
                type_index: host_type,
            })
            .collect(),
        funcs: vec![Func {
            type_index: main_type,
            locals: Vec::new(),
            body,
        }],
        exports: vec![Export {
            name: ENTRY.to_string(),
            kind: ExportKind::Func,
            index: hosts.len() as u32,
        }],
    }
}

/// Index of `wanted` in `types`, appending it first if it is not there. Two
/// functions with the same signature must share one type index; a duplicate
/// entry is legal wasm but is not what a canonical producer emits.
fn intern(types: &mut Vec<FuncType>, wanted: FuncType) -> u32 {
    if let Some(index) = types.iter().position(|t| *t == wanted) {
        return index as u32;
    }
    types.push(wanted);
    (types.len() - 1) as u32
}

/// Every host name the expression mentions. A `BTreeSet` so the import table
/// is sorted and deduplicated: `g+g` imports `js.g` once, and the import order
/// depends on the names rather than on where they appear.
fn collect_hosts(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Int(_) | Expr::Arg(_) => {}
        Expr::Host(name) => {
            out.insert(name.clone());
        }
        Expr::Unary(_, operand) => collect_hosts(operand, out),
        Expr::Binary(_, lhs, rhs) => {
            collect_hosts(lhs, out);
            collect_hosts(rhs, out);
        }
    }
}

/// How many parameters `main` declares: one past the highest `$N` used, so
/// `$2` alone still means three, and a source naming no argument means zero.
fn argument_count(expr: &Expr) -> u32 {
    match expr {
        Expr::Int(_) | Expr::Host(_) => 0,
        Expr::Arg(index) => index + 1,
        Expr::Unary(_, operand) => argument_count(operand),
        Expr::Binary(_, lhs, rhs) => argument_count(lhs).max(argument_count(rhs)),
    }
}

/// Post-order emission: operands first, then the operator. That is the wasm
/// stack discipline, and it is why the tree needs no register allocation.
fn emit(expr: &Expr, hosts: &[String], out: &mut Vec<Ins>) {
    match expr {
        Expr::Int(value) => out.push(Ins::I32Const(*value)),
        // Parameters occupy the first local indices, in order, so `$N` is
        // local `N` with nothing to look up.
        Expr::Arg(index) => out.push(Ins::LocalGet(*index)),
        // Imports occupy the first function indices, in the order of the
        // import section, which is the order of `hosts`.
        Expr::Host(name) => {
            let index = hosts
                .iter()
                .position(|h| h == name)
                .expect("every host name in the tree was collected");
            out.push(Ins::Call(index as u32));
        }
        // wasm has no integer negation instruction. `0 - x` is the standard
        // encoding and is what a wasm producer would emit.
        Expr::Unary(UnaryOp::Neg, operand) => {
            out.push(Ins::I32Const(0));
            emit(operand, hosts, out);
            out.push(Ins::I32Sub);
        }
        Expr::Binary(op, lhs, rhs) => {
            emit(lhs, hosts, out);
            emit(rhs, hosts, out);
            out.push(match op {
                BinaryOp::Add => Ins::I32Add,
                BinaryOp::Sub => Ins::I32Sub,
                BinaryOp::Mul => Ins::I32Mul,
                BinaryOp::Div => Ins::I32DivS,
                BinaryOp::Rem => Ins::I32RemS,
            });
        }
    }
}
