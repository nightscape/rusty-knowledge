use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json;
use sqlx::{Column, Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::collections::HashMap;
use std::path::Path;

use crate::storage::{
    backend::StorageBackend,
    schema::{EntitySchema, FieldType},
    types::{Entity, Filter, Result, StorageError, Value},
};

#[derive(Debug)]
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    pub async fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_url = format!("sqlite:{}", db_path.as_ref().display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(Self { pool })
    }

    pub async fn new_in_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect("sqlite::memory:")
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(Self { pool })
    }

    fn value_to_sql_param(&self, value: &Value) -> String {
        match value {
            Value::String(s) => format!("'{}'", s.replace('\'', "''")),
            Value::Integer(i) => i.to_string(),
            Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
            Value::DateTime(dt) => format!("'{}'", dt.to_rfc3339()),
            Value::Json(j) => format!(
                "'{}'",
                serde_json::to_string(j).unwrap().replace('\'', "''")
            ),
            Value::Reference(r) => format!("'{}'", r.replace('\'', "''")),
            Value::Null => "NULL".to_string(),
        }
    }

    #[allow(dead_code)]
    fn sql_value_to_value(&self, raw: &str, field_type: &FieldType) -> Result<Value> {
        match field_type {
            FieldType::String => Ok(Value::String(raw.to_string())),
            FieldType::Integer => raw
                .parse::<i64>()
                .map(Value::Integer)
                .map_err(|e| StorageError::SerializationError(e.to_string())),
            FieldType::Boolean => Ok(Value::Boolean(raw == "1")),
            FieldType::DateTime => DateTime::parse_from_rfc3339(raw)
                .map(|dt| Value::DateTime(dt.with_timezone(&Utc)))
                .map_err(|e| StorageError::SerializationError(e.to_string())),
            FieldType::Json => serde_json::from_str(raw)
                .map(Value::Json)
                .map_err(|e| StorageError::SerializationError(e.to_string())),
            FieldType::Reference(_) => Ok(Value::Reference(raw.to_string())),
        }
    }

    fn build_where_clause(&self, filter: &Filter) -> String {
        match filter {
            Filter::Eq(field, value) => {
                format!("{} = {}", field, self.value_to_sql_param(value))
            }
            Filter::In(field, values) => {
                let values_str = values
                    .iter()
                    .map(|v| self.value_to_sql_param(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} IN ({})", field, values_str)
            }
            Filter::And(filters) => {
                let clauses = filters
                    .iter()
                    .map(|f| self.build_where_clause(f))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                format!("({})", clauses)
            }
            Filter::Or(filters) => {
                let clauses = filters
                    .iter()
                    .map(|f| self.build_where_clause(f))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                format!("({})", clauses)
            }
            Filter::IsNull(field) => format!("{} IS NULL", field),
            Filter::IsNotNull(field) => format!("{} IS NOT NULL", field),
        }
    }
}

