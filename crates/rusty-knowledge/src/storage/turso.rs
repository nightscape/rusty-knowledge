use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use turso_core::{Database, DatabaseOpts, OpenFlags, MemoryIO};
#[cfg(target_family = "unix")]
use turso_core::UnixIO;

use crate::api::streaming::{Change, ChangeOrigin};
use crate::storage::{
    backend::StorageBackend,
    schema::{EntitySchema, FieldType},
    types::{StorageEntity, Filter, Result, StorageError, Value},
};

/// A change notification from a materialized view
///
/// Note: The row_changes() method automatically coalesces DELETE+INSERT pairs
/// into UPDATE events to prevent UI flicker when materialized views are updated.
///
/// **IMPORTANT - UI Keying Requirements**:
///
/// The `id` field in `ChangeData` is the SQLite ROWID, which is:
/// - Unique per view (not globally unique)
/// - Can be reused after DELETE operations
/// - Used for transport and coalescing only
///
/// **UI MUST KEY BY ENTITY ID from `data.get("id")`, NOT BY ROWID**
///
/// Example:
/// ```rust
/// match change.change {
///     ChangeData::Created { data, .. } => {
///         let entity_id = data.get("id").unwrap(); // Use this for widget key
///         // Don't use ROWID (from `data.get("_rowid")`) as widget key!
///     }
///     ChangeData::Updated { id: rowid, data, .. } => {
///         let entity_id = data.get("id").unwrap(); // Use this for widget key
///         // Don't use `rowid` as widget key!
///     }
///     ChangeData::Deleted { id: rowid, .. } => {
///         // For deletes, you may need to track entity_id separately
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct RowChange {
    pub relation_name: String,
    pub change: ChangeData,
}

/// The type of change and associated data
///
/// **WARNING**: For `Created` and `Updated` variants, the ROWID is stored in `data["_rowid"]`.
/// For `Deleted`, the `id` field is the SQLite ROWID, not the entity ID.
/// See `RowChange` documentation for UI keying requirements.
pub type ChangeData = Change<StorageEntity>;

/// Stream of view changes
pub type RowChangeStream = ReceiverStream<RowChange>;

/// Batches and coalesces CDC events to prevent UI flicker from DELETE+INSERT pairs
struct CdcCoalescer {
    changes: Vec<Option<RowChange>>,
    pending_deletes: HashMap<(String, String), usize>,
    pending_inserts: HashMap<(String, String), usize>,
}

impl CdcCoalescer {
    fn new() -> Self {
        Self {
            changes: Vec::new(),
            pending_deletes: HashMap::new(),
            pending_inserts: HashMap::new(),
        }
    }

    fn add(&mut self, change: RowChange) {
        self.changes.push(Some(change));
    }

