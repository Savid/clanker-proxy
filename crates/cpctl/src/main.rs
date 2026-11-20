//! `cpctl`: drives this machine's clanker-proxy daemon.
// A command-line program writes its output to stdout and stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let env = cpctl::Env::from_process();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cpctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    if argv.iter().any(|a| a == "--version") {
        println!("cpctl v{}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut stdin = std::io::stdin();
    let mut live = |line: &str| {
        print!("{line}");
        let _ = std::io::stdout().flush();
    };
    let out = runtime.block_on(cpctl::run(&argv, &env, &mut stdin, &mut live));
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    ExitCode::from(out.exit)
}
