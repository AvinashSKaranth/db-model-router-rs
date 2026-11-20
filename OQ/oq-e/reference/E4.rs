use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::types::row;
use dbmr::{create_model_router, AppError, Context, DatabaseAdapter, DbValue, Model, Transaction};
use serde_json::json;
use std::sync::Arc;

fn accounts(db: Arc<SqliteAdapter>) -> dbmr::Result<Model> {
    Model::builder(db, "accounts")
        .column("id", "integer")
        .column("owner", "required|string")
        .column("balance", "required|integer")
        .build()
}

pub async fn build_app(db: Arc<SqliteAdapter>) -> dbmr::Result<Router> {
    db.query("CREATE TABLE IF NOT EXISTS accounts (id INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL, balance INTEGER NOT NULL)", &[]).await?;
    Ok(Router::new().nest("/accounts", create_model_router(accounts(db)?)))
}

pub async fn transfer(db: Arc<SqliteAdapter>, from: i64, to: i64, amount: i64) -> dbmr::Result<()> {
    let ctx = Context::new();
    let tx = Transaction::begin(db.as_ref()).await?;
    let m = accounts(db)?.with_tx(&tx);
    let result = async {
        let mut moved = Vec::new();
        for (id, delta) in [(from, -amount), (to, amount)] {
            let acc = m.by_id(id, &ctx).await?.ok_or_else(|| AppError::NotFound(format!("Account {id} not found")))?;
            let DbValue::Int(bal) = acc["balance"] else { return Err(AppError::Internal("bad balance".into())) };
            let next = bal + i128::from(delta);
            if next < 0 {
                return Err(AppError::UnprocessableEntity("Insufficient funds".into()));
            }
            moved.push(row([("id", id.into()), ("balance", DbValue::Int(next))]));
        }
        for r in moved {
            m.patch(r, &ctx).await?;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => tx.commit().await,
        Err(e) => {
            tx.rollback().await?;
            Err(e)
        }
    }
}
