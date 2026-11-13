fn main() {
    // Packagers inject the build identity (see src/build_info.rs).
    println!("cargo:rerun-if-env-changed=PIIK_RELEASE");
    println!("cargo:rerun-if-env-changed=PIIK_REVISION");
    tauri_build::build();
}
