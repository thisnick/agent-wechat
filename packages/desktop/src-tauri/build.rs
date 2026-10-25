fn main() {
    println!("cargo:rerun-if-changed=tauri.linux.conf.json");
    tauri_build::build()
}
