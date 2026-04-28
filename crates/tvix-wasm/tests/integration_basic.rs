#[test]
fn integration_eval_real_nix() {
    assert_eq!(tvix_wasm::eval_nix("1 + 1").unwrap(), "2");
    assert_eq!(tvix_wasm::eval_nix("builtins.length [1 2 3]").unwrap(), "3");
}
