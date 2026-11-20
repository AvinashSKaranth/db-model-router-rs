//! Domain models and schema definitions for the Blog Application.
//!
//! Models:
//! - Authors:  `id`, `username`, `email`, `display_name`, `bio`, `is_active`, `created_at`
//! - Posts:    `id`, `author_id`, `title`, `slug`, `content`, `published`, `is_deleted`, `created_at`, `updated_at`
//! - Comments: `id`, `post_id`, `author_id`, `content`, `is_deleted`, `created_at`
//! - Likes:    `id`, `post_id`, `author_id`, `created_at` (composite unique on `[post_id, author_id]`)
//! - Shares:   `id`, `post_id`, `author_id`, `platform`, `created_at`

use std::sync::Arc;

use dbmr::core::types::DbValue;
use dbmr::{DatabaseAdapter, Model, ModelHooks};

/// Grouping container for all initialized Blog domain models.
#[derive(Clone)]
pub struct BlogModels {
    pub authors: Arc<Model>,
    pub posts: Arc<Model>,
    pub comments: Arc<Model>,
    pub likes: Arc<Model>,
    pub shares: Arc<Model>,
}

/// Helper function to build and configure all blog models and their relations.
pub fn build_all(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<BlogModels> {
    let mut authors = build_authors(db)?;
    let mut posts = build_posts(db)?;
    let mut comments = build_comments(db)?;
    let mut likes = build_likes(db)?;
    let mut shares = build_shares(db)?;

    // Relations: belongs_to
    posts = posts.belongs_to("author", &authors, "author_id");
    comments = comments.belongs_to("post", &posts, "post_id");
    comments = comments.belongs_to("author", &authors, "author_id");
    likes = likes.belongs_to("post", &posts, "post_id");
    likes = likes.belongs_to("author", &authors, "author_id");
    shares = shares.belongs_to("post", &posts, "post_id");
    shares = shares.belongs_to("author", &authors, "author_id");

    // Relations: has_many
    posts = posts.has_many("comments", &comments, "post_id");
    posts = posts.has_many("likes", &likes, "post_id");
    posts = posts.has_many("shares", &shares, "post_id");
    authors = authors.has_many("posts", &posts, "author_id");

    Ok(BlogModels {
        authors: Arc::new(authors),
        posts: Arc::new(posts),
        comments: Arc::new(comments),
        likes: Arc::new(likes),
        shares: Arc::new(shares),
    })
}

/// Build Authors model.
pub fn build_authors(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<Model> {
    Model::builder(db.clone(), "authors")
        .column("id", "integer")
        .column("username", "required|string|minLength:3|maxLength:30")
        .column("email", "required|email")
        .column("display_name", "required|string|minLength:1")
        .column("bio", "string|nullable")
        .column("is_active", "integer|nullable")
        .column("created_at", "string|nullable")
        .unique(["username"])
        .unique(["email"])
        .build()
}

/// Build Posts model with relations, safe delete, and automatic slug derivation.
pub fn build_posts(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<Model> {
    let hooks = ModelHooks::new().before_save(|mut record, _ctx| async move {
        if let Some(DbValue::Text(title)) = record.get("title").cloned() {
            let trimmed = title.trim();
            record.insert("title".into(), DbValue::Text(trimmed.to_string()));

            // Generate slug if not provided or empty
            let slug_empty = match record.get("slug") {
                None | Some(DbValue::Null) => true,
                Some(DbValue::Text(s)) => s.trim().is_empty(),
                _ => false,
            };

            if slug_empty {
                let slug: String = trimmed
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '-' })
                    .collect::<String>()
                    .split('-')
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("-");
                record.insert("slug".into(), DbValue::Text(slug));
            }
        }
        Ok(record)
    });

    Model::builder(db.clone(), "posts")
        .column("id", "integer")
        .column("author_id", "required|integer")
        .column("title", "required|string|minLength:3|maxLength:120")
        .column("slug", "string|nullable")
        .column("content", "required|string|minLength:10")
        .column("published", "integer|nullable")
        .column("is_deleted", "integer|nullable")
        .column("created_at", "string|nullable")
        .column("updated_at", "string|nullable")
        .safe_delete("is_deleted")
        .unique(["slug"])
        .hooks(hooks)
        .build()
}

/// Build Comments model with safe delete.
pub fn build_comments(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<Model> {
    Model::builder(db.clone(), "comments")
        .column("id", "integer")
        .column("post_id", "required|integer")
        .column("author_id", "required|integer")
        .column("content", "required|string|minLength:1|maxLength:1000")
        .column("is_deleted", "integer|nullable")
        .column("created_at", "string|nullable")
        .safe_delete("is_deleted")
        .build()
}

/// Build Likes model with composite unique constraint `[post_id, author_id]`.
pub fn build_likes(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<Model> {
    Model::builder(db.clone(), "likes")
        .column("id", "integer")
        .column("post_id", "required|integer")
        .column("author_id", "required|integer")
        .column("created_at", "string|nullable")
        .unique(["post_id", "author_id"])
        .build()
}

/// Build Shares model with platform validation (`in:twitter,facebook,linkedin,mastodon`).
pub fn build_shares(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<Model> {
    Model::builder(db.clone(), "shares")
        .column("id", "integer")
        .column("post_id", "required|integer")
        .column("author_id", "required|integer")
        .column(
            "platform",
            "required|string|in:twitter,facebook,linkedin,mastodon",
        )
        .column("created_at", "string|nullable")
        .build()
}