#[async_trait]
impl StorageBackend for SqliteBackend {
    async fn create_entity(&mut self, schema: &EntitySchema) -> Result<()> {
        let mut field_defs = Vec::new();

        for field in &schema.fields {
            let mut def = format!("{} {}", field.name, field.field_type.to_sqlite_type());

            if field.name == schema.primary_key {
                def.push_str(" PRIMARY KEY");
            }

            if field.required {
                def.push_str(" NOT NULL");
            }

            field_defs.push(def);
        }

        field_defs.push("_version TEXT".to_string());
        field_defs.push("_dirty INTEGER DEFAULT 0".to_string());

        let create_table_sql = format!(
            "CREATE TABLE IF NOT EXISTS {} ({})",
            schema.name,
            field_defs.join(", ")
        );

        sqlx::query(&create_table_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        for field in &schema.fields {
            if field.indexed {
                let index_sql = format!(
                    "CREATE INDEX IF NOT EXISTS idx_{}_{} ON {} ({})",
                    schema.name, field.name, schema.name, field.name
                );
                sqlx::query(&index_sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;
            }
        }

        Ok(())
    }

    async fn get(&self, entity: &str, id: &str) -> Result<Option<Entity>> {
        let query = format!("SELECT * FROM {} WHERE id = ?", entity);

        let row = sqlx::query(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        match row {
            Some(row) => {
                let mut entity_data = HashMap::new();

                for (idx, column) in row.columns().iter().enumerate() {
                    let col_name = column.name();
                    if col_name.starts_with('_') {
                        continue;
                    }

                    let value: Option<String> = row
                        .try_get(idx)
                        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

                    if let Some(v) = value {
                        entity_data.insert(col_name.to_string(), Value::String(v));
                    } else {
                        entity_data.insert(col_name.to_string(), Value::Null);
                    }
                }

                Ok(Some(entity_data))
            }
            None => Ok(None),
        }
    }

    async fn query(&self, entity: &str, filter: Filter) -> Result<Vec<Entity>> {
        let where_clause = self.build_where_clause(&filter);
        let query = format!("SELECT * FROM {} WHERE {}", entity, where_clause);

        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let mut results = Vec::new();

        for row in rows {
            let mut entity_data = HashMap::new();

            for (idx, column) in row.columns().iter().enumerate() {
                let col_name = column.name();
                if col_name.starts_with('_') {
                    continue;
                }

                let value: Option<String> = row
                    .try_get(idx)
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

                if let Some(v) = value {
                    entity_data.insert(col_name.to_string(), Value::String(v));
                } else {
                    entity_data.insert(col_name.to_string(), Value::Null);
                }
            }

            results.push(entity_data);
        }

        Ok(results)
    }

    async fn insert(&mut self, entity: &str, data: Entity) -> Result<()> {
        let fields: Vec<_> = data.keys().collect();
        let values: Vec<_> = data.values().map(|v| self.value_to_sql_param(v)).collect();

        let insert_sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            entity,
            fields
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            values.join(", ")
        );

        sqlx::query(&insert_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn update(&mut self, entity: &str, id: &str, data: Entity) -> Result<()> {
        let set_clauses: Vec<_> = data
            .iter()
            .filter(|(k, _)| k.as_str() != "id")
            .map(|(k, v)| format!("{} = {}", k, self.value_to_sql_param(v)))
            .collect();

        let update_sql = format!(
            "UPDATE {} SET {} WHERE id = '{}'",
            entity,
            set_clauses.join(", "),
            id.replace('\'', "''")
        );

        sqlx::query(&update_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn delete(&mut self, entity: &str, id: &str) -> Result<()> {
        let delete_sql = format!("DELETE FROM {} WHERE id = ?", entity);

        sqlx::query(&delete_sql)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn mark_dirty(&mut self, entity: &str, id: &str) -> Result<()> {
        let update_sql = format!("UPDATE {} SET _dirty = 1 WHERE id = ?", entity);

        sqlx::query(&update_sql)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn get_dirty(&self, entity: &str) -> Result<Vec<String>> {
        let query = format!("SELECT id FROM {} WHERE _dirty = 1", entity);

        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let ids = rows
            .iter()
            .map(|row| {
                row.try_get::<String, _>("id")
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(ids)
    }

    async fn mark_clean(&mut self, entity: &str, id: &str) -> Result<()> {
        let update_sql = format!("UPDATE {} SET _dirty = 0 WHERE id = ?", entity);

        sqlx::query(&update_sql)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn get_version(&self, entity: &str, id: &str) -> Result<Option<String>> {
        let query = format!("SELECT _version FROM {} WHERE id = ?", entity);

        let row = sqlx::query(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        match row {
            Some(row) => {
                let version: Option<String> = row
                    .try_get("_version")
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;
                Ok(version)
            }
            None => Ok(None),
        }
    }

    async fn set_version(&mut self, entity: &str, id: &str, version: String) -> Result<()> {
        let update_sql = format!("UPDATE {} SET _version = ? WHERE id = ?", entity);

        sqlx::query(&update_sql)
            .bind(version)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn get_children(
        &self,
        entity: &str,
        parent_field: &str,
        parent_id: &str,
    ) -> Result<Vec<Entity>> {
        let filter = Filter::Eq(
            parent_field.to_string(),
            Value::String(parent_id.to_string()),
        );
        self.query(entity, filter).await
    }

    async fn get_related(
        &self,
        entity: &str,
        foreign_key: &str,
        related_id: &str,
    ) -> Result<Vec<Entity>> {
        let filter = Filter::Eq(
            foreign_key.to_string(),
            Value::String(related_id.to_string()),
        );
        self.query(entity, filter).await
    }
}

#[cfg(test)]
#[path = "sqlite_tests.rs"]
mod tests;
