use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/kwin_capture.c");
    println!("cargo:rerun-if-changed=src/uinput_inject.c");
    println!("cargo:rerun-if-changed=protocols/zkde-screencast-unstable-v1.xml");
    // KWin screencast helper 只在 Linux 编译；Windows/macOS 跳过。
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let xml = "protocols/zkde-screencast-unstable-v1.xml";
    println!("cargo:rerun-if-changed={xml}");
    println!("cargo:rerun-if-changed=src/kwin_capture.c");

    let header = out_dir.join("zkde-screencast-client-protocol.h");
    let code = out_dir.join("zkde-screencast-protocol.c");
    run(
        "wayland-scanner",
        &["client-header", xml, header.to_str().unwrap()],
    );
    run(
        "wayland-scanner",
        &["private-code", xml, code.to_str().unwrap()],
    );

    let wayland = pkg_config::probe_library("wayland-client").expect("wayland-client 开发库");
    let mut build = cc::Build::new();
    build
        .file(&code)
        .file("src/kwin_capture.c")
        .file("src/uinput_inject.c")
        .include(&out_dir)
        .warnings(false);
    for include in &wayland.include_paths {
        build.include(include);
    }
    build.compile("lan_video_kwin_capture");
}

fn run(bin: &str, args: &[&str]) {
    let status = Command::new(bin)
        .args(args)
        .status()
        .unwrap_or_else(|err| panic!("运行 {bin} 失败: {err}"));
    if !status.success() {
        panic!("{bin} 执行失败: {status}");
    }
}
