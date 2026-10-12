use crate::{config::Config, filesystem::Filesystem};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
struct Server {
    config: PathBuf,
    gate: Arc<tokio::sync::Semaphore>,
}

fn tools() -> Vec<Tool> {
    let root = json!({"type":"string","description":"Folder name (nickname) or stable id returned by list_roots"});
    let path = json!({"type":"string","default":"","description":"Relative path inside the selected folder"});
    let text = json!({"type":"string","minLength":1,"maxLength":4096});
    let n = |default: u32, max: u32| json!({"type":"integer","minimum":1,"maximum":max,"default":default});
    let definitions = [
        (
            "list_roots",
            "List allowed folders",
            "List configured read-only folders and their availability.",
            json!({}),
            vec![],
        ),
        (
            "list_directory",
            "List directory",
            "List files and subdirectories within a configured folder.",
            json!({"root":root,"path":path,"depth":n(1,5),"limit":n(500,2000)}),
            vec!["root"],
        ),
        (
            "get_file_info",
            "Get file information",
            "Return size, dates, MIME type and URI.",
            json!({"root":root,"path":text}),
            vec!["root", "path"],
        ),
        (
            "search_files",
            "Search file names",
            "Case-insensitive filename search. Use root=all to search all connected folders.",
            json!({"root":{"type":"string","default":"all"},"path":path,"query":text,"limit":n(50,200)}),
            vec!["query"],
        ),
        (
            "search_text",
            "Search inside text files",
            "Unicode case-insensitive search in UTF-8 text. Narrow root/path for large folders. Reports offline roots and incomplete scans.",
            json!({"root":{"type":"string","default":"all"},"path":path,"query":{"type":"string","minLength":2,"maxLength":4096},"limit":n(30,100)}),
            vec!["query"],
        ),
        (
            "read_text_file",
            "Read text file",
            "Read UTF-8 text or selected lines. Prefer Markdown/OCR sidecars for office documents.",
            json!({"root":root,"path":text,"startLine":n(1,u32::MAX),"endLine":{"type":"integer","minimum":1},"maxChars":n(200000,200000)}),
            vec!["root", "path"],
        ),
        (
            "read_file",
            "Read file",
            "Read UTF-8 text or a bounded binary file as an embedded MCP resource.",
            json!({"root":root,"path":text,"maxChars":n(200000,200000)}),
            vec!["root", "path"],
        ),
        (
            "search",
            "Search local documents",
            "Search local document names and text in all configured folders.",
            json!({"query":text}),
            vec!["query"],
        ),
        (
            "fetch",
            "Fetch local document",
            "Read a document returned by search using its stable root:path ID.",
            json!({"id":text}),
            vec!["id"],
        ),
    ];
    definitions.into_iter().map(|(name,title,description,properties,required)|serde_json::from_value(json!({
        "name":name,"title":title,"description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}
    })).expect("static tool schema")).collect()
}
fn result(mut data: Value) -> CallToolResult {
    if data["kind"] == "binary" {
        let blob = data.as_object_mut().unwrap().remove("blob").unwrap();
        serde_json::from_value(json!({"structuredContent":data,"content":[{"type":"resource","resource":{"uri":data["uri"],"mimeType":data["mimeType"],"blob":blob}}]})).expect("binary result")
    } else {
        let text = data
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string(&data).unwrap());
        serde_json::from_value(
            json!({"structuredContent":data,"content":[{"type":"text","text":text}]}),
        )
        .expect("text result")
    }
}
fn failure(code: &str, message: &str) -> CallToolResult {
    serde_json::from_value(json!({"isError":true,"structuredContent":{"error":{"code":code,"message":message}},"content":[{"type":"text","text":format!("{code}: {message}")}]})).expect("error result")
}
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        serde_json::from_value(json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"strigov-local-mcp","title":"Strigov Local MCP","version":env!("CARGO_PKG_VERSION")},"instructions":"Read-only access to explicitly configured local document folders. Start with list_roots; use relative paths and stable folder aliases. Never claim a write succeeded. Prefer Markdown sidecars for PDF, DOCX and scans. Check unavailableRoots and truncated before claiming a complete search."})).expect("server config")
    }
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        // One bounded disk operation at a time: memory does not multiply with concurrent requests.
        let permit = self
            .gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ErrorData::internal_error("Server stopping", None))?;
        let path = self.config.clone();
        let output = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            match Config::load(&path) {
                Ok(config) => match Filesystem::new(config).call(
                    &request.name,
                    &Value::Object(request.arguments.unwrap_or_default()),
                ) {
                    Ok(data) => result(data),
                    Err(e) => failure(e.code, &e.message),
                },
                Err(_) => failure(
                    "CONFIG_UNAVAILABLE",
                    "Folder settings are unavailable or invalid; access is disabled",
                ),
            }
        })
        .await
        .map_err(|_| ErrorData::internal_error("File operation failed", None))?;
        Ok(output.into())
    }
}
pub fn serve(config: PathBuf) -> anyhow::Result<()> {
    Config::load(&config)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?
        .block_on(async {
            let server = Server {
                config,
                gate: Arc::new(tokio::sync::Semaphore::new(1)),
            };
            let running = server.serve(rmcp::transport::stdio()).await?;
            eprintln!("Strigov Local MCP Rust MCP ready (read-only)");
            running.waiting().await?;
            anyhow::Ok(())
        })
}
