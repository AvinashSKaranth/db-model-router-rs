//! `dbmr-http`: axum router for a [`dbmr_core::Model`] (Node `route.js` parity, incl. `QUERY`).

pub mod formatter;
pub mod health;
pub mod openapi;
pub mod router;

pub use dbmr_core::{CurrentUser, Scope};
pub use formatter::{format_csv, format_xml};
pub use health::health_router;
pub use openapi::{openapi, openapi_route};
pub use router::{
    create_model_router, create_model_router_with, error_response, ModelRouterState, RouterOptions,
};
