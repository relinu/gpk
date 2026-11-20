// With feature "cref": compile the C reference modules from legacy-c/ for differential tests.
fn main() {
    #[cfg(feature = "cref")]
    {
        let zinc = std::env::var("DEP_Z_INCLUDE").expect("libz-sys provides DEP_Z_INCLUDE");
        cc::Build::new()
            .files([
                "legacy-c/glz.c",
                "legacy-c/gpk/filters.c",
                "legacy-c/gpk/lzrc.c",
                "legacy-c/gpk/tcm.c",
                "legacy-c/gpk/pxc.c",
                "legacy-c/gpk/pngx.c",
                "legacy-c/gpk/jpgx.c",
            ])
            .include(zinc)
            .include("legacy-c")
            .opt_level(2)
            .warnings(false)
            .compile("cref");
        println!("cargo:rerun-if-changed=legacy-c");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
