fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    // With rtlog's `strip` feature the descriptor sections are not loaded.
    let sites = if std::env::var_os("CARGO_FEATURE_RTLOG10STRIP").is_some() {
        "sites-strip.x"
    } else {
        "sites.x"
    };
    // The linker looks for memory.x in the crate root before OUT_DIR, so the
    // parts live in layout/ and the root has no memory.x of its own.
    let memory = std::fs::read_to_string("layout/memory.x").unwrap();
    let sites = std::fs::read_to_string(format!("layout/{sites}")).unwrap();
    std::fs::write(out.join("memory.x"), memory + &sites).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    for f in ["memory.x", "sites.x", "sites-strip.x"] {
        println!("cargo:rerun-if-changed=layout/{f}");
    }
}
