use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let proto_root = PathBuf::from("third_party")
        .join("HackArena-Proto")
        .join("proto");
    if !proto_root.exists() {
        println!(
            "cargo:warning=Proto root not found: {} (skipping generation)",
            proto_root.display()
        );
        return;
    }

    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc");
    // SAFETY: build script runs single-threaded for this crate process.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    let mut protos = Vec::<PathBuf>::new();
    collect_proto_files(&proto_root, &mut protos).expect("collect .proto files");
    if protos.is_empty() {
        println!(
            "cargo:warning=No .proto files found under {}",
            proto_root.display()
        );
        return;
    }

    println!("cargo:rerun-if-changed={}", proto_root.display());
    println!(
        "cargo:warning=Generating Rust from {} proto file(s)",
        protos.len()
    );

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&protos, &[proto_root])
        .expect("compile proto files");
}

fn collect_proto_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_proto_files(&path, out)?;
            continue;
        }
        if file_type.is_file() && path.extension().is_some_and(|ext| ext == "proto") {
            out.push(path);
        }
    }
    Ok(())
}
