//! `fedimintd-floresta`: stock fedimintd with `florestad` as the Bitcoin backend.
//!
//! Scaffold checkpoint: this only proves the full fedimint dependency tree resolves
//! and compiles alongside the adapter. The real entry point (options parsing, tracing,
//! RocksDB, module registry, and `fedimint_server::run_with_iroh_p2p_relays_and_next_api`
//! with a `FlorestaClient` injected) is built in the next step.

use fedimint_floresta::FlorestaClient;

fn main() {
    // Touch the crates whose resolution we are checkpointing, so an unused-dependency
    // sweep does not remove them before they are wired in.
    let _modules = fedimintd::default_modules();
    let _ = FlorestaClient::new;
    println!("fedimintd-floresta scaffold: dependency tree resolves");
}
