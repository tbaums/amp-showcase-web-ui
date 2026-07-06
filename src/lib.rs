pub mod app;
pub mod models;
pub mod storage;
pub mod sync;

/// The app version, from Cargo.toml at compile time. Surfaced in the UI (so a
/// client can see which build it's on) AND — because `env!` bakes the literal
/// into the WASM — bumping the version changes the compiled bytes, hence the
/// content-hashed asset filename changes, forcing returning clients to fetch
/// the new bundle. A version-only release therefore still busts the cache.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod version_tests {
    use super::VERSION;
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn version_is_present_and_dotted() {
        // Proves the compile-time version wiring: non-empty and semver-shaped,
        // so the UI always has a real build identifier to show and to embed in
        // the WASM (the cache-bust guarantee).
        assert!(!VERSION.is_empty(), "CARGO_PKG_VERSION must be set");
        assert!(VERSION.contains('.'), "expected a dotted version, got {VERSION:?}");
    }
}
