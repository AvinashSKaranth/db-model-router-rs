use crate::compiler::{format_bulk_saved_message, format_remove_message};
use async_trait::async_trait;
use compact_str::CompactString;
use dbmr_core::adapter::DatabaseAdapter;
use dbmr_core::error::{AppError, Result};
use dbmr_core::filter::{Condition, FilterExpr, Operator};
use dbmr_core::types::{
    DbValue, FindResult, GetOptions, RowMap, SortDirection, SortKey, WriteSummary,
};
use futures::StreamExt;
use mongodb::bson::{doc, oid::ObjectId, Bson, Document};
use mongodb::options::{ClientOptions, FindOptions, UpdateOptions};
use mongodb::{Client, Database};
use std::sync::Arc;

#[derive(Clone)]
pub struct MongoAdapter {
    db: Database,
}

impl MongoAdapter {
    pub async fn connect(url: &str, db_name: &str) -> Result<Arc<Self>> {
        let opts = ClientOptions::parse(url)
            .await
            .map_err(|e| AppError::Database(format!("Invalid Mongo URL: {e}")))?;
        let client = Client::with_options(opts)
            .map_err(|e| AppError::Database(format!("Mongo connection failed: {e}")))?;
        let db = client.database(db_name);
        Ok(Arc::new(Self { db }))
    }

    pub fn new(db: Database) -> Self {
        Self { db }
    }
}

fn dbvalue_to_bson(v: &DbValue) -> Bson {
    match v {
        DbValue::Null => Bson::Null,
        DbValue::Bool(b) => Bson::Boolean(*b),
        DbValue::Int(i) => {
            i64::try_from(*i).map_or_else(|_| Bson::String(i.to_string()), Bson::Int64)
        }
        DbValue::Float(f) => Bson::Double(*f),
        DbValue::Decimal(d) => Bson::String(d.to_string()),
        DbValue::Text(s) => {
            if let Ok(oid) = ObjectId::parse_str(s) {
                Bson::ObjectId(oid)
            } else {
                Bson::String(s.clone())
            }
        }
        DbValue::Bytes(b) => Bson::Binary(mongodb::bson::Binary {
            subtype: mongodb::bson::spec::BinarySubtype::Generic,
            bytes: b.clone(),
        }),
        DbValue::Json(j) => mongodb::bson::to_bson(j).unwrap_or(Bson::Null),
    }
}

fn bson_to_dbvalue(b: &Bson) -> DbValue {
    match b {
        Bson::Null => DbValue::Null,
        Bson::Boolean(b) => DbValue::Bool(*b),
        Bson::Int32(i) => DbValue::Int(i128::from(*i)),
        Bson::Int64(i) => DbValue::Int(i128::from(*i)),
        Bson::Double(f) => DbValue::Float(*f),
        Bson::String(s) => DbValue::Text(s.clone()),
        Bson::ObjectId(oid) => DbValue::Text(oid.to_hex()),
        Bson::Binary(bin) => DbValue::Bytes(bin.bytes.clone()),
        _ => {
            if let Ok(val) = mongodb::bson::from_bson::<serde_json::Value>(b.clone()) {
                DbValue::Json(val)
            } else {
                DbValue::Null
            }
        }
    }
}

fn rowmap_to_doc(row: &RowMap) -> Result<Document> {
    let mut doc = Document::new();
    for (k, v) in row {
        doc.insert(field(k.as_str())?, dbvalue_to_bson(v));
    }
    Ok(doc)
}

fn doc_to_rowmap(doc: &Document) -> RowMap {
    let mut map = RowMap::new();
    for (k, v) in doc {
        let key = if k == "_id" { "id" } else { k.as_str() };
        map.insert(CompactString::new(key), bson_to_dbvalue(v));
    }
    map
}

