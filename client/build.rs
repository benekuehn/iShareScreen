fn main() {
    println!("cargo:rerun-if-changed=window.c");
    println!("cargo:rerun-if-changed=audio.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        let glfw = pkg_config::Config::new()
            .probe("glfw3")
            .expect("GLFW development package");
        let mut build = cc::Build::new();
        build.file("window.c").file("audio.c");
        for include in glfw.include_paths {
            build.include(include);
        }
        build.compile("iss_window");
    }
}
