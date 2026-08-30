//! Independent WASM 1.0 MVP goldens.
//!
//! Fixtures live in `tests/fixtures/` (not in `src/wasm.rs`). Expected values
//! were produced by `tests/fixtures/gen_mvp_goldens.py` from the spec, not by
//! running this interpreter. The runner only calls the shipped face:
//! [`tinyvm::eval`] or `Module::from_bytes` + `bind_import` +
//! `Module::eval`.
//!
//! Three fixture files, three jobs:
//! - `mvp_goldens.txt` — one path per MVP opcode (the wiring gate).
//! - `family_extra.txt` — one extra per family, different operands/layout.
//! - `family_edge.txt` — spec boundaries: traps, signed/unsigned splits,
//!   shift-count masks, NaN and signed zero, ties-to-even, memory limits and
//!   data segments, the import table (the semantics gate).
//!
//! Every row must *execute*: there is no empty-module escape hatch, and each
//! row's declared opcode column is checked against the module's own bytes, so
//! coverage cannot be manufactured by editing a text column.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tinyvm::{Val, WasmError, WasmModule, eval};

struct Case {
    id: String,
    family: String,
    opcodes: Vec<u8>,
    expect: Expect,
    wasm: Vec<u8>,
    binds: Vec<(String, String)>,
}

enum Expect {
    I32(i32),
    I64(i64),
    F32Bits(u32),
    F64Bits(u64),
    /// Any NaN: the sign/payload of a hardware-produced NaN is platform
    /// dependent and the spec admits either, so bits are not pinned.
    F32Nan,
    F64Nan,
    Trap,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_files() -> [&'static str; 4] {
    [
        "mvp_goldens.txt",
        "family_extra.txt",
        "family_edge.txt",
        "prd_leaves.txt",
    ]
}

/// `#78|...` is a data row. `# comment` and `# id|family|...` are not.
fn is_fixture_comment(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    rest.starts_with(' ') || rest.starts_with("id|") || !rest.contains('|')
}

fn parse_expect(s: &str) -> Expect {
    if s == "trap" {
        return Expect::Trap;
    }
    if s == "f32nan" {
        return Expect::F32Nan;
    }
    if s == "f64nan" {
        return Expect::F64Nan;
    }
    if let Some(rest) = s.strip_prefix("i32:") {
        return Expect::I32(rest.parse().expect("i32 expect"));
    }
    if let Some(rest) = s.strip_prefix("i64:") {
        return Expect::I64(rest.parse().expect("i64 expect"));
    }
    if let Some(rest) = s.strip_prefix("f32bits:") {
        let bits: u32 = if let Some(h) = rest.strip_prefix("0x") {
            u32::from_str_radix(h, 16).expect("f32 bits")
        } else {
            rest.parse().expect("f32 bits")
        };
        return Expect::F32Bits(bits);
    }
    if let Some(rest) = s.strip_prefix("f64bits:") {
        let bits: u64 = if let Some(h) = rest.strip_prefix("0x") {
            u64::from_str_radix(h, 16).expect("f64 bits")
        } else {
            rest.parse().expect("f64 bits")
        };
        return Expect::F64Bits(bits);
    }
    panic!("unknown expect {s}");
}

fn load_cases(name: &str) -> Vec<Case> {
    let text = fs::read_to_string(fixtures_dir().join(name)).expect(name);
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || is_fixture_comment(line) {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        assert!(
            parts.len() >= 5,
            "{name}:{} expected id|family|opcodes|expect|hex|bind",
            lineno + 1
        );
        let opcodes = parts[2]
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| u8::from_str_radix(s, 16).expect("opcode hex"))
            .collect();
        let binds = parts
            .get(5)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.split('+')
                    .map(|one| {
                        let (m, f) = one.split_once('.').expect("bind module.field");
                        (m.to_string(), f.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let wasm = decode_hex(parts[4]);
        assert!(
            !wasm.is_empty(),
            "{name}:{}: every golden must carry a real module — no empty rows",
            lineno + 1
        );
        out.push(Case {
            id: parts[0].to_string(),
            family: parts[1].to_string(),
            opcodes,
            expect: parse_expect(parts[3]),
            wasm,
            binds,
        });
    }
    out
}

fn decode_hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn must_ok<T>(r: Result<T, WasmError>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {}", e.message()),
    }
}

/// The host side of the import table. Each arm is a plain native closure; the
/// guest reaches it only through a bound import.
fn bind_host(module: &mut WasmModule, m: &str, field: &str) {
    let r = match (m, field) {
        ("host", "mul") => module.bind_import(m, field, |args, _mem| {
            assert_eq!(args.len(), 2);
            Ok(vec![args[0].wrapping_mul(args[1])])
        }),
        ("host", "add19") => module.bind_import(m, field, |args, _mem| {
            assert_eq!(args.len(), 1);
            Ok(vec![args[0].wrapping_add(19)])
        }),
        ("host", "double") => module.bind_import(m, field, |args, _mem| {
            assert_eq!(args.len(), 1);
            Ok(vec![args[0].wrapping_mul(2)])
        }),
        ("host", "plus100") => module.bind_import(m, field, |args, _mem| {
            assert_eq!(args.len(), 1);
            Ok(vec![args[0].wrapping_add(100)])
        }),
        // Writes through the &mut [u8] memory handle the host gate hands out.
        ("host", "poke") => module.bind_import(m, field, |_args, mem| {
            mem[8..12].copy_from_slice(&35i32.to_le_bytes());
            Ok(vec![])
        }),
        other => panic!("unknown host bind {other:?}"),
    };
    must_ok(r, "bind_import");
}

fn run_case(case: &Case) -> Result<Vec<Val>, WasmError> {
    if case.binds.is_empty() {
        return eval(&case.wasm);
    }
    let mut module = WasmModule::from_bytes(&case.wasm)?;
    for (m, f) in &case.binds {
        bind_host(&mut module, m, f);
    }
    module.eval(&[])
}

fn describe_vals(vals: &[Val]) -> String {
    vals.iter()
        .map(|v| match v {
            Val::I32(n) => format!("i32:{n}"),
            Val::I64(n) => format!("i64:{n}"),
            Val::F32(n) => format!("f32bits:{:#x}", n.to_bits()),
            Val::F64(n) => format!("f64bits:{:#x}", n.to_bits()),
            #[cfg(feature = "simd")]
            Val::V128(bytes) => format!("v128:{bytes:02x?}"),
            Val::FuncRef(None) => "funcref:null".to_string(),
            Val::FuncRef(Some(index)) => format!("funcref:{index}"),
            Val::StoreFuncRef(_) => "funcref:store".to_string(),
            Val::ExternRef(None) => "externref:null".to_string(),
            Val::ExternRef(Some(_)) => "externref:host".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn assert_expect(case: &Case, got: Result<Vec<Val>, WasmError>) {
    match (&case.expect, got) {
        (Expect::Trap, Err(WasmError::Trap(_))) => {}
        (Expect::I32(e), Ok(v)) => match v.as_slice() {
            [Val::I32(g)] if g == e => {}
            other => panic!(
                "{}: expected i32 {e}, got {}",
                case.id,
                describe_vals(other)
            ),
        },
        (Expect::I64(e), Ok(v)) => match v.as_slice() {
            [Val::I64(g)] if g == e => {}
            other => panic!(
                "{}: expected i64 {e}, got {}",
                case.id,
                describe_vals(other)
            ),
        },
        (Expect::F32Bits(e), Ok(v)) => match v.as_slice() {
            [Val::F32(g)] if g.to_bits() == *e => {}
            other => panic!(
                "{}: expected f32bits {e:#x}, got {}",
                case.id,
                describe_vals(other)
            ),
        },
        (Expect::F64Bits(e), Ok(v)) => match v.as_slice() {
            [Val::F64(g)] if g.to_bits() == *e => {}
            other => panic!(
                "{}: expected f64bits {e:#x}, got {}",
                case.id,
                describe_vals(other)
            ),
        },
        (Expect::F32Nan, Ok(v)) => match v.as_slice() {
            [Val::F32(g)] if g.is_nan() => {}
            other => panic!(
                "{}: expected an f32 NaN, got {}",
                case.id,
                describe_vals(other)
            ),
        },
        (Expect::F64Nan, Ok(v)) => match v.as_slice() {
            [Val::F64(g)] if g.is_nan() => {}
            other => panic!(
                "{}: expected an f64 NaN, got {}",
                case.id,
                describe_vals(other)
            ),
        },
        (Expect::Trap, Ok(v)) => panic!("{}: expected trap, got {}", case.id, describe_vals(&v)),
        (Expect::Trap, Err(WasmError::Decode(m))) => {
            panic!("{}: expected trap, got decode {m}", case.id)
        }
        (_, Err(e)) => panic!("{}: unexpected {}", case.id, e.message()),
    }
}

// ---------------------------------------------------------------------------
// The opcode column is metadata; the module bytes are the truth. This walks a
// module's code section so coverage is derived from what a row really contains.
// ---------------------------------------------------------------------------

fn leb(bytes: &[u8], i: &mut usize) -> u64 {
    let mut shift = 0;
    let mut out = 0u64;
    loop {
        let b = bytes[*i];
        *i += 1;
        out |= u64::from(b & 0x7F) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            return out;
        }
    }
}

fn skip_leb(bytes: &[u8], i: &mut usize) {
    while bytes[*i] & 0x80 != 0 {
        *i += 1;
    }
    *i += 1;
}

/// Opcode bytes present in a function body, immediates skipped correctly.
fn body_opcodes(body: &[u8], out: &mut BTreeSet<u8>) {
    let mut i = 0usize;
    while i < body.len() {
        let op = body[i];
        i += 1;
        out.insert(op);
        match op {
            0x02..=0x04 => i += 1, // blocktype
            0x0C | 0x0D | 0x10 | 0x20..=0x24 => skip_leb(body, &mut i),
            0x0E => {
                let n = leb(body, &mut i);
                for _ in 0..=n {
                    skip_leb(body, &mut i);
                }
            }
            0x11 => {
                skip_leb(body, &mut i);
                skip_leb(body, &mut i);
            }
            0x28..=0x3E => {
                skip_leb(body, &mut i);
                skip_leb(body, &mut i);
            }
            0x3F | 0x40 => i += 1,
            0x41 | 0x42 => skip_leb(body, &mut i),
            0x43 => i += 4,
            0x44 => i += 8,
            _ => {}
        }
    }
}

/// Every opcode byte in every function body of a module.
fn module_opcodes(wasm: &[u8]) -> BTreeSet<u8> {
    let mut out = BTreeSet::new();
    let mut i = 8usize; // magic + version
    while i < wasm.len() {
        let id = wasm[i];
        i += 1;
        let size = leb(wasm, &mut i) as usize;
        let start = i;
        let end = start + size;
        if id == 10 {
            let payload = &wasm[start..end];
            let mut j = 0usize;
            let count = leb(payload, &mut j);
            for _ in 0..count {
                let body_size = leb(payload, &mut j) as usize;
                let bend = j + body_size;
                let body = &payload[j..bend];
                // locals: vec of (count, valtype)
                let mut k = 0usize;
                let decls = leb(body, &mut k);
                for _ in 0..decls {
                    skip_leb(body, &mut k);
                    k += 1;
                }
                body_opcodes(&body[k..], &mut out);
                j = bend;
            }
        }
        i = end;
    }
    out
}

fn load_opcode_catalog() -> Vec<(String, u8)> {
    let text = fs::read_to_string(fixtures_dir().join("mvp_opcodes.txt")).unwrap();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let name = it.next().unwrap().to_string();
        let byte = u8::from_str_radix(it.next().unwrap(), 16).unwrap();
        out.push((name, byte));
    }
    out
}

#[test]
fn fixtures_live_outside_the_interpreter_source() {
    let src = fixtures_dir();
    for f in fixture_files() {
        assert!(src.join(f).is_file(), "missing fixture {f}");
    }
    let wasm_rs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/wasm.rs");
    let wasm_src = fs::read_to_string(wasm_rs).unwrap();
    for f in fixture_files() {
        assert!(
            !wasm_src.contains(f),
            "independent goldens must not be embedded in src/wasm.rs"
        );
    }
}

#[test]
fn catalog_lists_all_172_mvp_opcodes() {
    let catalog = load_opcode_catalog();
    assert_eq!(catalog.len(), 172, "WASM 1.0 MVP opcode count");
    let mut seen = BTreeSet::new();
    for (name, byte) in &catalog {
        assert!(
            seen.insert(*byte),
            "duplicate opcode byte {byte:#04x} ({name})"
        );
    }
}

/// A row may not claim an opcode its own module does not contain: coverage is
/// read from the bytes, and the column has to agree with them.
#[test]
fn opcode_columns_match_the_module_bytes() {
    let mut bad = Vec::new();
    for name in fixture_files() {
        for case in load_cases(name) {
            let present = module_opcodes(&case.wasm);
            for op in &case.opcodes {
                if !present.contains(op) {
                    bad.push(format!("{} claims {op:02X}, bytes do not have it", case.id));
                }
            }
        }
    }
    assert!(bad.is_empty(), "opcode column lies: {bad:#?}");
}

/// A row that is byte-identical to another adds no signal; the suite grew that
/// way once (store rows cloned from load rows) and must not again.
#[test]
fn no_two_goldens_are_byte_identical() {
    let mut seen: BTreeMap<(String, Vec<u8>), String> = BTreeMap::new();
    let mut dups = Vec::new();
    for name in ["mvp_goldens.txt", "family_extra.txt", "family_edge.txt"] {
        for case in load_cases(name) {
            let key = (format!("{:?}", ExpectKey(&case.expect)), case.wasm.clone());
            if let Some(first) = seen.insert(key, case.id.clone()) {
                dups.push(format!("{first} == {}", case.id));
            }
        }
    }
    assert!(dups.is_empty(), "duplicate goldens: {dups:?}");
}

struct ExpectKey<'a>(&'a Expect);

impl core::fmt::Debug for ExpectKey<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Expect::I32(v) => write!(f, "i32:{v}"),
            Expect::I64(v) => write!(f, "i64:{v}"),
            Expect::F32Bits(v) => write!(f, "f32:{v:#x}"),
            Expect::F64Bits(v) => write!(f, "f64:{v:#x}"),
            Expect::F32Nan => write!(f, "f32nan"),
            Expect::F64Nan => write!(f, "f64nan"),
            Expect::Trap => write!(f, "trap"),
        }
    }
}