    fn flush(&mut self) -> Vec<RowChange> {
        for idx in 0..self.changes.len() {
            if let Some(change) = self.changes[idx].clone() {
                let key = (change.relation_name.clone(),
                           match &change.change {
                               ChangeData::Deleted { id, .. } => id.clone(),
                               ChangeData::Created { data, .. } => {
                                   // Extract ROWID from _rowid field
                                   data.get("_rowid")
                                       .and_then(|v| match v {
                                           Value::String(s) => Some(s.clone()),
                                           _ => None,
                                       })
                                       .unwrap_or_else(|| "".to_string())
                               },
                               ChangeData::Updated { id, .. } => id.clone(),
                           });

                match &change.change {
                    ChangeData::Deleted { .. } => {
                        // Check if there's a pending INSERT for same key
                        if let Some(insert_idx) = self.pending_inserts.remove(&key) {
                            // INSERT then DELETE → no-op (drop both)
                            self.changes[insert_idx] = None;
                            self.changes[idx] = None;
                        } else {
                            // Track DELETE in case INSERT follows
                            self.pending_deletes.insert(key, idx);
                        }
                    }
                    ChangeData::Created { data, origin } => {
                        // Extract ROWID for coalescing
                        let rowid = data.get("_rowid")
                            .and_then(|v| match v {
                                Value::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .unwrap_or_else(|| "".to_string());

                        // Check if there's a pending DELETE for same key
                        if let Some(delete_idx) = self.pending_deletes.remove(&key) {
                            // DELETE then INSERT → UPDATE
                            self.changes[delete_idx] = None;
                            self.changes[idx] = Some(RowChange {
                                relation_name: change.relation_name.clone(),
                                change: ChangeData::Updated {
                                    id: rowid,
                                    data: data.clone(),
                                    origin: *origin,
                                },
                            });
                        } else {
                            // Track INSERT in case DELETE follows
                            self.pending_inserts.insert(key, idx);
                        }
                    }
                    ChangeData::Updated { .. } => {}
                }
            }
        }

        self.pending_deletes.clear();
        self.pending_inserts.clear();
        self.changes.drain(..).flatten().collect()
    }
}

#[derive(Debug)]
pub struct TursoBackend {
    db: Arc<Database>,
}

/// Turso-based storage backend
/// Note that this is the Turso Database, not Turso libsql.
///
/// From the docs:
/// How is Turso Database different from Turso's libSQL?
/// Turso Database is a project to build the next evolution of SQLite in Rust, with a strong open contribution focus and features like native async support, vector search, and more.
/// The libSQL project is also an attempt to evolve SQLite in a similar direction, but through a fork rather than a rewrite.
/// Rewriting SQLite in Rust started as an unassuming experiment, and due to its incredible success, replaces libSQL as our intended direction.
impl TursoBackend {
    /// Create a new file-based TursoBackend
    ///
    /// # Platform Support
    /// - **Unix-like systems** (macOS, Linux, BSD, iOS): Full file-based storage support via UnixIO
    /// - **Windows**: Not yet supported - falls back to in-memory storage
    ///
    /// The turso-core library currently does not export a public cross-platform IO implementation.
    /// Windows support will be added once turso-core exposes the necessary APIs.
    pub async fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        #[cfg(target_family = "unix")]
        {
            let io = Arc::new(UnixIO::new()
                .map_err(|e| StorageError::DatabaseError(e.to_string()))?);
            let opts = DatabaseOpts::default()
                .with_views(true);

            let db = Database::open_file_with_flags(
                io,
                db_path.as_ref().to_str()
                    .ok_or_else(|| StorageError::DatabaseError("Invalid path".to_string()))?,
                OpenFlags::default(),
                opts,
                None,
            ).map_err(|e| StorageError::DatabaseError(e.to_string()))?;

            Ok(Self { db })
        }
        #[cfg(not(target_family = "unix"))]
        {
            // Windows/other platforms: fall back to in-memory until turso-core exports cross-platform IO
            eprintln!("Warning: File-based storage not yet supported on this platform. Using in-memory storage.");
            let _ = db_path; // Suppress unused variable warning
            Self::new_in_memory().await
        }
    }

    pub async fn new_in_memory() -> Result<Self> {
        let io = Arc::new(MemoryIO::new());
        let opts = DatabaseOpts::default()
            .with_views(true); // Enable experimental views

        let db = Database::open_file_with_flags(
            io,
            ":memory:",
            OpenFlags::default(),
            opts,
            None,
        ).map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(Self { db })
    }

    pub fn get_connection(&self) -> Result<turso::Connection> {
        let conn_core = self.db.connect()
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;
        Ok(turso::Connection::create(conn_core))
    }

    /// Set up a stream to receive view change notifications
    /// Returns a connection (which must be kept alive) and a stream of changes
    ///
    /// Uses a bounded channel (capacity 1024) to prevent memory exhaustion under bursty changes.
    /// If the channel fills up, new events are dropped with a warning.
    pub fn row_changes(&self) -> Result<(turso::Connection, RowChangeStream)> {
        let conn = self.get_connection()?;
        let (tx, rx) = mpsc::channel(1024);

        conn.set_view_change_callback(move |event: &turso_core::types::RelationChangeEvent| {
            let mut coalescer = CdcCoalescer::new();

            for change in &event.changes {
                let change_data = match &change.change {
                    turso_core::DatabaseChangeType::Insert { .. } => {
                        if let Some(values) = change.parse_record() {
                            let mut data = Self::parse_row_values_with_schema(&values, &event.columns);
                            // Store ROWID in data for coalescing
                            data.insert("_rowid".to_string(), Value::String(change.id.to_string()));
                            ChangeData::Created {
                                data,
                                origin: ChangeOrigin::Remote,
                            }
                        } else {
                            continue;
                        }
                    }
                    turso_core::DatabaseChangeType::Update { .. } => {
                        if let Some(values) = change.parse_record() {
                            let mut data = Self::parse_row_values_with_schema(&values, &event.columns);
                            // Store ROWID in data for coalescing
                            data.insert("_rowid".to_string(), Value::String(change.id.to_string()));
                            ChangeData::Updated {
                                id: change.id.to_string(),
                                data,
                                origin: ChangeOrigin::Remote,
                            }
                        } else {
                            continue;
                        }
                    }
                    turso_core::DatabaseChangeType::Delete => {
                        // Use ROWID as the identifier
                        ChangeData::Deleted {
                            id: change.id.to_string(),
                            origin: ChangeOrigin::Remote,
                        }
                    }
                };

                let view_change = RowChange {
                    relation_name: event.relation_name.clone(),
                    change: change_data,
                };

                coalescer.add(view_change);
            }

            for coalesced_change in coalescer.flush() {
                if tx.try_send(coalesced_change).is_err() {
                    eprintln!("Warning: View change stream full (UI is behind), dropping event");
                }
            }
        })
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok((conn, ReceiverStream::new(rx)))
    }

    /// Helper to parse a row of turso_core::Value into our Entity type using schema
    fn parse_row_values_with_schema(values: &[turso_core::Value], columns: &[String]) -> StorageEntity {
        let mut entity = StorageEntity::new();

        for (idx, value) in values.iter().enumerate() {
            let our_value = match value {
                turso_core::Value::Null => Value::Null,
                turso_core::Value::Integer(i) => Value::Integer(*i),
                turso_core::Value::Float(f) => Value::String(f.to_string()),
                turso_core::Value::Text(s) => Value::String(s.to_string()),
                turso_core::Value::Blob(_) => Value::Null,
            };

            // Use column name from schema, or fall back to col_N if schema is incomplete
            let column_name = columns.get(idx)
                .map(|s| s.as_str())
                .unwrap_or_else(|| {
                    eprintln!("Warning: Column index {} exceeds schema length {}", idx, columns.len());
                    "unknown"
                });

            entity.insert(column_name.to_string(), our_value);
        }
        entity
    }

    pub fn value_to_sql_param(&self, value: &Value) -> String {
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

    fn value_to_turso_param(&self, value: &Value) -> turso::Value {
        match value {
            Value::String(s) => turso::Value::Text(s.clone()),
            Value::Integer(i) => turso::Value::Integer(*i),
            Value::Boolean(b) => turso::Value::Integer(if *b { 1 } else { 0 }),
            Value::DateTime(dt) => turso::Value::Text(dt.to_rfc3339()),
            Value::Json(j) => turso::Value::Text(serde_json::to_string(j).unwrap()),
            Value::Reference(r) => turso::Value::Text(r.clone()),
            Value::Null => turso::Value::Null,
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

    fn build_where_clause(&self, filter: &Filter, params: &mut Vec<turso::Value>) -> String {
        match filter {
            Filter::Eq(field, value) => {
                params.push(self.value_to_turso_param(value));
                format!("{} = ?", field)
            }
            Filter::In(field, values) => {
                let placeholders = values
                    .iter()
                    .map(|v| {
                        params.push(self.value_to_turso_param(v));
                        "?"
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} IN ({})", field, placeholders)
            }
            Filter::And(filters) => {
                let clauses = filters
                    .iter()
                    .map(|f| self.build_where_clause(f, params))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                format!("({})", clauses)
            }
            Filter::Or(filters) => {
                let clauses = filters
                    .iter()
                    .map(|f| self.build_where_clause(f, params))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                format!("({})", clauses)
            }
            Filter::IsNull(field) => format!("{} IS NULL", field),
            Filter::IsNotNull(field) => format!("{} IS NOT NULL", field),
        }
    }

    fn turso_value_to_value(&self, value: turso_core::Value) -> Value {
        match value {
            turso_core::Value::Null => Value::Null,
            turso_core::Value::Integer(i) => Value::Integer(i),
            turso_core::Value::Float(f) => Value::String(f.to_string()),
            turso_core::Value::Text(s) => Value::String(s.to_string()),
            turso_core::Value::Blob(_) => Value::Null,
        }
    }

    /// Execute raw SQL query with parameter binding
    ///
    /// Supports named parameters ($param_name) which are replaced with positional placeholders.
    /// Returns a vector of Entity (HashMap<String, Value>) representing the result rows.
    pub async fn execute_sql(
        &self,
        sql: &str,
        params: HashMap<String, Value>,
    ) -> Result<Vec<StorageEntity>> {
        let conn = self.get_connection()?;

        // Replace named parameters ($param_name) with positional placeholders (?)
        let (sql_with_placeholders, param_values) = self.bind_parameters(sql, &params)?;

        // Prepare and execute the statement
        let mut stmt = conn.prepare(&sql_with_placeholders)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let columns = stmt.columns();

        let mut rows = stmt.query(param_values)
            .await
            .map_err(|e| StorageError::QueryError(e.to_string()))?;

        // Collect results
        let mut results = Vec::new();
        while let Some(row) = rows.next()
            .await
            .map_err(|e| StorageError::QueryError(e.to_string()))?
        {
            let mut entity = StorageEntity::new();

            for (idx, column) in columns.iter().enumerate() {
                let col_name = column.name();
                let value = row.get_value(idx)
                    .map_err(|e| StorageError::QueryError(e.to_string()))?;

                entity.insert(col_name.to_string(), self.turso_value_to_value(value.into()));
            }

            results.push(entity);
        }

        Ok(results)
    }

    /// Bind named parameters in SQL ($param_name) to positional placeholders (?)
    ///
    /// Returns the modified SQL and a Vec of parameter values in the correct order.
    fn bind_parameters(&self, sql: &str, params: &HashMap<String, Value>) -> Result<(String, Vec<turso::Value>)> {
        let mut result_sql = String::with_capacity(sql.len());
        let mut param_values = Vec::new();
        let mut chars = sql.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '$' {
                // Extract parameter name
                let mut param_name = String::new();
                while let Some(&next_ch) = chars.peek() {
                    if next_ch.is_alphanumeric() || next_ch == '_' {
                        param_name.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }

                // Look up parameter value
                if let Some(value) = params.get(&param_name) {
                    result_sql.push('?');
                    param_values.push(self.value_to_turso_param(value));
                } else {
                    return Err(StorageError::QueryError(format!("Parameter ${} not found", param_name)));
                }
            } else {
                result_sql.push(ch);
            }
        }

        Ok((result_sql, param_values))
    }
}

#[async_trait]
impl StorageBackend for TursoBackend {
    async fn create_entity(&mut self, schema: &EntitySchema) -> Result<()> {
        let conn = self.get_connection()?;

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

        let create_table_sql = format!(
            "CREATE TABLE IF NOT EXISTS {} ({})",
            schema.name,
            field_defs.join(", ")
        );

        conn.execute(&create_table_sql, ())
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        for field in &schema.fields {
            if field.indexed {
                let index_sql = format!(
                    "CREATE INDEX IF NOT EXISTS idx_{}_{} ON {} ({})",
                    schema.name, field.name, schema.name, field.name
                );
                conn.execute(&index_sql, ())
                    .await
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;
            }
        }

        Ok(())
    }

    async fn get(&self, entity: &str, id: &str) -> Result<Option<StorageEntity>> {
        let conn = self.get_connection()?;

        let query_str = format!("SELECT * FROM {} WHERE id = ?", entity);

        let mut stmt = conn.prepare(&query_str)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let columns = stmt.columns();

        let mut rows = stmt.query([turso::Value::Text(id.to_string())])
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        if let Some(row) = rows.next()
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?
        {
            let mut entity_data = HashMap::new();

            for (idx, column) in columns.iter().enumerate() {
                let col_name = column.name();

                if col_name.starts_with('_') {
                    continue;
                }

                let value = row.get_value(idx)
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

                entity_data.insert(col_name.to_string(), self.turso_value_to_value(value.into()));
            }

            Ok(Some(entity_data))
        } else {
            Ok(None)
        }
    }

    async fn query(&self, entity: &str, filter: Filter) -> Result<Vec<StorageEntity>> {
        let conn = self.get_connection()?;

        let mut params = Vec::new();
        let where_clause = self.build_where_clause(&filter, &mut params);
        let query_str = format!("SELECT * FROM {} WHERE {}", entity, where_clause);

        let mut stmt = conn.prepare(&query_str)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let columns = stmt.columns();

        let mut rows = stmt.query(params)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let mut results = Vec::new();

        while let Some(row) = rows.next()
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?
        {
            let mut entity_data = HashMap::new();

            for (idx, column) in columns.iter().enumerate() {
                let col_name = column.name();

                if col_name.starts_with('_') {
                    continue;
                }

                let value = row.get_value(idx)
                    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

                entity_data.insert(col_name.to_string(), self.turso_value_to_value(value.into()));
            }

            results.push(entity_data);
        }

        Ok(results)
    }

    async fn insert(&mut self, entity: &str, data: StorageEntity) -> Result<()> {
        let conn = self.get_connection()?;

        let fields: Vec<_> = data.keys().collect();
        let placeholders: Vec<_> = (1..=fields.len()).map(|_| "?").collect();

        let insert_sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            entity,
            fields
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare(&insert_sql)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let params: Vec<turso::Value> = data.values().map(|v| self.value_to_turso_param(v)).collect();

        stmt.execute(params)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn update(&mut self, entity: &str, id: &str, data: StorageEntity) -> Result<()> {
        let conn = self.get_connection()?;

        let filtered_data: Vec<_> = data
            .iter()
            .filter(|(k, _)| k.as_str() != "id")
            .collect();

        let set_clauses: Vec<_> = filtered_data
            .iter()
            .map(|(k, _)| format!("{} = ?", k))
            .collect();

        let update_sql = format!(
            "UPDATE {} SET {} WHERE id = ?",
            entity,
            set_clauses.join(", ")
        );

        let mut stmt = conn.prepare(&update_sql)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let mut params: Vec<turso::Value> = filtered_data
            .iter()
            .map(|(_, v)| self.value_to_turso_param(v))
            .collect();
        params.push(turso::Value::Text(id.to_string()));

        stmt.execute(params)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn delete(&mut self, entity: &str, id: &str) -> Result<()> {
        let conn = self.get_connection()?;

        let delete_sql = format!("DELETE FROM {} WHERE id = ?", entity);

        let mut stmt = conn.prepare(&delete_sql)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        stmt.execute([turso::Value::Text(id.to_string())])
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }


    async fn get_version(&self, entity: &str, id: &str) -> Result<Option<String>> {
        let conn = self.get_connection()?;

        let query = format!("SELECT _version FROM {} WHERE id = ?", entity);

        let mut stmt = conn.prepare(&query)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        let mut rows = stmt.query([turso::Value::Text(id.to_string())])
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        if let Some(row) = rows.next()
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?
        {
            let value = row.get_value(0)
                .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

            match value {
                turso::Value::Text(s) => Ok(Some(s)),
                turso::Value::Null => Ok(None),
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    async fn set_version(&mut self, entity: &str, id: &str, version: String) -> Result<()> {
        let conn = self.get_connection()?;

        let update_sql = format!("UPDATE {} SET _version = ? WHERE id = ?", entity);

        let mut stmt = conn.prepare(&update_sql)
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        stmt.execute([turso::Value::Text(version), turso::Value::Text(id.to_string())])
            .await
            .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

        Ok(())
    }

    async fn get_children(
        &self,
        entity: &str,
        parent_field: &str,
        parent_id: &str,
    ) -> Result<Vec<StorageEntity>> {
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
    ) -> Result<Vec<StorageEntity>> {
        let filter = Filter::Eq(
            foreign_key.to_string(),
            Value::String(related_id.to_string()),
        );
        self.query(entity, filter).await
    }
}

#[cfg(test)]
#[path = "turso_tests.rs"]
mod turso_tests;

#[cfg(test)]
#[path = "turso_pbt_tests.rs"]
mod turso_pbt_tests;

#[cfg(test)]
mod cdc_coalescer_tests {
    use super::*;

    fn make_insert(view: &str, id: &str, value: &str) -> RowChange {
        let mut data = StorageEntity::new();
        data.insert("id".to_string(), Value::String(id.to_string()));
        data.insert("value".to_string(), Value::String(value.to_string()));
        data.insert("_rowid".to_string(), Value::String(id.to_string()));
        RowChange {
            relation_name: view.to_string(),
            change: ChangeData::Created { data, origin: ChangeOrigin::Remote },
        }
    }

    fn make_delete(view: &str, id: &str) -> RowChange {
        RowChange {
            relation_name: view.to_string(),
            change: ChangeData::Deleted { id: id.to_string(), origin: ChangeOrigin::Remote },
        }
    }

    fn make_update(view: &str, id: &str, value: &str) -> RowChange {
        let mut data = StorageEntity::new();
        data.insert("id".to_string(), Value::String(id.to_string()));
        data.insert("value".to_string(), Value::String(value.to_string()));
        data.insert("_rowid".to_string(), Value::String(id.to_string()));
        RowChange {
            relation_name: view.to_string(),
            change: ChangeData::Updated { id: id.to_string(), data, origin: ChangeOrigin::Remote },
        }
    }

    #[test]
    fn test_coalesce_delete_insert_becomes_update() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_delete("view1", "id1"));
        coalescer.add(make_insert("view1", "id1", "new_value"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 1);
        match &result[0].change {
            ChangeData::Updated { id, data, .. } => {
                assert_eq!(id, "id1");
                assert_eq!(data.get("value").unwrap(), &Value::String("new_value".to_string()));
            }
            _ => panic!("Expected Update, got {:?}", result[0].change),
        }
    }

    #[test]
    fn test_coalesce_standalone_delete_unchanged() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_delete("view1", "id1"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].change, ChangeData::Deleted { .. }));
    }

    #[test]
    fn test_coalesce_standalone_insert_unchanged() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_insert("view1", "id1", "value1"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].change, ChangeData::Created { .. }));
    }

    #[test]
    fn test_coalesce_update_unchanged() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_update("view1", "id1", "value1"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0].change, ChangeData::Updated { .. }));
    }

