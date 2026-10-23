//! The evaluator moves `Value`s and `Result<Value, RillError>`s through every
//! expression; keeping them small keeps the tree-walker fast.

#[test]
fn value_and_error_types_stay_small() {
    let value = std::mem::size_of::<rill::types::Value>();
    let error = std::mem::size_of::<rill::diagnostic::RillError>();
    let result = std::mem::size_of::<Result<rill::types::Value, rill::diagnostic::RillError>>();
    eprintln!("Value={value} RillError={error} Result<Value, RillError>={result}");
    assert!(value <= 32, "Value grew to {value} bytes; share or box the large variants");
    assert!(error <= 80, "RillError grew to {error} bytes; box the rarely used fields");
}