#[test]
fn independent_goldens_cover_every_mvp_opcode_via_eval() {
    let catalog = load_opcode_catalog();
    let cases = load_cases("mvp_goldens.txt");
    assert!(cases.len() >= 172, "need a golden path for every opcode");

    // Coverage comes from the decoded module, not from the opcode column.
    let mut covered: BTreeMap<u8, String> = BTreeMap::new();
    for case in &cases {
        let got = run_case(case);
        assert_expect(case, got);
        for op in module_opcodes(&case.wasm) {
            covered.entry(op).or_insert_with(|| case.id.clone());
        }
    }

    let mut missing = Vec::new();
    for (name, byte) in &catalog {
        if !covered.contains_key(byte) {
            missing.push(format!("{name} {byte:02X}"));
        }
    }
    assert!(
        missing.is_empty(),
        "goldens missing {} opcodes: {missing:?}",
        missing.len()
    );
}

#[test]
fn each_family_has_an_extra_independent_golden() {
    let extras = load_cases("family_extra.txt");
    for fam in FAMILIES {
        assert!(
            extras.iter().any(|c| c.family == fam),
            "missing extra golden for family {fam}"
        );
    }
    for case in &extras {
        assert_expect(case, run_case(case));
    }
}

const FAMILIES: [&str; 10] = [
    "control",
    "parametric",
    "locals",
    "memory",
    "i32",
    "i64",
    "f32",
    "f64",
    "conv",
    "host",
];

/// The semantics gate: spec boundaries every family must hold, including the
/// trap conditions and sign/width/rounding rules a wiring-only suite misses.
#[test]
fn spec_edge_goldens_hold() {
    let cases = load_cases("family_edge.txt");
    let mut per_family: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &cases {
        assert_expect(case, run_case(case));
        *per_family.entry(case.family.as_str()).or_default() += 1;
    }
    for fam in FAMILIES {
        let n = per_family.get(fam).copied().unwrap_or(0);
        assert!(n >= 6, "family {fam} has only {n} spec-boundary goldens");
    }
    let traps = cases
        .iter()
        .filter(|c| matches!(c.expect, Expect::Trap))
        .count();
    assert!(traps >= 20, "only {traps} trap goldens; traps are the gate");
}

