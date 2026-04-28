#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

/// Evaluate a Nix expression and return the result as a string.
///
/// On native targets this calls tvix-eval with a pure builder (no I/O).
/// On wasm32 targets tvix-eval cannot compile (it depends on `dirs` and other
/// native-only crates), so this always returns an error.
#[cfg(not(target_arch = "wasm32"))]
pub fn eval_nix(expr: &str) -> Result<String, String> {
    let eval = tvix_eval::Evaluation::builder_pure().build();
    let result = eval.evaluate(expr, None);

    if result.errors.is_empty() {
        match result.value {
            Some(value) => Ok(value.to_string()),
            None => Err("evaluation produced no value".into()),
        }
    } else {
        let msgs: Vec<String> = result.errors.iter().map(|e| e.to_string()).collect();
        Err(msgs.join("; "))
    }
}

#[cfg(target_arch = "wasm32")]
pub fn eval_nix(_expr: &str) -> Result<String, String> {
    Err("tvix-eval: native only".into())
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[wasm_bindgen]
pub fn eval_nix_wasm(expr: &str) -> Result<String, JsValue> {
    eval_nix(expr).map_err(|e| JsValue::from_str(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_basic() {
        assert_eq!(eval_nix("1 + 1").unwrap(), "2");
        assert_eq!(eval_nix(r#"builtins.toString 42"#).unwrap(), "\"42\"");
    }
}
