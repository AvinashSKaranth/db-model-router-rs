use clap::{Parser, Subcommand};
use std::process::{Command, ExitStatus};

#[derive(Parser)]
#[command(name = "xtask", about = "Developer ergonomics and CI automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run all CI checks locally (fmt, clippy, test, security lints)
    Ci,
    /// Run all unit and integration tests across the workspace
    TestAll,
    /// Run differential benchmarks
    Bench,
    /// Check test-case traceability mapping against Node reference suite
    TraceabilityCheck,
}

fn run_step(desc: &str, cmd: &mut Command) -> std::io::Result<ExitStatus> {
    println!("==> Running: {desc}");
    cmd.status()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ci => {
            println!("--- Running CI Pipeline locally ---");

            let status = run_step(
                "cargo fmt --all -- --check",
                Command::new("cargo").args(["fmt", "--all", "--", "--check"]),
            )?;
            if !status.success() {
                eprintln!("FAIL: rustfmt check failed");
                std::process::exit(1);
            }

            let status = run_step(
                "cargo clippy",
                Command::new("cargo").args([
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ]),
            )?;
            if !status.success() {
                eprintln!("FAIL: clippy lint failed");
                std::process::exit(1);
            }

            let status = run_step(
                "cargo test --workspace --all-features",
                Command::new("cargo").args(["test", "--workspace", "--all-features"]),
            )?;
            if !status.success() {
                eprintln!("FAIL: workspace unit tests failed");
                std::process::exit(1);
            }

            let status = run_step(
                "cargo run -p xtask -- traceability-check",
                Command::new("cargo").args(["run", "-p", "xtask", "--", "traceability-check"]),
            )?;
            if !status.success() {
                eprintln!("FAIL: traceability check failed");
                std::process::exit(1);
            }

            let status = run_step(
                "python scripts/ci_lints.py",
                Command::new("python").args(["scripts/ci_lints.py"]),
            )?;
            if !status.success() {
                eprintln!("FAIL: static security lints failed");
                std::process::exit(1);
            }

            println!("==> ALL CI CHECKS PASSED GREEN!");
        }
        Commands::TestAll => {
            let status = Command::new("cargo")
                .args(["test", "--workspace", "--all-features"])
                .status()?;
            if !status.success() {
                std::process::exit(1);
            }
        }
        Commands::Bench => {
            let status = Command::new("cargo")
                .args(["bench", "--workspace"])
                .status()?;
            if !status.success() {
                std::process::exit(1);
            }
        }
        Commands::TraceabilityCheck => {
            // Rebuild with: python scripts/build_traceability.py <node-repo>
            // Gate: every case has a known status and every referenced Rust file exists.
            let data = std::fs::read_to_string("tests/traceability.json")?;
            let v: serde_json::Value = serde_json::from_str(&data)?;
            let cases = v["cases"].as_array().map_or(&[] as &[_], |a| a.as_slice());
            let mut counts = std::collections::BTreeMap::<&str, u32>::new();
            let mut bad = Vec::new();
            for c in cases {
                let status = c["status"].as_str().unwrap_or("");
                if !["covered", "unverified", "out_of_scope", "waived", "open"].contains(&status) {
                    bad.push(format!(
                        "{}:{} unknown status '{status}'",
                        c["file"], c["line"]
                    ));
                }
                for r in c["rust"].as_array().into_iter().flatten() {
                    let r = r.as_str().unwrap_or("");
                    if !std::path::Path::new(r).exists() {
                        bad.push(format!(
                            "{}:{} references missing {r}",
                            c["file"], c["line"]
                        ));
                    }
                }
                *counts.entry(status).or_default() += 1;
            }
            println!("--- Test Traceability (file-level) ---");
            for (k, n) in &counts {
                println!("{k:>13}: {n}");
            }
            let get = |k: &str| f64::from(counts.get(k).copied().unwrap_or(0));
            let in_scope = get("covered") + get("unverified") + get("open");
            if in_scope > 0.0 {
                println!(
                    "covered / in-scope: {:.1}%",
                    get("covered") / in_scope * 100.0
                );
            }
            if !bad.is_empty() {
                eprintln!(
                    "FAIL:
  {}",
                    bad.join(
                        "
  "
                    )
                );
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
