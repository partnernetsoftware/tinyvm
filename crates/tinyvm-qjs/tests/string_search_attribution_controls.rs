//! Controls for the frozen String-search attribution experiment.
//!
//! This is deliberately ignored in the ordinary suite: it runs four large
//! interpreter series and reports measurements rather than product behavior.
//! It changes neither the engine nor an existing performance gate.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

const LENGTHS: &[(u32, u64)] = &[(7, 2_048), (9, 8_192), (11, 32_768), (13, 131_072)];

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|error| panic!("compile {source:?}: {error}"));
    let module = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_steps: 4_000_000_000,
            ..Limits::default()
        },
    )
    .expect("load diagnostic module");
    let mut instance = module.instantiate().expect("instantiate diagnostic module");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|error| panic!("run {source:?}: {}", error.message()));
    instance.last_steps()
}

fn build(doublings: u32) -> String {
    format!(
        r#"let s = "0123456789abcdef"; for (let i = 0; i < {doublings}; i = i + 1) {{ s = s + s; }}"#
    )
}

fn slope(points: &[(u64, u64)]) -> f64 {
    let count = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| *x as f64).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| *y as f64).sum::<f64>() / count;
    let numerator = points
        .iter()
        .map(|(x, y)| (*x as f64 - mean_x) * (*y as f64 - mean_y))
        .sum::<f64>();
    let denominator = points
        .iter()
        .map(|(x, _)| (*x as f64 - mean_x).powi(2))
        .sum::<f64>();
    numerator / denominator
}

#[test]
#[ignore = "research court: four large interpreter series; run explicitly with --ignored"]
fn build_only_and_historical_controls_close() {
    let mut length_cost = Vec::new();
    let mut includes_absolute = Vec::new();
    let mut includes_historical = Vec::new();
    let mut index_of_absolute = Vec::new();
    let mut index_of_historical = Vec::new();

    println!(
        "length,build_steps,length_steps,includes_steps,index_of_steps,length_cost,includes_absolute,includes_historical,index_of_absolute,index_of_historical"
    );
    for &(doublings, length) in LENGTHS {
        let prefix = build(doublings);
        let build_steps = steps(&format!("{prefix} return 0;"));
        let length_steps = steps(&format!("{prefix} return s.length;"));
        let includes_steps = steps(&format!("{prefix} return s.includes(\"\\n<<<<<<<\");"));
        let index_of_steps = steps(&format!("{prefix} return s.indexOf(\"zz\");"));

        let length_delta = length_steps - build_steps;
        let includes_abs = includes_steps - build_steps;
        let includes_old = includes_steps - length_steps;
        let index_abs = index_of_steps - build_steps;
        let index_old = index_of_steps - length_steps;
        println!(
            "{length},{build_steps},{length_steps},{includes_steps},{index_of_steps},{length_delta},{includes_abs},{includes_old},{index_abs},{index_old}"
        );

        length_cost.push((length, length_delta));
        includes_absolute.push((length, includes_abs));
        includes_historical.push((length, includes_old));
        index_of_absolute.push((length, index_abs));
        index_of_historical.push((length, index_old));
    }

    let length_slope = slope(&length_cost);
    let includes_absolute_slope = slope(&includes_absolute);
    let includes_historical_slope = slope(&includes_historical);
    let index_of_absolute_slope = slope(&index_of_absolute);
    let index_of_historical_slope = slope(&index_of_historical);
    println!(
        "slopes length={length_slope:.4} includes_absolute={includes_absolute_slope:.4} includes_historical={includes_historical_slope:.4} index_of_absolute={index_of_absolute_slope:.4} index_of_historical={index_of_historical_slope:.4}"
    );

    for (label, absolute, historical) in [
        (
            "includes",
            includes_absolute_slope,
            includes_historical_slope,
        ),
        (
            "indexOf",
            index_of_absolute_slope,
            index_of_historical_slope,
        ),
    ] {
        let observed = absolute - historical;
        let tolerance = 0.25_f64.max(length_slope.abs() * 0.05);
        assert!(
            (observed - length_slope).abs() <= tolerance,
            "{label} ruler does not close: absolute {absolute:.4} - historical {historical:.4} = {observed:.4}, independent length {length_slope:.4}, tolerance {tolerance:.4}"
        );
    }
}
