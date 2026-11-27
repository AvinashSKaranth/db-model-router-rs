//! `dbmr-http`: axum router for a [`crate::core::Model`] (Node `route.js` parity, incl. `QUERY`).

pub mod formatter;
pub mod health;
pub mod openapi;
pub mod router;

pub use crate::core::{CurrentUser, Scope};
pub use formatter::{format_csv, format_xml};
pub use health::health_router;
pub use openapi::{openapi, openapi_route};
pub use router::{
    allowed_collection_methods, allowed_item_methods, create_model_router,
    create_model_router_with, error_response, CachedColumn, DiscoveryCache, ModelRouterState,
    RouterOptions, CARDINALITY_FIXED_LIMIT, CARDINALITY_PROBE_LIMIT, DEFAULT_PREWARM_COLUMNS_LIMIT,
};
