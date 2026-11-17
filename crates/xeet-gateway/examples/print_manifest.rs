//! Prints the gateway provider's manifest:
//!   cargo run -q -p xeet-gateway --example print_manifest > docs/gateway.manifest.json
fn main() {
    let m = serde_json::to_value(xeet_gateway::provider::manifest()).unwrap();
    println!("{}", xeet_artifact::canon::pretty(&m));
}
