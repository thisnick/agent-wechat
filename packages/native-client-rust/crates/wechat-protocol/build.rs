fn main() {
    let mut build = cc::Build::new();
    build
        .include("vendor/silk/interface")
        .include("vendor/silk/src")
        .warnings(false)
        .flag_if_supported("-fwrapv")
        .flag_if_supported("-fno-strict-aliasing");
    let mut files = std::fs::read_dir("vendor/silk/src")
        .expect("vendored SILK sources")
        .map(|e| e.expect("source entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        build.file(file);
    }
    build.file("src/silk_bridge.c").compile("agent_wechat_silk");
    println!("cargo:rerun-if-changed=src/silk_bridge.c");
    println!("cargo:rerun-if-changed=vendor/silk/interface");
    println!("cargo:rerun-if-changed=vendor/silk/src");
}
