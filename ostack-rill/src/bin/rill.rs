//! `rill` with the `ostack.*` namespace: rill's own front end, with the
//! ostack backend registered before anything runs.

#[path = "../../../src/main.rs"]
mod front_end;

fn main() -> std::process::ExitCode {
    ostack_rill::register();
    front_end::main()
}
