//! Property expressions: a line of script that computes a property every
//! frame (`expr box.loc.y "value + wiggle(2, 30)"`), so motion that would take
//! dozens of keys is one statement. Scripts are Rhai, sandboxed (no I/O) and
//! deterministic. In scope: `t` (seconds), `f` (frame), `value` (the keyed or
//! set value: a number, or an array for a whole vector property). Helpers:
//! `wiggle(freq, amp)`, `noise(x)` (smooth, -1..1), `rand(i)` (0..1),
//! `lerp(a, b, x)`, `clamp(x, lo, hi)`, `smooth(lo, hi, x)`, `pulse(period)`
//! (0..1 sawtooth), plus Rhai's math (`sin`, `cos`, `abs`, `floor`, `PI()`, ...).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use rhai::{AST, Array, Dynamic, Engine, Scope};

thread_local! {
    static ENGINE: Engine = engine();
    static CACHE: RefCell<HashMap<String, Result<AST, String>>> = RefCell::new(HashMap::new());
    /// Time and per-channel seed of the expression running now (for `wiggle`).
    static NOW: Cell<(f64, f64)> = const { Cell::new((0.0, 0.0)) };
}

fn hash(x: i64, seed: f64) -> f64 {
    let mut v = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.to_bits();
    v ^= v >> 33;
    v = v.wrapping_mul(0xff51_afd7_ed55_8ccd);
    v ^= v >> 33;
    (v >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise in -1..1.
fn noise(x: f64, seed: f64) -> f64 {
    let i = x.floor();
    let f = x - i;
    let s = f * f * (3.0 - 2.0 * f);
    let (a, b) = (hash(i as i64, seed) * 2.0 - 1.0, hash(i as i64 + 1, seed) * 2.0 - 1.0);
    a + (b - a) * s
}

/// A script number as f64: helpers take `2` as readily as `2.0`.
fn n(d: Dynamic) -> f64 {
    d.as_float().ok().or_else(|| d.as_int().ok().map(|i| i as f64)).unwrap_or(0.0)
}

fn engine() -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(200_000);
    e.set_max_call_levels(32);
    e.register_fn("noise", |x: Dynamic| noise(n(x), 0.0));
    e.register_fn("wiggle", |freq: Dynamic, amp: Dynamic| {
        let (t, seed) = NOW.with(Cell::get);
        n(amp) * noise(t * n(freq), seed)
    });
    e.register_fn("rand", |i: Dynamic| hash(n(i) as i64, 0.5));
    e.register_fn("lerp", |a: Dynamic, b: Dynamic, x: Dynamic| {
        let (a, b) = (n(a), n(b));
        a + (b - a) * n(x)
    });
    e.register_fn("clamp", |x: Dynamic, lo: Dynamic, hi: Dynamic| n(x).clamp(n(lo), n(hi)));
    e.register_fn("smooth", |lo: Dynamic, hi: Dynamic, x: Dynamic| {
        let (lo, hi) = (n(lo), n(hi));
        let k = ((n(x) - lo) / (hi - lo)).clamp(0.0, 1.0);
        k * k * (3.0 - 2.0 * k)
    });
    e.register_fn("pulse", |period: Dynamic| {
        let (t, _) = NOW.with(Cell::get);
        (t / n(period)).rem_euclid(1.0)
    });
    e
}

fn compiled(src: &str) -> Result<AST, String> {
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(src.to_string())
            .or_insert_with(|| ENGINE.with(|e| e.compile_expression(src).or_else(|_| e.compile(src)).map_err(|err| err.to_string())))
            .clone()
    })
}

/// The value an expression gives at time `t` (seconds), frame `f`, for a
/// property whose own value is `value` (one number, or the vector's components).
/// `seed` makes `wiggle` differ per object and channel.
pub fn eval(src: &str, t: f32, f: f32, value: &[f32], seed: u64) -> Result<Vec<f32>, String> {
    let ast = compiled(src)?;
    let mut scope = Scope::new();
    scope.push("t", t as f64);
    scope.push("f", f as f64);
    match value {
        [v] => scope.push("value", *v as f64),
        vs => scope.push("value", vs.iter().map(|v| Dynamic::from(*v as f64)).collect::<Array>()),
    };
    NOW.with(|n| n.set((t as f64, (seed % 10_007) as f64 * 17.13)));
    let out: Dynamic = ENGINE.with(|e| e.eval_ast_with_scope(&mut scope, &ast)).map_err(|err| err.to_string())?;
    let num = |d: &Dynamic| d.as_float().ok().or_else(|| d.as_int().ok().map(|i| i as f64)).map(|x| x as f32);
    if let Some(x) = num(&out) {
        return Ok(vec![x; value.len()]);
    }
    if let Some(arr) = out.clone().try_cast::<Array>() {
        let xs: Option<Vec<f32>> = arr.iter().map(num).collect();
        if let Some(xs) = xs.filter(|xs| xs.len() == value.len()) {
            return Ok(xs);
        }
    }
    Err(format!("expected {} number(s), got `{out}`", value.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_compute_from_time_and_value() {
        assert_eq!(eval("value + 10 * t", 2.0, 48.0, &[5.0], 1).unwrap(), vec![25.0]);
        assert_eq!(eval("[value[0], value[1] * 2.0, 0]", 0.0, 0.0, &[1.0, 3.0, 9.0], 1).unwrap(), vec![1.0, 6.0, 0.0]);
        assert_eq!(eval("1", 0.0, 0.0, &[0.0, 0.0], 1).unwrap(), vec![1.0, 1.0], "a number fills a vector");
        let w = |t| eval("540 + wiggle(2, 30) - 540", t, 0.0, &[0.0], 7).unwrap()[0];
        assert_eq!(w(1.3), w(1.3), "deterministic");
        assert!((0..50).all(|i| w(i as f32 * 0.1).abs() <= 30.0) && (0..50).any(|i| w(i as f32 * 0.1).abs() > 1.0));
        assert!(eval("value +", 0.0, 0.0, &[0.0], 1).is_err());
        assert!(eval("\"text\"", 0.0, 0.0, &[0.0], 1).is_err());
        assert!(eval("loop {}", 0.0, 0.0, &[0.0], 1).is_err(), "runaway scripts are stopped");
    }
}
