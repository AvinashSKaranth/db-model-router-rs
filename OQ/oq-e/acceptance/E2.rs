mod common;
use common::call;
use dbmr::adapters::SqliteAdapter;
use serde_json::json;

#[tokio::test]
async fn e2_tenant_isolation() {
    let app = app::build_app(SqliteAdapter::connect("sqlite::memory:").await.unwrap()).await.unwrap();
    let (a, b) = (&[("x-tenant", "a")][..], &[("x-tenant", "b")][..]);
    assert_eq!(call(&app, "GET", "/notes", &[], None).await.status, 401, "no tenant header");

    let r = call(&app, "POST", "/notes/0", a, Some(json!({"title":"secret","tenant_id":"b"}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json["tenant_id"], "a", "client tenant_id must be overridden");
    let id = r.json["id"].as_i64().unwrap();

    assert_eq!(call(&app, "GET", "/notes", a, None).await.json["count"], 1);
    assert_eq!(call(&app, "GET", "/notes", b, None).await.json["count"], 0, "b sees a's notes");
    assert_eq!(call(&app, "GET", &format!("/notes/{id}"), b, None).await.status, 404);
    let r = call(&app, "PATCH", &format!("/notes/{id}"), b, Some(json!({"title":"pwned"}))).await;
    assert_eq!(r.status, 404, "b patched a's note: {}", r.text);
    assert_eq!(call(&app, "DELETE", &format!("/notes/{id}"), b, None).await.status, 404);
    let r = call(&app, "GET", "/notes?tenant_id=a", b, None).await;
    assert_eq!(r.json["count"], 0, "query param must not override tenant: {}", r.text);
    assert_eq!(call(&app, "GET", &format!("/notes/{id}"), a, None).await.json["title"], "secret");
}
