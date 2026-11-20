//! Repository automation, run as `cargo xtask <task>`.
// A command-line tool reports on stdout and stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeSet;
use std::process::{Command, ExitCode};

/// Crates whose logic must run unchanged on every host. They may not reach
/// I/O, clocks or randomness, so none of these may be in their dependency
/// closure.
const PURE_CRATES: &[&str] = &["cp-thread"];

const FORBIDDEN: &[&str] = &[
    "getrandom",
    "hyper",
    "mio",
    "rand",
    "reqwest",
    "rusqlite",
    "tokio",
    "worker",
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("deps") => deps(),
        _ => Err("usage: cargo xtask deps".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Checks each pure crate's normal dependencies as built for WebAssembly,
/// then builds it for WebAssembly. `cargo tree -p` resolves features for
/// that crate alone, so a feature another crate enables (say `uuid/v4` in a
/// host) does not hide or invent a dependency here.
fn deps() -> Result<(), String> {
    let mut failures = Vec::new();
    for krate in PURE_CRATES {
        let check = Command::new(env!("CARGO"))
            .args([
                "check",
                "--locked",
                "--package",
                krate,
                "--target",
                "wasm32-unknown-unknown",
            ])
            .status()
            .map_err(|e| format!("run cargo check: {e}"))?;
        if !check.success() {
            failures.push(format!("{krate} does not build for wasm32-unknown-unknown"));
        }
        let output = Command::new(env!("CARGO"))
            .args([
                "tree",
                "--locked",
                "--package",
                krate,
                "--edges",
                "normal",
                "--target",
                "wasm32-unknown-unknown",
                "--prefix",
                "none",
                "--format",
                "{lib}",
            ])
            .output()
            .map_err(|e| format!("run cargo tree: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo tree -p {krate}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let names: BTreeSet<&str> = std::str::from_utf8(&output.stdout)
            .map_err(|e| format!("cargo tree output: {e}"))?
            .lines()
            .map(str::trim)
            .collect();
        for bad in FORBIDDEN.iter().filter(|f| names.contains(*f)) {
            failures.push(format!("{krate} depends on {bad}"));
        }
    }
    if failures.is_empty() {
        println!("pure crates checked: {}", PURE_CRATES.len());
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}
