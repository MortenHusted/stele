mod support;

use callfix_callee::{Worker, cross_crate_free, in_macro_arg};
fn run() {
    let worker = Worker;
    let _receiver = worker.receiver(1);
    let _associated = Worker::associated(2);
    let _module = support::local_helper(3);
    let _cross_crate = cross_crate_free(4);

    let _external_to_string = serde_json::to_string(&1_u8);
    let _external_from_str: Result<u8, _> = serde_json::from_str("1");
    let _external_to_value = serde_json::to_value(1_u8);
    let _standard_library = std::fs::read("not-executed");
    let _in_macro_argument = format!("{}", in_macro_arg(7));
    std_backed_receivers();
}

// Regression gate: these receivers are only inferable when the sysroot
// crates are attached to the crate graph (`Box::new` and `Option::unwrap`
// live in the standard library). Against an empty startup crate graph both
// calls degrade to Unmapped observations and the edges below vanish.
fn std_backed_receivers() {
    let boxed = Box::new(Worker);
    let _boxed_call = boxed.receiver(5);
    let optional = Some(Worker);
    let _unwrapped_call = optional.unwrap().receiver(6);
}

fn main() {
    run();
}
