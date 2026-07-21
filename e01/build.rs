use std::io::{BufRead, BufReader};
use std::{env, fs, process::Command, str};

fn lines_from_file(file: &str) -> Vec<String> {
    let file = fs::File::open(file).expect("no such file");
    let buf = BufReader::new(file);
    buf.lines()
        .map(|l| l.expect("Could not parse line"))
        .collect()
}

fn remove_inner_attrs(file: &str) {
    let mut lines = lines_from_file(file);
    for line in &mut lines {
        if line.contains("#!") {
            *line = line.replace("#!", "#");
        }
    }

    fs::write(file, lines.join("\n")).expect("Failed to update file");
}

/// Run a git command in the crate directory, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| str::from_utf8(&out.stdout).ok().map(|s| s.trim().to_string()))
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Embed the current commit hash as the `GIT_COMMIT` compile-time env var.
///
/// The release build script (`scripts/build-release.sh`) sets `GIT_COMMIT`
/// itself after verifying a clean tree; honor that when present. Otherwise fall
/// back to asking git directly and append `-dirty` when tracked files have
/// uncommitted changes, so ad-hoc dev builds still carry a truthful marker.
fn emit_git_commit() {
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }

    let commit = env::var("GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let short = git(&["rev-parse", "--short", "HEAD"])?;
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            Some(if dirty { format!("{short}-dirty") } else { short })
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT={commit}");
}

fn main() {
    emit_git_commit();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=ksy/");

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let env_var_compiler_name = "KAITAI_STRUCT_COMPILER";

    // if env KAITAI_STRUCT_COMPILER is not defined
    if env::var_os(env_var_compiler_name).is_none() {
        // copy pre-generated files
        if let Ok(entries) = fs::read_dir(
            env::current_dir()
                .unwrap()
                .join("ksy")
                .join("pre-generated"),
        ) {
            for entry in entries.flatten() {
                let out = std::path::Path::new(out_dir.to_str().unwrap()).join(entry.file_name());
                println!("copying {:?} to {:?}", entry.path(), out);
                fs::copy(entry.path(), out).unwrap();
            }
        }
        println!("copyed pre-generated files");
        return;
    }

    let kaitai_struct_compiler = env::var_os(env_var_compiler_name)
        .unwrap_or_else(|| panic!("Not defined env var '{env_var_compiler_name}'"))
        .to_str()
        .unwrap()
        .to_string();
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
    if let Ok(entries) = fs::read_dir(env::current_dir().unwrap().join("ksy")) {
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
            remove_inner_attrs(entry.path().to_str().unwrap());
        }
    }
    assert_eq!(generated_files, ksy_files.len());

    println!("cargo:rerun-if-changed=ksy");
}
