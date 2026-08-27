use std::{env, fs, path::Path, process::Command, str};

/// Turn the generated files' crate-level `#![...]` attributes into item
/// attributes, so the files can be included as modules. Only lines that
/// begin with `#!` are attributes; a `#!` inside a string literal is not.
fn remove_inner_attrs(file: &Path) {
    let src = fs::read_to_string(file).expect("read generated file");
    let out = src
        .lines()
        .map(|line| match line.trim_start().strip_prefix("#!") {
            Some(rest) => format!("#{rest}"),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(file, out).expect("Failed to update file");
}

fn main() {
    // Embed the current commit hash (shared logic with the other binaries).
    buildinfo::emit_git_commit();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ksy");
    println!("cargo:rerun-if-env-changed=KAITAI_STRUCT_COMPILER");

    // The parsers in ksy/pre-generated/ are checked in and compiled straight
    // from there (see src/generated/mod.rs). They are only regenerated when a
    // kaitai-struct-compiler is named.
    let env_var_compiler_name = "KAITAI_STRUCT_COMPILER";
    let Some(kaitai_struct_compiler) = env::var_os(env_var_compiler_name) else {
        return;
    };
    let kaitai_struct_compiler = kaitai_struct_compiler.to_str().unwrap().to_string();

    let ksy_dir = env::current_dir().unwrap().join("ksy");
    let out_dir = ksy_dir.join("pre-generated");

    let cmd_is_batch = kaitai_struct_compiler.ends_with(".bat");
    let mut cmd = if cmd_is_batch {
        Command::new("cmd")
    } else {
        Command::new(&kaitai_struct_compiler)
    };
    println!("kaitai_struct_compiler: {kaitai_struct_compiler}");

    if cmd_is_batch {
        cmd.args(["/C", &kaitai_struct_compiler]);
    };

    let mut ksy_files: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&ksy_dir) {
        for entry in entries.flatten() {
            if let Some(ext) = entry.path().extension()
                && ext == "ksy"
            {
                ksy_files.push(entry.path().as_path().to_string_lossy().to_string());
            }
        }
    }

    let output = cmd
        .args(["--target", "rust", "--outdir", out_dir.to_str().unwrap()])
        .args(&ksy_files)
        .output()
        .expect("failed to execute process");
    eprintln!("{cmd:?}");
    let errors = output.stderr;
    if !errors.is_empty() {
        let messages = str::from_utf8(&errors).unwrap();
        for message in messages.lines() {
            if message.trim().starts_with("error:") {
                panic!("{}", messages);
            }
        }
    }

    let mut generated_files = 0;
    if let Ok(entries) = fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            generated_files += 1;
            remove_inner_attrs(&entry.path());
        }
    }
    assert_eq!(generated_files, ksy_files.len());
}