#[test]
fn host_import_table_bind_and_unbound_are_independent() {
    let cases = load_cases("mvp_goldens.txt");
    let bound = cases.iter().find(|c| c.id == "host.mul").expect("host.mul");
    let unbound = cases
        .iter()
        .find(|c| c.id == "host.unbound")
        .expect("host.unbound");
    assert!(!bound.binds.is_empty());
    assert!(unbound.binds.is_empty());
    assert_expect(bound, run_case(bound));
    assert_expect(unbound, run_case(unbound));

    let mut m = must_ok(WasmModule::from_bytes(&bound.wasm), "from_bytes host.mul");
    assert_eq!(m.imports().len(), 1);
    assert_eq!(m.imports()[0].module, "host");
    assert_eq!(m.imports()[0].field, "mul");
    // Unbound until bind_import: the guest call must trap.
    assert!(matches!(m.eval(&[]), Err(WasmError::Trap(_))));
    // Binding a name that is not in the import table must fail loudly, and
    // must leave the gate shut.
    assert!(m.bind_import("host", "nope", |_, _| Ok(vec![])).is_err());
    assert!(m.bind_import("nope", "mul", |_, _| Ok(vec![])).is_err());
    assert!(matches!(m.eval(&[]), Err(WasmError::Trap(_))));
    bind_host(&mut m, "host", "mul");
    match must_ok(m.eval(&[]), "eval bound mul").as_slice() {
        [Val::I32(221)] => {}
        other => panic!("bound mul expected 221, got {}", describe_vals(other)),
    }
    // A host that fails must propagate its error, not be swallowed.
    let mut fails = must_ok(WasmModule::from_bytes(&bound.wasm), "from_bytes");
    must_ok(
        fails.bind_import("host", "mul", |_, _| Err(WasmError::Trap("host said no"))),
        "bind failing host",
    );
    assert!(matches!(fails.eval(&[]), Err(WasmError::Trap(_))));

    // Two imports: the function index space puts them first, so a defined
    // function's combined index is shifted by the import count.
    let edges = load_cases("family_edge.txt");
    let shift = edges
        .iter()
        .find(|c| c.id == "edge.host.index_shift_defined_func")
        .expect("index shift golden");
    let m2 = must_ok(WasmModule::from_bytes(&shift.wasm), "from_bytes shift");
    assert_eq!(m2.imports().len(), 2);
    assert_eq!(m2.export_index("main"), Some(3));
    assert_expect(shift, run_case(shift));
}

fn prd_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../prd/PRD.md")
}

/// `[x]` node tokens inside the first fenced tree of the PRD.
fn parse_prd_x_leaves(prd: &str) -> Vec<String> {
    parse_prd_leaves(prd, "[x]")
}

/// Node tokens carrying `marker` inside the PRD's fenced trees. The body is
/// the `[x]` parser's, verbatim, with the marker parameterised: `LEAF_TESTS`
/// keys are exact whole-line tokens, so any drift in the normalisation here
/// would turn every forward mapping red at once.
fn parse_prd_leaves(prd: &str, marker: &str) -> Vec<String> {
    let mut in_fence = false;
    let mut leaves = Vec::new();
    for line in prd.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence || !line.contains(marker) {
            continue;
        }
        let token: String = line
            .replace(marker, "")
            .chars()
            .map(|c| match c {
                '│' | '├' | '└' | '─' | '|' => ' ',
                _ => c,
            })
            .collect();
        let token = token.trim();
        if !token.is_empty() {
            leaves.push(token.to_string());
        }
    }
    leaves
}