/// S6: reject field names Mongo would read as operators (`$...`) or that are empty.
fn field(column: &str) -> Result<&str> {
    if column.is_empty() || column.starts_with('$') || column.contains('\0') {
        return Err(AppError::UnprocessableEntity(format!(
            "Invalid field name: {column}"
        )));
    }
    Ok(if column == "id" { "_id" } else { column })
}

fn list_bson(v: &DbValue) -> Vec<Bson> {
    match v {
        DbValue::Json(serde_json::Value::Array(a)) => a
            .iter()
            .map(|item| dbvalue_to_bson(&DbValue::from(item.clone())))
            .collect(),
        other => vec![dbvalue_to_bson(other)],
    }
}

fn like_regex(v: &DbValue) -> String {
    let s = v.as_str().unwrap_or("");
    let body: Vec<_> = s.trim_matches('%').split('%').map(regex::escape).collect();
    body.join(".*")
}

/// Every comparison uses an explicit operator, so a JSON object value can never be read
/// as an operator document.
fn build_condition_doc(cond: &Condition) -> Result<Document> {
    let key = field(cond.column.as_str())?;
    let b = || dbvalue_to_bson(&cond.value);
    Ok(match (&cond.operator, &cond.value) {
        (Operator::IsNull, _) | (Operator::Eq, DbValue::Null) => {
            doc! { key: { "$eq": Bson::Null } }
        }
        (Operator::IsNotNull, _) | (Operator::NotEq, DbValue::Null) => {
            doc! { key: { "$ne": Bson::Null } }
        }
        (Operator::Eq, _) => doc! { key: { "$eq": b() } },
        (Operator::NotEq, _) => doc! { key: { "$ne": b() } },
        (Operator::Gt, _) => doc! { key: { "$gt": b() } },
        (Operator::Lt, _) => doc! { key: { "$lt": b() } },
        (Operator::Gte, _) => doc! { key: { "$gte": b() } },
        (Operator::Lte, _) => doc! { key: { "$lte": b() } },
        (Operator::Like, v) => doc! { key: { "$regex": like_regex(v), "$options": "i" } },
        (Operator::NotLike, v) => {
            doc! { key: { "$not": { "$regex": like_regex(v), "$options": "i" } } }
        }
        (Operator::In, v) => doc! { key: { "$in": list_bson(v) } },
        (Operator::NotIn, v) => doc! { key: { "$nin": list_bson(v) } },
    })
}

/// Soft-deleted rows carry `true` or `1`; a missing flag counts as live.
fn live(sd: &str) -> Document {
    doc! { sd: { "$nin": [true, 1] } }
}

fn compile_mongo_filter(filter: &FilterExpr, safe_delete: Option<&str>) -> Result<Document> {
    let mut or_parts = Vec::new();
    for group in filter.groups.iter().filter(|g| !g.is_empty()) {
        let mut and_parts = group
            .iter()
            .map(|c| build_condition_doc(c).map(Bson::Document))
            .collect::<Result<Vec<_>>>()?;
        if let Some(sd) = safe_delete {
            and_parts.push(Bson::Document(live(field(sd)?)));
        }
        or_parts.push(Bson::Document(doc! { "$and": and_parts }));
    }
    Ok(match (or_parts.len(), safe_delete) {
        (0, Some(sd)) => live(field(sd)?),
        (0, None) => Document::new(),
        (1, _) => match or_parts.remove(0) {
            Bson::Document(d) => d,
            _ => unreachable!(),
        },
        _ => doc! { "$or": or_parts },
    })
}

fn sort_doc(sort: &[SortKey]) -> Result<Option<Document>> {
    let mut d = Document::new();
    for k in sort.iter().filter(|k| !k.column.is_empty()) {
        let dir = if k.direction == SortDirection::Desc {
            -1
        } else {
            1
        };
        d.insert(field(k.column.as_str())?, dir);
    }
    Ok((!d.is_empty()).then_some(d))
}

