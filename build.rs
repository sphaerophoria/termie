use std::{fs::OpenOptions, io::BufWriter, path::Path, process::Command};

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("no out dir");
    let out_dir = Path::new(&out_dir);
    let terminfo_out_dir = out_dir.join("terminfo");
    let terminfo_definition = "res/termie.ti";
    println!("cargo:rerun-if-changed={terminfo_definition}");

    let mut child = Command::new("tic")
        .arg("-o")
        .arg(&terminfo_out_dir)
        .arg("-x")
        .arg(terminfo_definition)
        .spawn()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());

    let terminfo_tarball_path = out_dir.join("terminfo.tar");
    let terminfo_tarball_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(terminfo_tarball_path)
        .unwrap();

    let mut tar_builder = tar::Builder::new(BufWriter::new(terminfo_tarball_file));
    tar_builder
        .append_dir_all(".", terminfo_out_dir)
        .expect("Failed to add terminfo to tarball");
    tar_builder
        .finish()
        .expect("Failed to write terminfo tarball");
}