/// Leaves whose backing is a test rather than a fixture family. The mapped
/// test must exist in this package's integration tests and assert something
/// concrete — the point of naming it here is that a leaf can no longer be
/// satisfied by a text row.
// 28 leaves added 2026-08-29, in one batch, and that batch size is the finding
// rather than the work. This canary requires every `[x]` in the capability
// tree to name a test that runs, and it went unrun for days of PRD edits
// because the verification anybody actually typed was `cargo test -p
// tinyvm-qjs` -- a package this test does not live in. The claims were true;
// nothing was checking that they were.
//
// `prd/PRD.md`'s acceptance section now writes the command set down, with this
// package in it, so the next gap is caught by the gate rather than by someone
// deciding to look.
const LEAF_TESTS: [(&str, &str); 272] = [
    (
        "位运算 `&   ^ ~ << >> >>>` 与六个复合赋值：ToInt32 一个 prefab，逐运算符门控  2026-08-31；每个运算符 ~170 B、45 步（2^31 外 65）；`xor16` 手写 16 轮 14.5M → `^` 267k 步（54×）",
        "the_six_operators_on_small_integers",
    ),
    (
        "`JSON.stringify(v, null, space)`：数 1..10 / 串前 10 码元，25.5.2 原样  2026-08-31；JSON 程序 +501 B；50 个小对象 space=2 281 965 步（61/输出字节），紧凑路 178 202 不动；replacer 仍具名拒绝",
        "a_number_space_indents_by_that_many_spaces",
    ),
    (
        "`charCodeAt(i)`：UTF-16 位置，代理项的半边是个数，照答      2026-08-31；733 B；ASCII 八字节一步，1 000 字符第 999 位 2 126 步",
        "char_code_at_and_char_at_read_utf16_positions",
    ),
    (
        "`charAt(i)` / `substring(a[, b])`：与 slice 共一个码元核心     2026-08-31；869 / 1 026 / 909 B；落在代理对中间 = slice 的同一句拒绝",
        "substring_clamps_and_swaps",
    ),
    (
        "`s[i]`（只读）：键是整数就是那一码元，越界 undefined       2026-08-31；768 B；门是「文本定不了的计算键读」，`o[k]` 也付（+716，记在账上）",
        "a_string_indexes_by_code_unit",
    ),
    (
        "`sort([cmp])`：自底向上归并，稳定、原地、返回自身，undefined 殿后  2026-08-31；948 / 1 116 B；1 000 个数 + 比较器 1.71M 步，1 000 个串默认序 4.21M 步（手写归并 13.8M，3.3×）",
        "sort_is_stable_in_place_and_returns_the_receiver",
    ),
    (
        "`Array.isArray(x)`：折成 `x.__is_array()`，任意接收者   2026-08-31；59 B；`rh_compat` 曾手写「有数字 length 的对象」",
        "is_array_answers_the_tag_and_nothing_else",
    ),
    (
        "数组 `indexOf` / `includes`：与 String 同名，一个 prefab 按 tag 分派  2026-08-31；702 / 623 B；未命中 52 / 55 步/元素；String 调用者 +221 B",
        "array_index_of_and_includes_compare_strictly",
    ),
    (
        "`concat(x[, y])`：数组展开一层，其它原样追加         2026-08-31；569 / 639 B；28 步/元素",
        "concat_spreads_arrays_and_appends_everything_else",
    ),
    (
        "`join([sep])`：undefined/null 为空，对象元素具名拒绝   2026-08-31；871 / 885 B；两趟一次分配，10 字节元素 236 步/个",
        "join_writes_every_element_with_the_separator_between",
    ),
    (
        "`break` / `continue`                           见下方方法段的同名行",
        "break_leaves_the_loop",
    ),
    (
        "split  · toLowerCase  · toUpperCase      [–] 前两个已落地，后者零使用",
        "split_cuts_at_every_separator",
    ),
    (
        "`for … of` over an array (13.7.5)              折成索引循环，无新节点",
        "it_visits_every_element_in_order",
    ),
    (
        "元素每轮是新绑定                             白拿：声明在循环体里",
        "each_pass_binds_a_new_element_so_closures_do_not_share",
    ),
    (
        "length 每轮重读，body 里 pop 会被看见         与数组迭代器一致",
        "the_length_is_read_each_pass_rather_than_cached",
    ),
    (
        "字符串 / 非数组按名拒绝，可 catch             不静默跑零轮",
        "a_non_array_is_refused_rather_than_silently_iterated_zero_times",
    ),
    (
        "模块：`import * as` + `export`（16.2）          编译期取入，仍是一个 .wasm",
        "an_exported_function_is_reachable_through_the_alias",
    ),
    (
        "宿主给解析回调，编译器不碰文件系统           与「核只吃字节」同一条纪律",
        "an_unresolvable_specifier_is_named",
    ),
    (
        "命名空间双向不漏（模块↔导入者）              判据 ③，两条测试",
        "an_unexported_name_is_not_visible_to_the_importer",
    ),
    (
        "循环导入点名拒绝，不是栈溢出                 判据 ④",
        "a_cycle_is_refused_and_both_specifiers_are_named",
    ),
    (
        "无 import 的程序零字节                       判据 ②，与 closures 同一组数",
        "a_program_without_imports_pays_nothing_for_them",
    ),
    (
        "一个声明执行两次 = 两个绑定 (14.3.1)         见下",
        "a_let_declared_in_a_loop_body_is_a_new_binding_each_pass",
    ),
    (
        "函数内的 let / const（含 while / 嵌套块）  012，cell 移到声明处",
        "a_while_body_gets_the_same_treatment_because_the_rule_is_about_declarations",
    ),
    (
        "脚本层的 let / const                      012，循环内的改走捕获",
        "the_same_holds_at_script_level_where_the_storage_differs",
    ),
    (
        "不在循环里的脚本绑定仍是 global        判据 ④：不许为它涨字节",
        "a_script_binding_read_from_a_function_outside_a_loop_still_works",
    ),
    (
        "`for` 头部的 let 每轮复制 (13.7.4.7)       012，body 之后 update 之前",
        "the_for_header_binding_is_fresh_each_pass",
    ),
    (
        "`while` 闭包看到末值                  **对照**：333 是正确答案",
        "a_while_closing_over_an_outer_variable_still_sees_the_last_value",
    ),
    (
        "每多一个循环内被捕获的绑定                +83 字节（斜率，已写成测试）",
        "what_a_per_iteration_binding_costs_is_written_down",
    ),
    (
        "includes / startsWith / endsWith                需求普查前两名",
        "includes_answers_whether_the_substring_occurs",
    ),
    (
        "split（非空分隔符）+ 共享的 substr 辅助件        426 字节",
        "split_cuts_at_every_separator",
    ),
    (
        "toLowerCase（Unicode 区间表）                    **8 836 字节**，价目公开",
        "caf_uppercase_lowercases_correctly",
    ),
    (
        "不调用它的程序零字节                         表在门后",
        "a_program_that_never_lowercases_carries_none_of_it",
    ),
    (
        "中文 / emoji / 已小写：原样返回不 trap        判据 ②",
        "text_with_no_case_passes_through_untouched",
    ),
    (
        "replace（首个）/ replaceAll（全部）              525 + 515 字节，各发射一份",
        "replace_is_first_only_and_replace_all_is_every_one",
    ),
    (
        "`break` / `continue`（无标签）                   continue 自带标签，按需发射",
        "break_leaves_the_loop",
    ),
    (
        "第四类 fault code：运行期能力边界                  7 字节，只有到那条臂的程序付",
        "a_missing_string_property_reports_a_capability_boundary",
    ),
    (
        "`Number(x)`：折成 `+x`，零运行时                  缺的只是名字，转换早就有",
        "number_converts_the_way_unary_plus_does",
    ),
    (
        "空片段保留（前导 / 尾随 / 连续分隔符）       最容易漏掉的边界",
        "empty_pieces_are_kept",
    ),
    (
        "字节层比较，对多字节字符精确                é 与 😀 钉住",
        "multi_byte_characters_match_as_characters",
    ),
    (
        "includes 不经由 indexOf，因此更便宜          320 vs 440 字节",
        "includes_is_cheaper_than_index_of_because_it_needs_no_position",
    ),
    (
        "arrays: the eighth tag, a dense vector",
        "an_array_literal_holds_its_elements_in_source_order",
    ),
    (
        "literal, a[i], .length, nesting",
        "length_is_reachable_under_both_spellings",
    ),
    (
        "out of range reads undefined, not a fault",
        "an_index_past_the_end_is_undefined_and_not_a_fault",
    ),
    (
        "a write past the end fills, no holes",
        "a_write_past_the_end_extends_the_array_with_undefined",
    ),
    ("JSON reads and writes one", "json_parse_builds_an_array"),
    (
        "methods: push / pop / map                  see below",
        "push_mutates_the_receiver_and_returns_the_new_length",
    ),
    (
        "an array-free program pays nothing for arrays  9 784 -> 9 784 bytes",
        "a_program_with_no_array_and_no_json_is_byte_identical_to_what_it_was",
    ),
    (
        "an indexed read is 36.6x the object spelling   526 vs 19 235 steps",
        "an_indexed_read_costs_what_the_eighth_tag_was_chosen_for",
    ),
    (
        "closures that capture an outer local           by binding, gated",
        "a_nested_function_reads_an_enclosing_local",
    ),
    (
        "a write after the closure exists is seen    not by value",
        "a_write_after_the_closure_exists_is_visible_through_it",
    ),
    (
        "parameters count; any nesting depth         flat closures",
        "captures_work_under_the_declared_names_mode_too",
    ),
    (
        "two instances, two environments             identity, observable",
        "two_instances_of_one_function_expression_have_separate_environments",
    ),
    (
        "a no-capture program pays nothing           21 fixed / 99 each",
        "a_program_with_no_capture_is_byte_identical_to_what_it_was",
    ),
    (
        "the whole DecimalLiteral grammar (12.9.3)",
        "the_decimal_literal_grammar_is_read_whole",
    ),
    (
        "1.5 · .5 · 1. · 1e3 · 2E2 · 1.5e-3",
        "the_decimal_literal_grammar_is_read_whole",
    ),
    (
        "integers past i32 and past 2^53             nearest double",
        "the_decimal_literal_grammar_is_read_whole",
    ),
    (
        "template literals (13.2.8)                      folded to `+`",
        "a_template_without_substitutions_is_the_string_it_spells",
    ),
    (
        "nesting; any expression in a substitution   brace-depth stack",
        "templates_nest",
    ),
    (
        "TV normalises CRLF and lone CR to one LF    12.9.6",
        "the_tv_normalises_line_terminators_to_one_lf",
    ),
    (
        "a template-free program pays nothing        byte-identical",
        "a_template_free_program_pays_nothing_for_this_milestone",
    ),
    (
        "arrow functions (15.3)                          = a function expression",
        "an_arrow_and_the_function_expression_it_means_are_one_module",
    ),
    (
        "both parameter forms, both body forms",
        "a_parenthesised_parameter_list_works_like_a_functions",
    ),
    (
        "the cover grammar, settled before parsing   13.2.2",
        "a_parenthesised_expression_is_still_one",
    ),
    (
        "an arrow-free program pays no bytes         and no compile time",
        "an_arrow_free_program_pays_nothing_for_this_milestone",
    ),
    (
        r#"`"ab".length`                                   UTF-16 code units"#,
        "length_counts_utf16_code_units_and_not_bytes",
    ),
    (
        "counts units, not UTF-8 bytes               café is 4",
        "length_counts_utf16_code_units_and_not_bytes",
    ),
    (
        "every other String property still traps     deliberate",
        "every_other_property_of_a_string_still_traps",
    ),
    (
        "a program without `.length` got smaller     -19 bytes",
        "a_program_that_never_names_length_pays_nothing",
    ),
    (
        "methods: trim indexOf push pop map              binding **measured**",
        "trim_removes_whitespace_from_both_ends",
    ),
    (
        "the mechanism was decided by experiment     research/method-binding",
        "an_unknown_member_is_still_refused_the_way_its_receiver_refuses_it",
    ),
    (
        "trim covers all of Zs + LineTerminator      12.2 + 12.3",
        "trim_removes_whitespace_from_both_ends",
    ),
    (
        "indexOf positions agree with .length        UTF-16 units",
        "index_of_finds_a_substring_by_code_unit_position",
    ),
    (
        "map calls back into a function value        a prefab **can**",
        "map_calls_back_into_a_function_value",
    ),
    (
        "a plain object's same-named property wins   run-time receiver",
        "a_plain_object_property_named_like_a_method_is_untouched",
    ),
    (
        "adding a method costs non-callers nothing   per-method gate",
        "length_is_still_a_value_and_not_a_method",
    ),
    ("eval(bytes)", "eval_bytes"),
    (
        "in-guest throughput gate",
        "interpreter_throughput_reports_nanoseconds_per_guest_instruction",
    ),
    (
        "ns per guest instruction, eight shapes",
        "interpreter_throughput_reports_nanoseconds_per_guest_instruction",
    ),
    (
        "WABT agrees before any timing is believed",
        "interpreter_throughput_reports_nanoseconds_per_guest_instruction",
    ),
    (
        "eval_wasm(data, globals, locals)",
        "eval_wasm_sends_globals_and_locals_to_the_host_door",
    ),
    (
        "eval / eval_with aliases",
        "eval_and_eval_with_remain_callable_aliases",
    ),
    (
        "language skin (tinyvm-qjs)",
        "language_skin_is_qjs2wasm_over_eval_wasm",
    ),
    (
        "qjs2wasm names / ops / host-call subset",
        "qjs2wasm_names_ops_host_call",
    ),
    (
        "eval_qjs = eval_wasm(qjs2wasm, globals, locals)",
        "eval_qjs_is_qjs2wasm_then_eval_wasm",
    ),
    (
        "commissar demo (example commissar)",
        "commissar_demo_eval_wasm_and_sugar",
    ),
    (
        "V1 values across the call boundary",
        "qjs_m1_moves_v1_values_across_the_call_boundary",
    ),
    (
        "declarations, functions, control flow",
        "qjs_m1_lowers_declarations_functions_and_control_flow",
    ),
    (
        "host calls with declared raw signatures",
        "qjs_m1_reaches_a_declared_host_door_with_arguments",
    ),
    (
        "object literals, property access, assignment",
        "qjs_m1_builds_objects_and_reads_their_properties",
    ),
    (
        "functions as values, stored/passed/called",
        "qjs_m1_stores_passes_and_calls_a_function_value",
    ),
    (
        "Number<->String conversion, per ECMA-262",
        "qjs_m1_converts_between_numbers_and_strings",
    ),
    (
        "conditional expressions, try/catch/finally",
        "qjs_m1_lowers_a_conditional_and_a_try",
    ),
    (
        "JSON.parse / JSON.stringify, per ECMA-262 25.5",
        "qjs_m1_parses_and_prints_json",
    ),
    (
        "an uncaught throw is legible, not a bare trap",
        "qjs_m1_tells_an_uncaught_throw_from_a_broken_script",
    ),
    (
        "the acceptance library runs through a host door",
        "qjs_m1_runs_a_fleet_wrapper_through_a_declared_host_door",
    ),
    (
        "a host length answer must be a length",
        "qjs_m1_refuses_a_host_length_that_is_not_a_length",
    ),
    (
        "nesting bounded by a diagnostic, not an abort",
        "qjs_m1_bounds_nesting_with_a_diagnostic_not_an_abort",
    ),
    (
        "every rejection names the engine boundary",
        "qjs_m1_rejections_name_the_engine_boundary",
    ),
    (
        "an exhausted heap is legible, not a bare trap",
        "qjs_m1_tells_an_exhausted_heap_from_a_broken_script",
    ),
    (
        "`Object.keys(o)`：折成 `o.__keys()`，门控 prefab   208 字节；迁移语料 12 处",
        "keys_come_back_in_insertion_order",
    ),
    (
        "a non-index property write is a named refusal  fault 10, `refused_operations.rs`",
        "a_refused_write_says_what_was_written",
    ),
    (
        "第三种接收者：调用点原只分「数组 / 否则字符串」  对象接收者曾落进 String 拒绝",
        "it_reads_the_way_the_corpus_uses_it",
    ),
    (
        "the thrown String itself is host-readable (`FAULT_THROWN` pointer, `guest_thrown_message`)  94237cb；下游 agenterm `2cde8b63` 打印它",
        "a_thrown_string_is_readable_from_the_host",
    ),
    (
        "a missing String property names itself at run time (`FAULT_MISSING_STRING_METHOD`), not a bare trap  2026-08-29；`slice`/`substr`/`substring` 曾是三个「不同的 bug」",
        "a_missing_string_method_names_itself",
    ),
    (
        "`slice(start[, end])`：码元位置、负索引、NaN=0、共享核心              2026-08-29；756 B / 647 B / 两者 1 029 B；代理对内的边界 trap",
        "positions_are_utf16_code_units",
    ),
    (
        "a host argument of the wrong type names the call and the position (`FAULT_HOST_ARGUMENT`)  2026-08-29；字面量 String 参数不再带标签测试",
        "a_number_where_a_string_is_declared_names_the_call_and_the_argument",
    ),
    (
        "`0x`/`0o`/`0b` number literals（无位数或超 64 位具名拒绝）  2026-08-29；Win32 常量不必再写十进制",
        "the_three_radices_answer",
    ),
    (
        "reserved words as property names（`o.class`、`{ do: 1 }`）  2026-08-29；`.` 后与 `:` 前是 IdentifierName",
        "reserved_words_name_properties_after_a_dot_and_in_a_literal",
    ),
    (
        "a property read off undefined/null/a primitive names the key (`FAULT_PROPERTY_OF_NON_OBJECT`)  2026-08-29；仍不可捕获（A8），但不再哑",
        "undefined_and_null_name_the_key",
    ),
    (
        "inside a `try`, that read is a catchable TypeError (a String)  2026-08-29；`try` 本身就开 unwind 通道；无 `try` 仍是具名 fault",
        "inside_a_try_the_read_is_a_catchable_type_error_naming_the_key",
    ),
    (
        "a real App target consumes the XCFramework (xcodegen, both destinations)  2026-08-30 验收 #5",
        "the_ios_smoke_builds_the_app_target_for_both_destinations",
    ),
    ("iOS runtime boundary", "native_interpreter_boundary"),
    ("interpret wasm", "eval_bytes"),
    ("JIT native code", "native_interpreter_boundary"),
    ("device-side AOT", "native_interpreter_boundary"),
    ("dyn native loading", "native_interpreter_boundary"),
    ("tinyvm engine", "eval_bytes"),
    (
        "game runtime",
        "standard_wasm_cartridge_drives_one_bounded_frame",
    ),
    (
        "game ABI",
        "manifest_capabilities_and_lifecycle_signatures_are_exact",
    ),
    (
        "core v1 imports",
        "core_v1_media_versions_are_explicit_and_format_matched",
    ),
    (
        "strict declared-memory semantics",
        "standard_bytes_require_declared_memory",
    ),
    (
        "strict memarg alignment",
        "standard_memarg_alignment_is_validated_at_load",
    ),
    (
        "canonical function expression structure",
        "standard_function_expression_structure_is_canonical",
    ),
    (
        "a refusal names the function, its name, its source line and column  `from_bytes_explained`：`name` 段 + `qjs.lines` 段（行 + UTF-16 列），静态核不读",
        "a_lines_section_puts_the_source_line_on_the_refusal",
    ),
    (
        "strict i64 signed-LEB range",
        "standard_i64_leb_rejects_invalid_unused_high_bits",
    ),
    (
        "valid custom-section names",
        "standard_custom_section_name_is_validated_while_opaque_payload_stays_ignored",
    ),
    (
        "empty memory-section vector",
        "standard_bytes_require_declared_memory",
    ),
    (
        "mutable global.set target",
        "standard_global_set_requires_a_mutable_declaration",
    ),
    (
        "strict untyped select value domain",
        "standard_untyped_select_rejects_reference_values",
    ),
    (
        "export-declared ref.func",
        "standard_ref_func_declarations_include_function_exports",
    ),
    (
        "imported-global element expressions",
        "wabt_element_expressions_preserve_imported_externref",
    ),
    (
        "WABT load-gate oracle",
        "wabt_oracle_fixture_exactly_matches_the_rust_load_gate",
    ),
    ("stable two-pass copy lengths", "ios_xcframework_swift_link"),
    ("scoped immutable frame views", "ios_xcframework_swift_link"),
    ("single-buffer RGBA expansion", "ios_xcframework_swift_link"),
    ("grid3d presentation", "ios_xcframework_swift_link"),
    (
        "allocation-free typed cell iteration",
        "ios_xcframework_swift_link",
    ),
    (
        "bounded multi-source aggregation",
        "ios_xcframework_swift_link",
    ),
    (
        "Apple keyboard/gamepad adapter",
        "ios_xcframework_swift_link",
    ),
    (
        "overlapping keyboard alias retention",
        "ios_xcframework_swift_link",
    ),
    (
        "real App rising-edge input behavior",
        "current_main_runtime_runs_in_real_nostalgia_app_target",
    ),
    (
        "real iOS app consumer",
        "current_main_runtime_runs_in_real_nostalgia_app_target",
    ),
    (
        "static module validation CLI",
        "module_validate_cli_is_static_and_rejects_invalid_bytes",
    ),
    (
        "bulk memory copy/fill",
        "standard_bulk_memory_copy_fill_execute_with_wasm_semantics",
    ),
    (
        "bulk memory passive lifecycle",
        "standard_bulk_memory_proposal_executes_with_instance_semantics",
    ),
    (
        "sign extension proposal",
        "standard_sign_extension_proposal_executes",
    ),
    (
        "nontrapping float-to-int",
        "standard_nontrapping_conversion_proposal_saturates",
    ),
    (
        "multi-value proposal",
        "standard_multi_value_proposal_executes",
    ),
    (
        "single-table funcref profile",
        "standard_funcref_table_profile_executes_with_instance_semantics",
    ),
    (
        "multiple defined funcref tables",
        "standard_multiple_funcref_tables_execute_and_share_one_host_budget",
    ),
    (
        "multiple internally defined memories",
        "wabt_compiled_multi_memory_matches_tinyvm",
    ),
    (
        "extended constant expressions",
        "standard_extended_const_executes_and_rejects_invalid_expression_stacks",
    ),
    (
        "standard imported globals",
        "standard_imported_globals_bind_types_and_share_mutation",
    ),
    (
        "named standard resource exports",
        "standard_resource_exports_are_resolved_by_name",
    ),
    (
        "standard imported linear memories",
        "standard_imported_memory_binds_limits_and_shares_store_identity",
    ),
    (
        "store-owned imported funcref tables",
        "wabt_compiled_imported_table_decodes_in_standard_index_space",
    ),
    (
        "linked exported globals",
        "wabt_compiled_imported_globals_match_tinyvm",
    ),
    (
        "linked exported memories",
        "wabt_compiled_imported_memory_matches_tinyvm",
    ),
    (
        "linked exported tables",
        "wabt_compiled_imported_table_decodes_in_standard_index_space",
    ),
    (
        "linked exported functions",
        "wabt_compiled_exported_functions_link_across_instances",
    ),
    (
        "numeric value signatures",
        "wabt_compiled_exported_functions_link_across_instances",
    ),
    (
        "store-owned funcref values",
        "wabt_compiled_exported_functions_link_across_instances",
    ),
    (
        "opaque externref function/global values",
        "standard_externref_function_and_global_values_preserve_host_identity",
    ),
    (
        "standard externref tables",
        "wabt_compiled_externref_tables_preserve_host_identity",
    ),
    (
        "tail-call proposal",
        "standard_tail_calls_trampoline_across_direct_indirect_and_host_targets",
    ),
    (
        "decode complexity budget",
        "tiny_declared_count_bombs_fail_before_allocation",
    ),
    (
        "typed standard function imports",
        "standard_typed_host_imports_preserve_all_value_kinds",
    ),
    ("H5/JS/WKWebView", "native_interpreter_boundary"),
    (
        "persistent instance",
        "instance_preserves_globals_but_module_calls_stay_fresh",
    ),
    (
        "explicit guest call stack",
        "guest_call_stack_is_explicit_bounded_and_native_stack_independent",
    ),
    (
        "host-owned call-depth ceiling",
        "call_stack_limits_are_host_owned_and_fail_at_exact_boundaries",
    ),
    (
        "host-owned activation-slot ceiling",
        "call_stack_limits_are_host_owned_and_fail_at_exact_boundaries",
    ),
    (
        "fallible execution-stack growth",
        "operand_and_control_growth_are_preflighted_at_host_slot_boundary",
    ),
    (
        "one trap message per ceiling",
        "each_execution_ceiling_reports_its_own_message",
    ),
    (
        "bounds-checked guest memory windows",
        "guest_windows_are_bounds_checked_over_the_whole_boundary",
    ),
    (
        "two-pass variable-length host result",
        "a_guest_collects_a_variable_length_host_result_in_two_passes",
    ),
    (
        "string-free fault classification",
        "every_host_configured_ceiling_names_its_own_limits_field",
    ),
    (
        "every allocator refusal reads Allocation",
        "every_allocation_call_site_produces_a_fault_the_classifier_calls_allocation",
    ),
    ("start once", "instance_runs_start_exactly_once"),
    (
        "per-call fuel",
        "instruction_budget_is_host_owned_and_resets_per_call",
    ),
    (
        "memory budget",
        "memory_budget_rejects_initial_min_and_caps_grow",
    ),
    (
        "table budget",
        "table_budget_follows_host_not_crate_constant",
    ),
    (
        "deterministic execution stats",
        "execution_stats_are_deterministic_and_cover_guest_host_resources",
    ),
    (
        "call/activation peak telemetry",
        "execution_stats_are_deterministic_and_cover_guest_host_resources",
    ),
    (
        "bounded in-place host dispatch",
        "in_place_native_module_receives_exact_bounded_result_slice",
    ),
    (
        "standard .wasm cartridge",
        "standard_wasm_cartridge_drives_one_bounded_frame",
    ),
    (
        "manifest compatibility",
        "manifest_capabilities_and_lifecycle_signatures_are_exact",
    ),
    (
        "init/tick/suspend/resume",
        "suspend_resume_restores_guest_state_and_host_rng",
    ),
    (
        "portable state snapshot",
        "snapshot_identity_schema_and_bounds_fail_closed",
    ),
    (
        "protected prepublication snapshot replace",
        "ios_xcframework_swift_link",
    ),
    (
        "bounded prepared slot + borrowed restore slice",
        "ios_xcframework_swift_link",
    ),
    (
        "bounded frame output",
        "standard_core_only_cartridge_drives_an_indexed2d_frame",
    ),
    (
        "bounded app metadata hot path",
        "indexed2d_metadata_requires_an_explicit_core_capability",
    ),
    (
        "recyclable host buffers",
        "tick_into_recycles_bounded_frame_storage",
    ),
    (
        "native module registry",
        "native_dispatch_quota_is_charged_before_callback_and_resets_per_lifecycle",
    ),
    (
        "atomic resource-table factory",
        "native_module_can_own_a_resource_behind_a_generation_checked_guest_handle",
    ),
    ("App Store bundled-only gate", "ios_xcframework_swift_link"),
    (
        "machine host profile",
        "host_profile_is_canonical_and_checks_exact_standard_imports",
    ),
    (
        "exact zero-budget channel semantics",
        "zero_game_output_limits_round_trip_and_disable_each_channel",
    ),
    (
        "profile-bound descriptor return",
        "ios_xcframework_swift_link",
    ),
    (
        "typed compatibility issue report",
        "ios_xcframework_swift_link",
    ),
    (
        "exact-build Wasm feature negotiation",
        "exact_host_profile_reports_simd_subset_mismatch_without_execution",
    ),
    (
        "metadata schema diagnostics",
        "signal_lock_converter_reports_bounded_application_metadata",
    ),
    (
        "indexed2d metadata extension",
        "ordinary_c_toolchain_emits_a_portable_standard_cartridge",
    ),
    ("catalog profile binding", "ios_xcframework_swift_link"),
    (
        "Depth Well grid3d",
        "standard_depth_well_plays_and_restores_deterministically",
    ),
    (
        "Paddle Guard indexed2d",
        "standard_paddle_guard_launches_moves_and_emits_indexed_frames",
    ),
    (
        "Signal Lock Swift-to-Wasm migration",
        "standard_signal_lock_rotates_channels_and_renders_a_readable_radar",
    ),
    (
        "deterministic replay vectors",
        "depth_well_replay_is_portable_bounded_and_tamper_evident",
    ),
    (
        "development JSC + H5 differential",
        "webkit_matches_tinyvm_replay",
    ),
    (
        "contract / abstraction / backend split",
        "unsupported_platform_operations_fail_explicitly",
    ),
    (
        "internal handles + preopen-only paths",
        "preopens_keep_physical_paths_out_of_guest_space",
    ),
    (
        "optional WASI Preview 1 adapter",
        "wasi_p1_complete_subset_binds_all_exact_signatures",
    ),
    (
        "args + environ",
        "wasi_p1_process_clock_random_preopen_and_close_execute_through_standard_imports",
    ),
    (
        "clock + random",
        "wasi_p1_process_clock_random_preopen_and_close_execute_through_standard_imports",
    ),
    (
        "preopen discovery + fd_close",
        "wasi_p1_process_clock_random_preopen_and_close_execute_through_standard_imports",
    ),
    (
        "fd read/write/seek/stat",
        "wasi_p1_fd_io_seek_and_stat_use_guest_descriptors_and_standard_layouts",
    ),
    (
        "path open/unlink",
        "wasi_p1_path_open_and_unlink_stay_relative_to_a_virtual_preopen",
    ),
    (
        "proc_exit",
        "wasi_p1_proc_exit_is_non_returning_and_exposes_the_typed_code",
    ),
    (
        "capability-directory std backend",
        "standard_wasi_module_reaches_the_real_preopen_backend",
    ),
    (
        "iOS Simulator App container wiring",
        "ios_wasi_host_simulator_container",
    ),
    (
        "indexed guest-memory callback context",
        "typed_host_can_borrow_selected_defined_memories_by_standard_index",
    ),
    (
        "domain + generation guest resource handles",
        "native_module_can_own_a_resource_behind_a_generation_checked_guest_handle",
    ),
    (
        "generalize memory-zero call-scoped borrowing",
        "typed_host_can_borrow_selected_defined_memories_by_standard_index",
    ),
    (
        "explicit selected-memory callback context",
        "selected_memory_context_preserves_aliasing_for_imported_memories",
    ),
    (
        "unified host/guest handle lifetimes",
        "native_module_can_own_a_resource_behind_a_generation_checked_guest_handle",
    ),
    (
        "cross-runtime non-reused table domains",
        "shared_allocator_prevents_cross_runtime_stale_handle_aliases",
    ),
    (
        "native-resource snapshot quiescence",
        "native_module_can_own_a_resource_behind_a_generation_checked_guest_handle",
    ),
    (
        "versioned native import conventions",
        "native_module_names_are_canonical_and_major_versioned",
    ),
    (
        "converter-visible compatibility reports",
        "host_profile_cli_publishes_inspects_and_checks_without_execution",
    ),
    (
        "versioned JSON host-compatibility report",
        "host_profile_cli_publishes_inspects_and_checks_without_execution",
    ),
    (
        "versioned JSON lifecycle conformance report",
        "dynamic_converter_json_distinguishes_static_media_and_determinism_failures",
    ),
    (
        "versioned JSON replay conformance report",
        "replay_cli_records_checks_reproduces_and_never_overwrites",
    ),
    (
        "representative replay publication gate",
        "catalog_publisher_requires_exact_nonempty_representative_replay",
    ),
    (
        "cross-boundary copy/call benchmarks",
        "boundary_benchmark_separates_call_view_copy_and_guest_costs",
    ),
    (
        "bounded call/callback/completion channels",
        "native_completion_queue_bounds_identity_items_and_reserved_bytes",
    ),
    (
        "owner-thread completion queue core",
        "pending_native_completion_prevents_portable_snapshot",
    ),
    (
        "event-loop-neutral async completion ABI",
        "versioned_completion_imports_drive_pending_ready_take_and_stale_states",
    ),
    (
        "versioned guest import protocol",
        "versioned_completion_imports_drive_pending_ready_take_and_stale_states",
    ),
    (
        "C ABI channel ownership + late delivery",
        "ios_xcframework_swift_link",
    ),
    (
        "Swift MainActor owner + host profile",
        "ios_xcframework_swift_link",
    ),
    (
        "standard async cartridge fixture",
        "standard_async_completion_cartridge_runs_host_neutrally",
    ),
    (
        "booted iOS Simulator completion lifecycle",
        "ios_xcframework_swift_link",
    ),
    (
        "proposal priority by real cartridge workload",
        "real_cartridge_workload_prioritizes_standard_features",
    ),
    (
        "optional SIMD game-kernel subset",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "narrow-lane saturating add/sub",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "whole-vector bitwise masks",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "byte shuffle/swizzle",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "integer lane comparison masks",
        "every_integer_lane_comparison_has_standard_mask_semantics",
    ),
    (
        "signed/unsigned integer comparison masks",
        "every_integer_lane_comparison_has_standard_mask_semantics",
    ),
    (
        "integer all_true/bitmask reductions",
        "every_integer_lane_reduction_has_standard_scalar_semantics",
    ),
    (
        "integer lane min/max + unsigned average",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "wrapping integer lane arithmetic",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "scalar/vector lane bridge",
        "wabt_compiled_simd_game_kernels_match_tinyvm",
    ),
    (
        "fan-authored standard .wasm",
        "ordinary_c_toolchain_emits_a_portable_standard_cartridge",
    ),
    (
        "header-only C core v1 declarations",
        "ordinary_c_toolchain_emits_a_portable_standard_cartridge",
    ),
    ("<100KiB>", "size_budget_script_gates_100kib"),
    ("#78", "issue78_runtimes_stay_out_of_the_crate"),
    ("cu", "cu"),
    ("dyn", "dyn"),
    ("chassis", "chassis"),
    ("WASI as implicit/default game host", "WASI"),
    ("APE", "APE"),
    ("WAT", "WAT"),
    (
        "independent WABT/JSC differential per leaf",
        "accepted_standard_feature_matrix_executes_all_oracles_and_budgets",
    ),
    (
        "size/resource budget retained per leaf",
        "accepted_standard_feature_matrix_executes_all_oracles_and_budgets",
    ),
    (
        "P2 — accepted standard Wasm coverage",
        "every_reported_standard_feature_has_an_independent_executable_matrix_edge",
    ),
    ("bounded PCM tone synthesis", "ios_xcframework_swift_link"),
    (
        "single-buffer WAV + bounded wave LRU",
        "ios_xcframework_swift_link",
    ),
    (
        "interruption / route / reset owner",
        "ios_xcframework_swift_link",
    ),
    (
        "real App exact tone-event consumption",
        "current_main_runtime_runs_in_real_nostalgia_app_target",
    ),
    (
        "real App shared session + frame pacer",
        "current_main_runtime_runs_in_real_nostalgia_app_target",
    ),
];

