// libghostty-vt の静的ライブラリの場所。既定は vendor/ghostty-vt（zig build --prefix の出力）。
// GHOSTTY_VT_PREFIX で差し替えられる。
fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prefix = std::env::var("GHOSTTY_VT_PREFIX")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("vendor/ghostty-vt"));
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_PREFIX");
    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    if std::env::var("SPIKE_DYNAMIC").is_ok() {
        println!("cargo:rustc-link-lib=dylib=ghostty-vt");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    }
    println!("cargo:rerun-if-env-changed=SPIKE_DYNAMIC");
}
