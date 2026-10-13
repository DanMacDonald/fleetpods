//! `cargo xtask test` runs the acceptance scenarios.
//!
//! Each scenario is its own test with its own world, so Cargo can run them
//! on separate threads. They do not share files, sockets, or environment
//! variables.

use std::env;
use std::process::{Command, ExitCode};
use std::thread::available_parallelism;

const SCENARIOS: [&str; 32] = [
    "a01", "a02", "a03", "a04", "a05", "a06", "a07", "a08", "a09", "a10", "a11", "a12", "a13",
    "a14", "a15", "a16", "a17", "a18", "a19", "a20", "a21", "a22", "a23", "a24", "a25", "a26",
    "a27", "a28", "a29", "a30", "a31", "a32",
];

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().cloned().unwrap_or_default();
    if command.is_empty() || command == "--help" || command == "-h" {
        usage();
        return if command.is_empty() { ExitCode::from(2) } else { ExitCode::SUCCESS };
    }
    args.remove(0);
    match command.as_str() {
        "test" => accept(&args),
        "test-unit" => unit(&args),
        "list" => list(),
        other => {
            eprintln!("unknown command {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "\
Usage: cargo xtask <command>

  test [filter...]   Run acceptance scenarios in parallel.
  test-unit          Run the library unit tests.
  list               Show the acceptance scenario ids."
    );
}

fn accept(args: &[String]) -> ExitCode {
    if let Err(missing) = scenarios_present() {
        eprintln!("acceptance suite is missing: {missing}");
        return ExitCode::from(1);
    }
    let args = passthrough(args);
    let mut cargo = cargo();
    cargo.args(["test", "-p", "fleetpods", "--features", "accept", "--test", "accept", "--"]);
    if !args.iter().any(|arg| arg.starts_with("--test-threads")) {
        let threads = thread_count();
        eprintln!("acceptance: {threads} threads");
        cargo.arg(format!("--test-threads={threads}"));
    }
    cargo.args(args);
    status(cargo)
}

fn unit(args: &[String]) -> ExitCode {
    let mut cargo = cargo();
    cargo.args(["test", "-p", "fleetpods", "--lib", "--bins", "--"]);
    cargo.args(passthrough(args));
    status(cargo)
}

fn list() -> ExitCode {
    for id in SCENARIOS {
        println!("{id}");
    }
    ExitCode::SUCCESS
}

fn scenarios_present() -> Result<(), String> {
    let output = cargo()
        .args([
            "test",
            "-p",
            "fleetpods",
            "--features",
            "accept",
            "--test",
            "accept",
            "--",
            "--list",
        ])
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let listed = String::from_utf8_lossy(&output.stdout);
    let mut missing = Vec::new();
    for id in SCENARIOS {
        let needle = format!("::{id}_");
        if !listed.contains(&needle) {
            missing.push(id);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join(" "))
    }
}

fn thread_count() -> usize {
    if let Ok(value) = env::var("FLEETPODS_TEST_THREADS") {
        if let Ok(n) = value.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    available_parallelism().map(|n| n.get()).unwrap_or(1)
}

fn passthrough(args: &[String]) -> Vec<String> {
    args.iter().filter(|arg| arg.as_str() != "--").cloned().collect()
}

fn cargo() -> Command {
    // Cargo locates the workspace from the current directory. A compile-time
    // manifest path becomes stale when a checkout and its target cache move.
    Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
}

fn status(mut command: Command) -> ExitCode {
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(err) => {
            eprintln!("could not run cargo: {err}");
            ExitCode::from(1)
        }
    }
}