/// Every integration test the canary can point a PRD leaf at.
///
/// **Both crates**, not just this one. The language leaves -- arrays,
/// closures, templates, arrows, methods -- are asserted in `tinyvm-qjs`'s
/// suite, so a canary that only read this crate's tests could never map them.
/// It did only read this crate's, which is why it had been failing with a
/// growing list of unmapped leaves: a gate that cannot go green teaches
/// everyone to read past it, and this one had a structural reason it could
/// not. Reading the sibling is the fix.
fn suite_test_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates = here.parent().expect("crates/");
    for dir in [here.join("tests"), crates.join("tinyvm-qjs").join("tests")] {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("integration-test entry").path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let src = fs::read_to_string(path).expect("read integration test");
            for line in src.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("fn ")
                    && let Some(name) = rest.split('(').next()
                {
                    names.insert(name.strip_prefix("r#").unwrap_or(name).to_string());
                }
            }
        }
    }
    names
}

fn cargo_deps_section() -> String {
    let t =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let after = t.split("[dependencies]").nth(1).unwrap_or("");
    after.split("\n[").next().unwrap_or(after).to_string()
}

/// Every `[x]` leaf is either an opcode family with executed goldens in all
/// three fixture files, or a leaf mapped to a test that asserts it.
#[test]
fn prd_x_leaves_have_suite_edges() {
    let prd = fs::read_to_string(prd_path()).expect("prd/PRD.md");
    let leaves = parse_prd_x_leaves(&prd);
    assert!(!leaves.is_empty(), "PRD fence must list [x] leaves");
    let tests = suite_test_names();
    let mut families: BTreeMap<String, usize> = BTreeMap::new();
    for name in ["mvp_goldens.txt", "family_extra.txt", "family_edge.txt"] {
        for case in load_cases(name) {
            *families.entry(case.family).or_default() += 1;
        }
    }
    let mapped: BTreeMap<&str, &str> = LEAF_TESTS.into_iter().collect();
    let mut missing = Vec::new();
    for leaf in &leaves {
        if let Some(test) = mapped.get(leaf.as_str()) {
            assert!(
                tests.contains(*test),
                "leaf {leaf} maps to missing test {test}"
            );
            continue;
        }
        match families.get(leaf) {
            // A family leaf needs a base path, an extra, and a spec edge.
            Some(n) if *n >= 3 => {}
            _ => missing.push(leaf.clone()),
        }
    }
    assert!(
        missing.is_empty(),
        "PRD [x] leaves with no executed backing: {missing:?}"
    );
}

