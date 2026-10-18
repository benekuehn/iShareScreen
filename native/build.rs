fn main() {
    println!("cargo:rerun-if-changed=bridge.c");
    let mut build = cc::Build::new();
    build.file("bridge.c").flag_if_supported("-std=c11");
    for name in ["libavcodec", "libavutil", "libva", "egl", "glesv2"] {
        let lib = pkg_config::Config::new().probe(name).expect(name);
        for path in lib.include_paths {
            build.include(path);
        }
    }
    build.compile("iss_bridge");
}
