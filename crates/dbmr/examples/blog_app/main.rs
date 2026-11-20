//! Blog Application entrypoint.
//!
//! Run verification: `cargo run -p dbmr --example blog_app`
//! Serve HTTP on :3000: `cargo run -p dbmr --example blog_app -- --serve`
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub mod index;
pub mod migration;
pub mod model;
pub mod router;
pub mod test;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--serve" || arg == "-s") {
        index::run_server("127.0.0.1:3000").await?;
    } else {
        test::run_all_tests().await?;
    }
    Ok(())
}