/// Leaf phrase -> the test that exists once the leaf is built. Substring on
/// the leaf token, exact on the test name.
///
/// The `[ ]` line's comment column is rewritten every time a leaf is
/// re-planned; the feature phrase is the stable part, so this table keys on
/// the phrase rather than on the whole token the way `LEAF_TESTS` does. Add a
/// row at the moment a `[ ]` leaf is picked up as work, before its test lands;
/// the row then trips the day the test lands and nobody re-marks the PRD.
///
/// The test named must be one asserting the feature *works* -- never a test
/// that merely mentions the topic. Two leaves in the tree make any
/// topic-based heuristic fire wrongly: `exception handling` stays `[ ]` by
/// design (the wasm EH proposal, not JS `throw`, see the prose below the
/// tree), and `parse_int_is_not_silently_number` asserts a *refusal*, so
/// neither may appear here. Same for the `*_is_refused_by_name` and
/// `*_name_themselves` tests: a leaf whose only test is its refusal is
/// correctly `[ ]`.
///
/// On 2026-08-29 four leaves were found done-but-`[ ]` (commit 7c4f9dc): two
/// here (`break`/`continue`, `split`/`toLowerCase`), two in the downstream
/// PRD (`push`/`map`, the production call-site migration). `push`/`map` has a
/// row because this tree carries the same leaf; the call-site migration lives
/// only in the other repository's tree and is out of this test's reach.
const STALE_HINTS: &[(&str, &str)] = &[
    // Regression rows: `[x]` today, so they match no `[ ]` token, but if a
    // leaf is ever re-opened these fire the day its test comes back. At
    // `7c4f9dc^` these tests existed while the PRD still read `[ ]` -- except
    // `push / pop / map`, which was already `[x]` in this PRD and was stale
    // only in agenterm's; its row here is a seed for the mechanism, not a
    // record of a stale leaf in this file.
    ("`break` / `continue`", "break_leaves_the_loop"), // loops_and_replace_m3.rs
    ("split", "split_cuts_at_every_separator"),        // string_methods_m3.rs
    ("toLowerCase", "ascii_lowercases"),               // lowercase_m3.rs
    (
        "push / pop / map",
        "push_mutates_the_receiver_and_returns_the_new_length",
    ), // method_conformance.rs
    ("push / pop / map", "map_calls_back_into_a_function_value"), // method_conformance.rs
    // Open leaves, test names chosen now so the row exists before the work.
    (
        "无声明形式 `for (x of y)`",
        "an_assignment_target_can_be_the_loop_variable",
    ), // for_of_m3.rs
    ("`for … in`", "for_in_enumerates_own_properties_in_order"), // for_of_m3.rs
    (
        "具名导入 / 默认导出 / 再导出 / 动态 import",
        "a_named_import_binds_one_export",
    ), // modules_m3.rs
    // Regression row since 2026-08-29: hex/octal/binary lower (b7e757c);
    // numeric separators are the leaf that stays open.
    ("`0x`/`0o`/`0b` number literals", "the_three_radices_answer"), // radix_literals.rs
    (
        "numeric separators",
        "a_separator_is_dropped_from_the_digits",
    ), // lex_m1.rs
    ("tagged templates", "a_tag_receives_the_raw_strings_array"),   // templates_m3.rs
    ("`parseInt`", "parse_int_reads_a_prefix_in_the_given_radix"),  // loops_and_replace_m3.rs
    (
        "every other method",
        "every_shipped_method_has_a_row_and_a_body",
    ), // method_conformance.rs
    (
        "load-time lowering / stack-top caching",
        "a_lowered_module_runs_the_same_goldens",
    ), // interpreter_throughput.rs
    (
        "typed function references",
        "a_typed_funcref_call_checks_the_signature_at_load",
    ), // standard_feature_matrix.rs
    (
        "memory64 proposal",
        "a_memory64_module_addresses_past_four_gib",
    ), // standard_feature_matrix.rs
    ("exception handling", "a_try_table_catches_a_tagged_throw"),   // standard_feature_matrix.rs
    (
        "threads/shared memory",
        "an_atomic_rmw_on_shared_memory_is_ordered",
    ), // standard_feature_matrix.rs
];

