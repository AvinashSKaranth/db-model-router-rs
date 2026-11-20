mod common;
use common::call;
use dbmr::adapters::SqliteAdapter;
use serde_json::json;

#[tokio::test]
async fn e4_atomic_transfer() {
    let db = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
    let app = app::build_app(db.clone()).await.unwrap();
    let a = call(&app, "POST", "/accounts/0", &[], Some(json!({"owner":"a","balance":100}))).await.json["id"].as_i64().unwrap();
    let b = call(&app, "POST", "/accounts/0", &[], Some(json!({"owner":"b","balance":0}))).await.json["id"].as_i64().unwrap();
    let bal = |id: i64| {
        let app = app.clone();
        async move { call(&app, "GET", &format!("/accounts/{id}"), &[], None).await.json["balance"].as_i64() }
    };

    app::transfer(db.clone(), a, b, 30).await.unwrap();
    assert_eq!((bal(a).await, bal(b).await), (Some(70), Some(30)));

    let err = app::transfer(db.clone(), a, b, 1000).await.unwrap_err();
    assert_eq!(err.status_code(), 422, "overdraft: {err:?}");
    assert_eq!((bal(a).await, bal(b).await), (Some(70), Some(30)), "overdraft changed balances");

    let err = app::transfer(db.clone(), a, 9999, 10).await.unwrap_err();
    assert_eq!(err.status_code(), 404, "missing account: {err:?}");
    assert_eq!(bal(a).await, Some(70), "partial transfer to missing account was committed");
}
