mod common;
use common::call;
use dbmr::adapters::SqliteAdapter;
use serde_json::json;

#[tokio::test]
async fn e1_book_catalog() {
    let app = app::build_app(SqliteAdapter::connect("sqlite::memory:").await.unwrap()).await.unwrap();
    let r = call(&app, "POST", "/books/0", &[], Some(json!({"title":"Dune","author":"Herbert","year":1965,"isbn":"1"}))).await;
    assert_eq!(r.status, 200, "single create: {}", r.text);
    let id = r.json["id"].as_i64().expect("created record has id");
    assert_eq!(r.json["title"], "Dune");

    let r = call(&app, "POST", "/books/0", &[], Some(json!({"author":"X","isbn":"2"}))).await;
    assert_eq!(r.status, 422, "missing title: {}", r.text);

    let r = call(&app, "POST", "/books", &[], Some(json!({"data":[
        {"title":"Neuromancer","author":"Gibson","year":1984,"isbn":"3"},
        {"title":"Anathem","author":"Stephenson","year":2008,"isbn":"4"}]}))).await;
    assert_eq!((r.status.as_u16(), r.json["rows"].as_i64()), (200, Some(2)), "bulk: {}", r.text);

    let r = call(&app, "GET", "/books?author=Gibson", &[], None).await;
    assert_eq!(r.json["count"], 1, "filter eq: {}", r.text);
    let r = call(&app, "GET", "/books?year=%3E%3D2000", &[], None).await;
    assert_eq!(r.json["count"], 1, "filter >=: {}", r.text);

    let r = call(&app, "PUT", &format!("/books/{id}"), &[], Some(json!({"title":"Dune","author":"Herbert","year":1966,"isbn":"1"}))).await;
    assert_eq!((r.status.as_u16(), r.json["year"].as_i64()), (200, Some(1966)), "update: {}", r.text);

    assert_eq!(call(&app, "DELETE", &format!("/books/{id}"), &[], None).await.status, 200);
    assert_eq!(call(&app, "GET", &format!("/books/{id}"), &[], None).await.status, 404);
}
