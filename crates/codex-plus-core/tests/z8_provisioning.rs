// Keep this slice independently testable until the application crate wires
// `z8_provisioning` into the core module tree.  The production module remains
// free of Tauri and launcher dependencies.
#[path = "../src/z8_provisioning.rs"]
mod z8_provisioning;
