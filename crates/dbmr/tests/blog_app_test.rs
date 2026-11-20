//! Integration test exercising the Blog Application project on dbmr.
//!
//! Validates that the modular blog_app project (`index`, `migration`, `model`,
//! `router`, `test`, `migrations/`) executes cleanly end-to-end.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../examples/blog_app/index.rs"]
pub mod index;
#[path = "../examples/blog_app/migration.rs"]
pub mod migration;
#[path = "../examples/blog_app/model.rs"]
pub mod model;
#[path = "../examples/blog_app/router.rs"]
pub mod router;
#[path = "../examples/blog_app/test.rs"]
pub mod test;

#[tokio::test]
async fn test_blog_app_full_workflow() -> dbmr::Result<()> {
    test::run_all_tests().await
}
