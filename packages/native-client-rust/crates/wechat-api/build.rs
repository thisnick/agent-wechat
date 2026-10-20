fn main() {
    println!(
        "cargo:rustc-env=WX_TARGET={}",
        std::env::var("TARGET").expect("cargo target")
    );
}
