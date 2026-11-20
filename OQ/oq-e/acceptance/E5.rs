mod common;
use common::call;
use dbmr::adapters::SqliteAdapter;
use dbmr::DatabaseAdapter;
use serde_json::json;

#[tokio::test]
async fn e5_soft_delete_search_csv() {
    let db = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
    let app = app::build_app(db.clone()).await.unwrap();
    let r = call(&app, "POST", "/tasks", &[], Some(json!({"data":[
        {"title":"write report","status":"open"},
        {"title":"review report","status":"done"},
        {"title":"lunch","status":"open"}]}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(call(&app, "POST", "/tasks/0", &[], Some(json!({"title":"x","status":"later"}))).await.status, 422);

    assert_eq!(call(&app, "GET", "/tasks?search=report", &[], None).await.json["count"], 2);

    let id = call(&app, "GET", "/tasks?title=lunch", &[], None).await.json["data"][0]["id"].as_i64().unwrap();
    assert_eq!(call(&app, "DELETE", &format!("/tasks/{id}"), &[], None).await.status, 200);
    assert_eq!(call(&app, "GET", "/tasks", &[], None).await.json["count"], 2, "soft-deleted row visible");
    assert_eq!(call(&app, "GET", &format!("/tasks/{id}"), &[], None).await.status, 404);
    let raw = db.query("SELECT is_deleted FROM tasks WHERE title = 'lunch'", &[]).await.unwrap();
    assert_eq!(raw.len(), 1, "row was hard-deleted");
    assert_eq!(raw[0]["is_deleted"], dbmr::DbValue::Int(1));

    let r = call(&app, "GET", "/tasks?output_content_type=csv&sort=id", &[], None).await;
    assert!(r.content_type.starts_with("text/csv"), "{}", r.content_type);
    assert_eq!(r.text.lines().count(), 3, "header + 2 live rows: {}", r.text);
    assert!(r.text.lines().next().unwrap().contains("title"));
}