/// The inverse of `prd_x_leaves_have_suite_edges`: a `[ ]` leaf whose
/// shipped-test already exists is done and nobody re-marked it.
///
/// "Done" is measured the same way as the forward canary measures "backed":
/// the named test exists in `suite_test_names()`. Running it from here would
/// be a nested `cargo test` per row and would turn a 0.02s gate into a build;
/// a test that exists and fails is a red gate in its own suite, in the same
/// command set the PRD's acceptance section prescribes.
///
/// The hygiene half: every hint's needle must still match a token in the
/// tree under some marker, so a renamed leaf cannot silently orphan its row.
/// The hint's test is *not* required to exist -- most rows are written
/// before their test by design.
#[test]
fn prd_unchecked_leaves_are_not_already_done() {
    let prd = fs::read_to_string(prd_path()).expect("prd/PRD.md");
    let unchecked = parse_prd_leaves(&prd, "[ ]");
    assert!(!unchecked.is_empty(), "PRD fence must list [ ] leaves");
    let tests = suite_test_names();
    let mut stale = Vec::new();
    for leaf in &unchecked {
        for (needle, test) in STALE_HINTS {
            if leaf.contains(needle) && tests.contains(*test) {
                stale.push(format!(
                    "leaf `{leaf}` is done ({test} exists) but still [ ]"
                ));
            }
        }
    }
    assert!(stale.is_empty(), "{}", stale.join("\n"));

    let mut every_token: Vec<String> = Vec::new();
    for marker in ["[x]", "[ ]", "[~]", "[–]"] {
        every_token.extend(parse_prd_leaves(&prd, marker));
    }
    let orphaned: Vec<&str> = STALE_HINTS
        .iter()
        .map(|(needle, _)| *needle)
        .filter(|needle| !every_token.iter().any(|token| token.contains(needle)))
        .collect();
    assert!(
        orphaned.is_empty(),
        "STALE_HINTS needles that match no PRD tree token: {orphaned:?}"
    );
}