impl MongoAdapter {
    #[allow(clippy::too_many_arguments)]
    async fn fetch(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        limit: Option<u64>,
        offset: Option<u64>,
        count: bool,
    ) -> Result<FindResult> {
        let coll = self.db.collection::<Document>(table);
        let query_doc = compile_mongo_filter(filter, safe_delete)?;
        let mut find_opts = FindOptions::default();
        find_opts.skip = offset;
        find_opts.limit = limit.map(|l| i64::try_from(l).unwrap_or(i64::MAX));
        find_opts.sort = sort_doc(sort)?;
        let mut cursor = coll
            .find(query_doc.clone())
            .with_options(find_opts)
            .await
            .map_err(|e| AppError::Database(format!("Mongo find error: {e}")))?;
        let mut rows = Vec::new();
        while let Some(res) = cursor.next().await {
            let doc = res.map_err(|e| AppError::Database(format!("Mongo cursor error: {e}")))?;
            rows.push(doc_to_rowmap(&doc));
        }
        let count = if count {
            coll.count_documents(query_doc)
                .await
                .map_err(|e| AppError::Database(format!("Mongo count error: {e}")))?
        } else {
            rows.len() as u64
        };
        Ok(FindResult { data: rows, count })
    }
}

#[async_trait]
impl DatabaseAdapter for MongoAdapter {
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        self.db.run_command(doc! { "ping": 1 }).await.is_ok()
    }

    async fn get(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult> {
        self.fetch(
            table,
            filter,
            sort,
            safe_delete,
            options.limit,
            options.offset,
            options.count,
        )
        .await
    }

    async fn list(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        page: u64,
        size: u64,
    ) -> Result<FindResult> {
        let offset = page.saturating_mul(size);
        self.fetch(
            table,
            filter,
            sort,
            safe_delete,
            Some(size),
            Some(offset),
            true,
        )
        .await
    }

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let coll = self.db.collection::<Document>(table);
        let query_doc = compile_mongo_filter(filter, safe_delete)?;
        let total = coll
            .count_documents(query_doc)
            .await
            .map_err(|e| AppError::Database(format!("Mongo count error: {e}")))?;
        Ok(total)
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        if data.is_empty() {
            return Ok(WriteSummary {
                rows: 0,
                message: format_bulk_saved_message(table, 0),
                id: None,
                record: None,
            });
        }

        let coll = self.db.collection::<Document>(table);
        let mut last_id = None;
        let mut last_record = None;

        for row in data {
            let mut doc = rowmap_to_doc(row)?;
            if !doc.contains_key("_id") {
                doc.insert("_id", ObjectId::new());
            }

            let inserted_id = doc.get("_id").cloned();
            let _ = coll
                .insert_one(doc.clone())
                .await
                .map_err(|e| AppError::Database(format!("Mongo insert error: {e}")))?;

            if let Some(id_b) = inserted_id {
                last_id = Some(bson_to_dbvalue(&id_b));
            }
            last_record = Some(doc_to_rowmap(&doc));
        }

        let count = data.len() as u64;
        Ok(WriteSummary {
            rows: count,
            message: format_bulk_saved_message(table, count),
            id: if count == 1 { last_id } else { None },
            record: if count == 1 { last_record } else { None },
        })
    }

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        if data.is_empty() {
            return Ok(WriteSummary {
                rows: 0,
                message: format_bulk_saved_message(table, 0),
                id: None,
                record: None,
            });
        }

        let coll = self.db.collection::<Document>(table);
        let mut total_rows = 0;

        for row in data {
            // Conflict target must be fully present; otherwise plain insert (an empty
            // filter would overwrite an arbitrary document).
            let Some(target) = unique
                .first()
                .filter(|u| u.iter().all(|k| row.contains_key(k.as_str())))
            else {
                self.insert_rows(table, std::slice::from_ref(row), unique)
                    .await?;
                total_rows += 1;
                continue;
            };
            let mut filter_doc = Document::new();
            for k in target {
                filter_doc.insert(field(k)?, doc! { "$eq": dbvalue_to_bson(&row[k.as_str()]) });
            }

            let mut set_doc = rowmap_to_doc(row)?;
            set_doc.remove("_id"); // $set must not include _id

            let update_doc = doc! { "$set": set_doc };
            let opts = UpdateOptions::builder().upsert(true).build();

            let res = coll
                .update_one(filter_doc, update_doc)
                .with_options(opts)
                .await
                .map_err(|e| AppError::Database(format!("Mongo upsert error: {e}")))?;

            if res.upserted_id.is_some() || res.modified_count > 0 || res.matched_count > 0 {
                total_rows += 1;
            }
        }

        Ok(WriteSummary {
            rows: total_rows,
            message: format_bulk_saved_message(table, total_rows),
            id: None,
            record: None,
        })
    }

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        let coll = self.db.collection::<Document>(table);
        let query_doc = compile_mongo_filter(filter, safe_delete)?;
        let count = match safe_delete {
            Some(sd) => {
                coll.update_many(query_doc, doc! { "$set": { field(sd)?: true } })
                    .await
                    .map_err(|e| AppError::Database(format!("Mongo delete error: {e}")))?
                    .modified_count
            }
            None => {
                coll.delete_many(query_doc)
                    .await
                    .map_err(|e| AppError::Database(format!("Mongo delete error: {e}")))?
                    .deleted_count
            }
        };
        Ok(WriteSummary {
            rows: count,
            message: format_remove_message(table, count),
            id: None,
            record: None,
        })
    }

    /// One `update_many` with `$set`: the filter (incl. any version check) and the write
    /// are evaluated atomically per document. Returns the matched count.
    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let mut set_doc = rowmap_to_doc(set)?;
        set_doc.remove("_id"); // _id is immutable
        if set_doc.is_empty() {
            return Ok(0);
        }
        let query_doc = compile_mongo_filter(filter, safe_delete)?;
        let res = self
            .db
            .collection::<Document>(table)
            .update_many(query_doc, doc! { "$set": set_doc })
            .await
            .map_err(|e| AppError::Database(format!("Mongo update error: {e}")))?;
        Ok(res.matched_count)
    }

    async fn query(&self, _sql: &str, _params: &[DbValue]) -> Result<Vec<RowMap>> {
        Err(AppError::Database(
            "Raw SQL queries not supported on MongoDB adapter".to_string(),
        ))
    }

    async fn ping(&self) -> Result<()> {
        self.db
            .run_command(doc! { "ping": 1 })
            .await
            .map(|_| ())
            .map_err(|e| AppError::Database(format!("Mongo ping error: {e}")))
    }

    fn escape_id(&self, name: &str) -> String {
        name.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use dbmr_core::filter::condition;

    #[test]
    fn filter_is_operator_safe() {
        let f = FilterExpr {
            groups: vec![vec![
                condition("age", Operator::Gte, DbValue::from(10)),
                condition("age", Operator::Lte, DbValue::from(20)),
                condition(
                    "name",
                    Operator::Eq,
                    DbValue::Json(serde_json::json!({"$ne": null})),
                ),
            ]],
        };
        let d = compile_mongo_filter(&f, Some("del")).unwrap();
        let and = d.get_array("$and").unwrap();
        assert_eq!(
            and.len(),
            4,
            "same-column conditions both kept + soft-delete"
        );
        let name = and[2].as_document().unwrap().get_document("name").unwrap();
        assert_eq!(name.keys().next().unwrap(), "$eq");
        assert!(compile_mongo_filter(&FilterExpr::eq("$where", "1"), None).is_err());
        let mut row = RowMap::new();
        row.insert("$set".into(), DbValue::from(1));
        assert!(rowmap_to_doc(&row).is_err());
        assert_eq!(
            compile_mongo_filter(&FilterExpr::default(), Some("del")).unwrap(),
            live("del")
        );
    }
}
