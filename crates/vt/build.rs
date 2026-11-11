//! libghostty-vt の静的ライブラリをリンクする（spikes/vt-link と同じ）。
//! 既定は vendor/ghostty-vt（`cargo xtask fetch-ghostty-vt` の出力）。GHOSTTY_VT_PREFIX で差し替えられる。
fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prefix = std::env::var("GHOSTTY_VT_PREFIX")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("vendor/ghostty-vt"));
    let lib = prefix.join("lib");
    if !lib.join("ghostty-vt-static.lib").exists() {
        println!(
            "cargo:warning={} に ghostty-vt-static.lib がありません。`cargo xtask fetch-ghostty-vt` を実行してください",
            lib.display()
        );
    }
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_PREFIX");
    println!("cargo:rerun-if-changed={}", lib.join("ghostty-vt-static.lib").display());
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt-static");
}
