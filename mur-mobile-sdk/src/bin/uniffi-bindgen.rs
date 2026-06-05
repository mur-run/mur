//! Bundled UniFFI binding generator (enabled via the `cli` feature).
//!
//! Generate Swift bindings from the built library:
//! ```sh
//! cargo build --features cli
//! cargo run --features cli --bin uniffi-bindgen -- \
//!     generate --library target/debug/libmur_mobile_sdk.dylib \
//!     --language swift --out-dir <out>
//! ```
fn main() {
    uniffi::uniffi_bindgen_main()
}