    #[test]
    fn test_coalesce_multiple_different_ids() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_delete("view1", "id1"));
        coalescer.add(make_insert("view1", "id1", "new1"));
        coalescer.add(make_delete("view1", "id2"));
        coalescer.add(make_insert("view1", "id2", "new2"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 2);
        for change in &result {
            assert!(matches!(change.change, ChangeData::Updated { .. }));
        }
    }

    #[test]
    fn test_coalesce_different_views_not_coalesced() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_delete("view1", "id1"));
        coalescer.add(make_insert("view2", "id1", "value1"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 2);
        assert!(matches!(result[0].change, ChangeData::Deleted { .. }));
        assert!(matches!(result[1].change, ChangeData::Created { .. }));
    }

    #[test]
    fn test_coalesce_insert_delete_different_id() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_delete("view1", "id1"));
        coalescer.add(make_insert("view1", "id2", "value"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_coalesce_insert_delete_becomes_noop() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_insert("view1", "id1", "value1"));
        coalescer.add(make_delete("view1", "id1"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 0, "INSERT then DELETE should result in no-op");
    }

    #[test]
    fn test_coalesce_insert_delete_insert_becomes_update() {
        let mut coalescer = CdcCoalescer::new();
        coalescer.add(make_insert("view1", "id1", "value1"));
        coalescer.add(make_delete("view1", "id1"));
        coalescer.add(make_insert("view1", "id1", "value2"));

        let result = coalescer.flush();
        assert_eq!(result.len(), 1);
        match &result[0].change {
            ChangeData::Created { data, .. } => {
                assert_eq!(data.get("value").unwrap(), &Value::String("value2".to_string()));
            }
            _ => panic!("Expected Created, got {:?}", result[0].change),
        }
    }
}
