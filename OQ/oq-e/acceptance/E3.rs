mod common;
use common::call;
use dbmr::adapters::SqliteAdapter;
use serde_json::json;

#[tokio::test]
async fn e3_nested_books() {
    let app = app::build_app(SqliteAdapter::connect("sqlite::memory:").await.unwrap()).await.unwrap();
    let a1 = call(&app, "POST", "/authors/0", &[], Some(json!({"name":"Le Guin"}))).await.json["id"].as_i64().unwrap();
    let a2 = call(&app, "POST", "/authors/0", &[], Some(json!({"name":"Banks"}))).await.json["id"].as_i64().unwrap();

    let r = call(&app, "POST", &format!("/authors/{a1}/books/0"), &[], Some(json!({"title":"Earthsea","author_id":a2}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json["author_id"].as_i64(), Some(a1), "author_id must come from path");
    let b1 = r.json["id"].as_i64().unwrap();
    call(&app, "POST", &format!("/authors/{a2}/books/0"), &[], Some(json!({"title":"Excession"}))).await;

    assert_eq!(call(&app, "GET", &format!("/authors/{a1}/books"), &[], None).await.json["count"], 1);
    assert_eq!(call(&app, "GET", "/books", &[], None).await.json["count"], 2);
    assert_eq!(call(&app, "GET", &format!("/authors/{a1}/books/{b1}"), &[], None).await.status, 200);
    assert_eq!(call(&app, "GET", &format!("/authors/{a2}/books/{b1}"), &[], None).await.status, 404);
    assert_eq!(call(&app, "DELETE", &format!("/authors/{a2}/books/{b1}"), &[], None).await.status, 404);
    assert_eq!(call(&app, "GET", &format!("/books/{b1}"), &[], None).await.status, 200, "wrongly deleted");
}
