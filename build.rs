use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=resources/viewer.gresource.xml");
    println!("cargo:rerun-if-changed=resources/visor-mascot.png");
    let destination =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR")).join("viewer.gresource");
    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir=resources")
        .arg("--target")
        .arg(destination)
        .arg("resources/viewer.gresource.xml")
        .status()
        .expect("Install glib-compile-resources (GLib development tools)");
    assert!(
        status.success(),
        "Could not compile the viewer icon resource"
    );
}