#[test]
fn eval_bytes() {
    let case = load_cases("prd_leaves.txt")
        .into_iter()
        .find(|c| c.id == "eval(bytes)")
        .expect("eval(bytes) fixture");
    assert_expect(&case, run_case(&case));
}

#[test]
fn native_interpreter_boundary() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wasm = fs::read_to_string(crate_dir.join("src/wasm.rs")).expect("read wasm engine");
    assert!(
        !wasm.contains("unsafe"),
        "the interpreter engine must not require an unsafe native-code door"
    );
    let deps = cargo_deps_section().to_ascii_lowercase();
    for backend in [
        "javascriptcore",
        "webkit",
        "wasmtime",
        "wasmi",
        "cranelift",
        "dynasm",
    ] {
        assert!(
            !deps.contains(backend),
            "{backend} must not replace the tinyvm interpreter authority"
        );
    }
}

/// The `<100KiB>` leaf is a measurement, not a grep: this builds the no_std
/// static core, links it with production dead-code elimination, strips it, and
/// checks the size and the selftest.
#[test]
fn size_budget_script_gates_100kib() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = crate_dir.join("measure-core.sh");
    let sh = fs::read_to_string(&script).unwrap();
    assert!(
        sh.contains("102400"),
        "measure-core.sh must keep the 100 KiB cap"
    );
    assert!(sh.contains("OK: < 100 KiB and selftest==42"));

    if Command::new("sh")
        .args(["-c", "command -v cc"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("no C compiler: size gate not measured in this environment");
        return;
    }
    // A separate target dir so the outer cargo's lock is untouched.
    let target = crate_dir.join("../../target/measure-core-gate");
    let out = Command::new("sh")
        .arg(&script)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .expect("run measure-core.sh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("OK: < 100 KiB and selftest==42"),
        "measure-core.sh failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let size: usize = stdout
        .lines()
        .find_map(|l| l.strip_prefix("static core: "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .expect("size line");
    assert!(size < 102_400, "static core is {size} bytes");
    assert!(
        stdout.contains("selftest rc=42"),
        "selftest must return 42:\n{stdout}"
    );
}

#[test]
fn cu() {
    assert!(
        !cargo_deps_section().contains("agenterm-cu"),
        "cu is a non-goal: not a crate dependency"
    );
}

#[test]
fn r#dyn() {
    assert!(
        !cargo_deps_section().contains("agenterm-dyn"),
        "dyn is a non-goal: not a crate dependency"
    );
}

#[test]
fn chassis() {
    assert!(
        !cargo_deps_section().contains("agenterm-chassis"),
        "chassis is a non-goal: not a crate dependency"
    );
}

#[test]
#[allow(non_snake_case)]
fn WASI() {
    assert!(
        !cargo_deps_section().to_ascii_lowercase().contains("wasi"),
        "an external WASI runtime must not replace tinyvm's optional owned adapter"
    );
}

#[test]
#[allow(non_snake_case)]
fn APE() {
    let deps = cargo_deps_section();
    assert!(
        !deps.lines().any(|l| l.trim_start().starts_with("ape")),
        "APE is a non-goal: not kernel work"
    );
}

#[test]
fn issue78_runtimes_stay_out_of_the_crate() {
    let deps = cargo_deps_section().to_ascii_lowercase();
    for banned in ["sljit", "wasmtime", "wasmi", "wasmbin"] {
        assert!(
            !deps.contains(banned),
            "{banned} must not be a crate dep (#78)"
        );
    }
}

#[test]
#[allow(non_snake_case)]
fn WAT() {
    let deps = cargo_deps_section();
    assert!(
        !deps.to_ascii_lowercase().contains("wat") && !deps.contains("wabt"),
        "WAT is not a kernel input"
    );
}
