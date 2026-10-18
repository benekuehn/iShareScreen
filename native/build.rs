fn main() {
    println!("cargo:rerun-if-changed=bridge.c");
    println!("cargo:rerun-if-changed=presentation.c");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let protocols = pkg_config::get_variable("wayland-protocols", "pkgdatadir")
        .expect("wayland-protocols build dependency");
    let xml =
        std::path::Path::new(&protocols).join("stable/presentation-time/presentation-time.xml");
    for (mode, file) in [
        ("client-header", "presentation-time-client-protocol.h"),
        ("private-code", "presentation-time-protocol.c"),
    ] {
        assert!(
            std::process::Command::new("wayland-scanner")
                .arg(mode)
                .arg(&xml)
                .arg(out.join(file))
                .status()
                .expect("wayland-scanner build dependency")
                .success()
        );
    }
    let mut build = cc::Build::new();
    build.file("bridge.c").flag_if_supported("-std=c11");
    build
        .file("presentation.c")
        .file(out.join("presentation-time-protocol.c"))
        .include(&out);
    for name in [
        "libavcodec",
        "libavutil",
        "libva",
        "egl",
        "glesv2",
        "wayland-client",
    ] {
        let lib = pkg_config::Config::new().probe(name).expect(name);
        for path in lib.include_paths {
            build.include(path);
        }
    }
    build.compile("iss_bridge");
}
