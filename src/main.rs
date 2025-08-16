// Minimal rmcp 0.5 MCP server exposing SQLite CRUD tools
// Build: cargo build --release
// Run: DATABASE_URL="sqlite:///absolute/path/to/warp.sqlite" target/release/warp-sqlite-mcp

use anyhow::Result;
use regex::Regex;
use rmcp::{ServiceExt, transport::stdio};
use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Parameters},
    model::{CallToolResult, Content, ErrorData, ServerInfo, ServerCapabilities, Implementation, ProtocolVersion},
    ServerHandler,
};
use rmcp_macros::{tool, tool_router, tool_handler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite, Row, Column, ValueRef};
use std::sync::Arc;
use std::future::Future;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

#[derive(Clone)]
struct AppState {
    pool: Pool<Sqlite>,
    ident_re: Regex,
}

fn is_valid_ident(re: &Regex, s: &str) -> bool { re.is_match(s) }

#[derive(Deserialize, JsonSchema)]
struct InsertInput { table: String, values: serde_json::Map<String, Value> }
#[derive(Deserialize, JsonSchema)]
struct SelectInput {
    table: String,
    columns: Option<Vec<String>>,
    #[serde(rename = "where")] r#where: Option<String>,
    params: Option<Vec<Value>>,
    order_by: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}
#[derive(Deserialize, JsonSchema)]
struct UpdateInput {
    table: String,
    set: serde_json::Map<String, Value>,
    #[serde(rename = "where")] r#where: Option<String>,
    params: Option<Vec<Value>>,
}
#[derive(Deserialize, JsonSchema)]
struct DeleteInput { table: String, #[serde(rename = "where")] r#where: Option<String>, params: Option<Vec<Value>> }

// Domain-specific tool inputs
#[derive(Deserialize, JsonSchema)]
struct McpRegisterInput { mcp_server_uuid: String }
#[derive(Deserialize, JsonSchema)]
struct McpUnregisterInput { mcp_server_uuid: String }
#[derive(Deserialize, JsonSchema)]
struct McpSetEnvInput { mcp_server_uuid: String, env: Value }
#[derive(Deserialize, JsonSchema)]
struct McpGetEnvInput { mcp_server_uuid: String }

#[derive(Deserialize, JsonSchema)]
struct NotebookCreateInput { title: Option<String>, body: String }
#[derive(Deserialize, JsonSchema)]
struct NotebookAppendInput { id: i64, delta: String }
#[derive(Deserialize, JsonSchema)]
struct NotebookDeleteInput { id: i64 }
#[derive(Deserialize, JsonSchema)]
struct NotebookListInput { query: Option<String>, limit: Option<i64>, offset: Option<i64> }
#[derive(Deserialize, JsonSchema)]
struct NotebookGetInput { id: i64 }

#[derive(Deserialize)]
struct FileConfig { database: DatabaseConfig }
#[derive(Deserialize)]
struct DatabaseConfig { url: String }

fn load_db_url() -> String {
    if let Ok(v) = std::env::var("DATABASE_URL") { return v; }
    // Try ./config.toml and alongside the executable
    let candidates = [
        std::env::current_dir().ok().map(|p| p.join("config.toml")),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("config.toml"))),
    ];
    for opt in candidates {
        if let Some(path) = opt {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = toml::from_str::<FileConfig>(&text) {
                    return cfg.database.url;
                }
            }
        }
    }
    "sqlite://./app.sqlite".to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    // DATABASE_URL example: sqlite:///Users/samuelatagana/Library/Application Support/dev.warp.Warp-Stable/warp.sqlite
    let db_url = load_db_url();

    let pool = SqlitePoolOptions::new().max_connections(5).connect(&db_url).await?;
    // Best-effort WAL
    let _ = sqlx::query("PRAGMA journal_mode = WAL;").execute(&pool).await;

    let state = Arc::new(AppState {
        pool,
        ident_re: Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap(),
    });

    let service = SqliteService { state, tool_router: SqliteService::tool_router() };
    let server = service.serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

