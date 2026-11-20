//! HTTP REST router configuration for the Blog Application.
//!
//! Generates REST CRUD endpoints for all domain models, configures nested routes
//! (`/posts/{post_id}/comments`), and provides an OpenAPI 3.1 specification.

use axum::extract::{Path, Request};
use axum::middleware::{self, Next};
use axum::response::{Json, Response};
use axum::routing::get;
use axum::Router;
use dbmr::http::{openapi, openapi_route, RouterOptions};
use dbmr::{create_model_router, Scope};
use serde_json::json;

use super::model::BlogModels;

/// Middleware to extract `:post_id` from the URL path and inject it into the dbmr `Scope`.
///
/// This isolates nested route queries (e.g. `/posts/1/comments`) strictly to the specified post.
async fn extract_post_id_scope(Path(post_id): Path<i64>, mut req: Request, next: Next) -> Response {
    let mut scope = req.extensions().get::<Scope>().cloned().unwrap_or_default();
    scope.0.insert("post_id".into(), json!(post_id));
    req.extensions_mut().insert(scope);
    next.run(req).await
}

/// Build the unified Axum router for the blog application.
pub fn build_router(models: &BlogModels) -> dbmr::Result<Router> {
    let opts = RouterOptions::default();

    // Generate OpenAPI 3.1 specification document
    let spec = openapi(
        "Blog Application REST API",
        "1.0.0",
        &[
            ("/authors", &models.authors, &opts),
            ("/posts", &models.posts, &opts),
            ("/comments", &models.comments, &opts),
            ("/likes", &models.likes, &opts),
            ("/shares", &models.shares, &opts),
        ],
    );

    // Nested comments router under `/posts/{post_id}/comments`
    let post_comments_router = create_model_router(models.comments.clone())
        .route_layer(middleware::from_fn(extract_post_id_scope));

    let app = Router::new()
        .route("/health", get(|| async { Json(json!({"status": "ok"})) }))
        .nest("/openapi.json", openapi_route(spec))
        .nest("/authors", create_model_router(models.authors.clone()))
        .nest("/posts", create_model_router(models.posts.clone()))
        .nest("/comments", create_model_router(models.comments.clone()))
        .nest("/likes", create_model_router(models.likes.clone()))
        .nest("/shares", create_model_router(models.shares.clone()))
        .nest("/posts/{post_id}/comments", post_comments_router);

    Ok(app)
}
