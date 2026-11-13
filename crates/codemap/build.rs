fn main() {
    let root = "vendor/tree-sitter-blade/src";
    cc::Build::new()
        .std("c11")
        .include(root)
        .file(format!("{root}/parser.c"))
        .file(format!("{root}/scanner.c"))
        .warnings(false)
        .compile("stele-blade-parser");
    println!("cargo:rerun-if-changed={root}");
}