#[derive(Clone)]
struct SqliteService {
    state: Arc<AppState>,
    tool_router: ToolRouter<SqliteService>,
}

#[tool_router]
impl SqliteService {
    #[tool(description = "Insert a row; returns last_insert_rowid")]
    pub async fn sqlite_insert(&self, params: Parameters<InsertInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let state = &self.state;
        if !is_valid_ident(&state.ident_re, &input.table) {
            return Err(ErrorData::invalid_params("Invalid table name".to_string(), None));
        }
        let mut cols = Vec::new();
        let mut binds = Vec::new();
        for (k, v) in input.values.iter() {
            if !is_valid_ident(&state.ident_re, k) {
                return Err(ErrorData::invalid_params(format!("Invalid column: {}", k), None));
            }
            cols.push(k.clone());
            binds.push(v.clone());
        }
        if cols.is_empty() {
            return Err(ErrorData::invalid_params("No columns provided".to_string(), None));
        }
        let placeholders = std::iter::repeat("?").take(cols.len()).collect::<Vec<_>>().join(", ");
        let sql = format!("INSERT INTO {} ({}) VALUES ({})", input.table, cols.join(", "), placeholders);
        let mut q = sqlx::query(&sql);
        for v in binds { q = bind_value(q, v).map_err(|e| ErrorData::internal_error(e.to_string(), None))?; }
        let res = q.execute(&state.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "last_insert_rowid": res.last_insert_rowid() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Select rows; returns rows array of objects")]
    pub async fn sqlite_select(&self, params: Parameters<SelectInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let state = &self.state;
        if !is_valid_ident(&state.ident_re, &input.table) {
            return Err(ErrorData::invalid_params("Invalid table name".to_string(), None));
        }
        let cols = if let Some(list) = &input.columns {
            if list.is_empty() { "*".to_string() } else {
                for c in list { if !is_valid_ident(&state.ident_re, c) { return Err(ErrorData::invalid_params(format!("Invalid column: {}", c), None)); } }
                list.join(", ")
            }
        } else { "*".to_string() };
        let mut sql = format!("SELECT {} FROM {}", cols, input.table);
        if let Some(w) = &input.r#where { sql.push_str(" WHERE "); sql.push_str(w); }
        if let Some(ob) = &input.order_by { sql.push_str(" ORDER BY "); sql.push_str(ob); }
        if let Some(l) = input.limit { sql.push_str(&format!(" LIMIT {}", l)); }
        if let Some(o) = input.offset { sql.push_str(&format!(" OFFSET {}", o)); }
        let mut q = sqlx::query(&sql);
        if let Some(params) = input.params { for p in params { q = bind_value(q, p).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?; } }
        let rows = q.fetch_all(&state.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut out = Vec::<serde_json::Map<String, Value>>::new();
        for row in rows {
            let cols = row.columns();
            let mut obj = serde_json::Map::new();
            for col in cols {
                let name = col.name().to_string();
                let raw = row.try_get_raw(name.as_str());
                let v = match raw {
                    Ok(r) if r.is_null() => Value::Null,
                    Ok(_) => {
                        if let Ok(v) = row.try_get::<i64, _>(name.as_str()) { Value::from(v) }
                        else if let Ok(v) = row.try_get::<f64, _>(name.as_str()) { Value::from(v) }
                        else if let Ok(v) = row.try_get::<String, _>(name.as_str()) { Value::from(v) }
                        else if let Ok(v) = row.try_get::<Vec<u8>, _>(name.as_str()) { Value::from(B64.encode(v)) }
                        else { Value::Null }
                    }
                    Err(_) => Value::Null,
                };
                obj.insert(name, v);
            }
            out.push(obj);
        }
        let content = Content::json(serde_json::json!({ "rows": out }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Update rows; returns affected_row_count")]
    pub async fn sqlite_update(&self, params: Parameters<UpdateInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let state = &self.state;
        if !is_valid_ident(&state.ident_re, &input.table) { return Err(ErrorData::invalid_params("Invalid table name".to_string(), None)); }
        if input.set.is_empty() { return Err(ErrorData::invalid_params("No columns provided in set".to_string(), None)); }
        let mut frags = Vec::new();
        let mut vals = Vec::new();
        for (k, v) in input.set.iter() {
            if !is_valid_ident(&state.ident_re, k) { return Err(ErrorData::invalid_params(format!("Invalid column: {}", k), None)); }
            frags.push(format!("{} = ?", k));
            vals.push(v.clone());
        }
        let mut sql = format!("UPDATE {} SET {}", input.table, frags.join(", "));
        if let Some(w) = &input.r#where { sql.push_str(" WHERE "); sql.push_str(w); }
        let mut q = sqlx::query(&sql);
        for v in vals { q = bind_value(q, v).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?; }
        if let Some(params) = input.params { for p in params { q = bind_value(q, p).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?; } }
        let res = q.execute(&state.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "affected_row_count": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Delete rows; returns affected_row_count")]
    pub async fn sqlite_delete(&self, params: Parameters<DeleteInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let state = &self.state;
        if !is_valid_ident(&state.ident_re, &input.table) { return Err(ErrorData::invalid_params("Invalid table name".to_string(), None)); }
        let mut sql = format!("DELETE FROM {}", input.table);
        if let Some(w) = &input.r#where { sql.push_str(" WHERE "); sql.push_str(w); }
        let mut q = sqlx::query(&sql);
        if let Some(params) = input.params { for p in params { q = bind_value(q, p).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?; } }
        let res = q.execute(&state.pool).await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "affected_row_count": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    // ---- MCP management tools ----
    #[tool(description = "Register an MCP server UUID in active_mcp_servers (idempotent)")]
    pub async fn mcp_register_server(&self, params: Parameters<McpRegisterInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let sql = "INSERT OR IGNORE INTO active_mcp_servers (mcp_server_uuid) VALUES (?1)";
        let res = sqlx::query(sql)
            .bind(input.mcp_server_uuid)
            .execute(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "rows_affected": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Unregister an MCP server UUID from active_mcp_servers")]
    pub async fn mcp_unregister_server(&self, params: Parameters<McpUnregisterInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let sql = "DELETE FROM active_mcp_servers WHERE mcp_server_uuid = ?1";
        let res = sqlx::query(sql)
            .bind(input.mcp_server_uuid)
            .execute(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "rows_affected": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Set environment variables JSON for an MCP server UUID (upsert)")]
    pub async fn mcp_set_env(&self, params: Parameters<McpSetEnvInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let env_text = serde_json::to_string(&input.env).map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let sql = "INSERT INTO mcp_environment_variables (mcp_server_uuid, environment_variables) VALUES (?1, ?2) \
                   ON CONFLICT(mcp_server_uuid) DO UPDATE SET environment_variables=excluded.environment_variables";
        let res = sqlx::query(sql)
            .bind(input.mcp_server_uuid)
            .bind(env_text)
            .execute(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "rows_affected": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Get environment variables JSON for an MCP server UUID")]
    pub async fn mcp_get_env(&self, params: Parameters<McpGetEnvInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let sql = "SELECT environment_variables FROM mcp_environment_variables WHERE mcp_server_uuid = ?1";
        let row = sqlx::query(sql)
            .bind(input.mcp_server_uuid)
            .fetch_optional(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let val = if let Some(r) = row {
            let s: String = r.try_get(0).unwrap_or_default();
            serde_json::from_str::<Value>(&s).unwrap_or(Value::Null)
        } else { Value::Null };
        let content = Content::json(serde_json::json!({ "env": val }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    // ---- Notebook tools ----
    #[tool(description = "Create a notebook with title and body; returns { id }")]
    pub async fn notebook_create(&self, params: Parameters<NotebookCreateInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let title = input.title.unwrap_or_else(|| "".to_string());
        let sql = "INSERT INTO notebooks (title, data) VALUES (?1, ?2)";
        let res = sqlx::query(sql)
            .bind(title)
            .bind(input.body)
            .execute(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "id": res.last_insert_rowid() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Append delta text to a notebook's body; returns rows_affected")]
    pub async fn notebook_append(&self, params: Parameters<NotebookAppendInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let sql = "UPDATE notebooks SET data = COALESCE(data,'') || ?1 WHERE id = ?2";
        let res = sqlx::query(sql)
            .bind(input.delta)
            .bind(input.id)
            .execute(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "rows_affected": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Delete a notebook by id; returns rows_affected")]
    pub async fn notebook_delete(&self, params: Parameters<NotebookDeleteInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let sql = "DELETE FROM notebooks WHERE id = ?1";
        let res = sqlx::query(sql)
            .bind(input.id)
            .execute(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let content = Content::json(serde_json::json!({ "rows_affected": res.rows_affected() }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "List notebooks with optional query on title/body; returns id,title,snippet")]
    pub async fn notebook_list(&self, params: Parameters<NotebookListInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let limit = input.limit.unwrap_or(50).clamp(1, 500);
        let offset = input.offset.unwrap_or(0).max(0);
        let (sql, bind_query) = if let Some(q) = input.query {
            ("SELECT id, title, substr(data,1,200) AS snippet FROM notebooks WHERE (title LIKE ?1 OR data LIKE ?2) ORDER BY id DESC LIMIT ?3 OFFSET ?4", Some(q))
        } else {
            ("SELECT id, title, substr(data,1,200) AS snippet FROM notebooks ORDER BY id DESC LIMIT ?1 OFFSET ?2", None)
        };
        let rows = if let Some(q) = bind_query {
            let like = format!("%{}%", q);
            sqlx::query(sql)
                .bind(&like)
                .bind(&like)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.state.pool)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        } else {
            sqlx::query(sql)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.state.pool)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        };
        let mut out = Vec::new();
        for r in rows {
            let id: i64 = r.try_get("id").unwrap_or_default();
            let title: String = r.try_get("title").unwrap_or_default();
            let snippet: String = r.try_get("snippet").unwrap_or_default();
            out.push(serde_json::json!({"id": id, "title": title, "snippet": snippet}));
        }
        let content = Content::json(serde_json::json!({ "items": out }))
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Get a notebook by id; returns full row")]
    pub async fn notebook_get(&self, params: Parameters<NotebookGetInput>) -> std::result::Result<CallToolResult, ErrorData> {
        let input = params.0;
        let sql = "SELECT id, title, data FROM notebooks WHERE id = ?1";
        let row = sqlx::query(sql)
            .bind(input.id)
            .fetch_optional(&self.state.pool)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let val = if let Some(r) = row {
            let id: i64 = r.try_get("id").unwrap_or_default();
            let title: String = r.try_get("title").unwrap_or_default();
            let data: String = r.try_get("data").unwrap_or_default();
            serde_json::json!({"id": id, "title": title, "data": data})
        } else { serde_json::json!({}) };
        let content = Content::json(val).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![content]))
    }
}

#[tool_handler]
impl ServerHandler for SqliteService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            server_info: Implementation { name: "warp-sqlite-mcp".into(), version: "0.1.0".into() },
            capabilities: ServerCapabilities { tools: Some(Default::default()), ..Default::default() },
            instructions: Some("SQLite CRUD MCP".into()),
        }
    }
}

fn bind_value<'q>(mut q: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>, v: Value)
    -> Result<sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>, anyhow::Error>
{
    use serde_json::Value::*;
    q = match v {
        Null => q.bind(None::<std::string::String>),
        Bool(b) => q.bind(b),
        Number(n) => {
            if let Some(i) = n.as_i64() { q.bind(i) }
            else if let Some(u) = n.as_u64() { q.bind(i64::try_from(u).unwrap_or(i64::MAX)) }
            else if let Some(f) = n.as_f64() { q.bind(f) }
            else { q.bind(None::<i64>) }
        }
        String(s) => q.bind(s),
        Array(_) | Object(_) => q.bind(v.to_string()), // store JSON as text
    };
    Ok(q)
}

