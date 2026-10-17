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
            "Read UTF-8 text; PNG/JPEG/GIF/WebP images as MCP image content; other bounded binary files as an embedded resource.",
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
fn summary(data: &Value) -> String {
    let mut text = format!(
        "{} ({}, {} bytes)",
        data["path"].as_str().unwrap_or("file"),
        data["mimeType"]
            .as_str()
            .unwrap_or("application/octet-stream"),
        data["size"].as_u64().unwrap_or(0),
    );
    if let (Some(width), Some(height)) = (data["width"].as_u64(), data["height"].as_u64()) {
        text.push_str(&format!(", {width}×{height}"));
    }
    if let Some(page) = data["page"].as_u64() {
        text.push_str(&format!(", page {page}"));
    }
    if let Some(region) = data.get("region") {
        text.push_str(&format!(", region {region}"));
    }
    text.replace('\n', "\\n").replace('\r', "\\r")
}
fn result(mut data: Value) -> CallToolResult {
    if data["kind"] == "image" {
        let image = data.as_object_mut().unwrap().remove("data").unwrap();
        serde_json::from_value(json!({"structuredContent":data,"content":[{"type":"text","text":summary(&data)},{"type":"image","data":image,"mimeType":data["mimeType"]}]})).expect("image result")
    } else if data["kind"] == "binary" {
        let blob = data.as_object_mut().unwrap().remove("blob").unwrap();
        serde_json::from_value(json!({"structuredContent":data,"content":[{"type":"text","text":summary(&data)},{"type":"resource","resource":{"uri":data["uri"],"mimeType":data["mimeType"],"blob":blob}}]})).expect("binary result")
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
        serde_json::from_value(json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"strigov-local-mcp","title":"Strigov Local MCP","version":env!("CARGO_PKG_VERSION")},"instructions":"Read-only access to explicitly configured local document folders. Start with list_roots; use relative paths and stable folder aliases. Never claim a write succeeded. Prefer Markdown sidecars for PDF, DOCX and scans. Check unavailableRoots and truncated before claiming a complete search. PNG/JPEG/GIF/WebP images are returned as MCP image content with a text summary."})).expect("server config")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_result_has_summary_then_image_without_duplicate_payload() {
        let data = json!({"kind":"image","root":"work","path":"work/probe.png",
            "uri":"mcp://local-files/work/probe.png","mimeType":"image/png","size":3,
            "data":"/wAB","width":900,"height":300,"page":2,
            "region":{"x":0,"y":0,"width":1,"height":1}});
        let output = serde_json::to_value(result(data.clone())).unwrap();
        assert_eq!(output["content"].as_array().unwrap().len(), 2);
        assert_eq!(output["content"][0]["type"], "text");
        let summary = output["content"][0]["text"].as_str().unwrap();
        for part in [
            "work/probe.png",
            "image/png",
            "3 bytes",
            "900×300",
            "page 2",
            "region",
        ] {
            assert!(summary.contains(part), "{summary}");
        }
        assert!(!summary.contains('\n'));
        assert_eq!(
            output["content"][1],
            json!({"type":"image","data":"/wAB","mimeType":"image/png"})
        );
        let mut metadata = data;
        metadata.as_object_mut().unwrap().remove("data");
        assert_eq!(output["structuredContent"], metadata);
    }

    #[test]
    fn binary_result_has_summary_then_resource_without_duplicate_payload() {
        let data = json!({"kind":"binary","root":"work","path":"work/probe.pdf",
            "uri":"mcp://local-files/work/probe.pdf","mimeType":"application/pdf","size":3,"blob":"/wAB"});
        let output = serde_json::to_value(result(data.clone())).unwrap();
        assert_eq!(output["content"].as_array().unwrap().len(), 2);
        assert_eq!(output["content"][0]["type"], "text");
        let summary = output["content"][0]["text"].as_str().unwrap();
        for part in ["work/probe.pdf", "application/pdf", "3 bytes"] {
            assert!(summary.contains(part), "{summary}");
        }
        assert_eq!(
            output["content"][1],
            json!({"type":"resource","resource":{
            "uri":"mcp://local-files/work/probe.pdf","mimeType":"application/pdf","blob":"/wAB"}})
        );
        let mut metadata = data;
        metadata.as_object_mut().unwrap().remove("blob");
        assert_eq!(output["structuredContent"], metadata);
    }

    #[test]
    fn text_and_json_results_are_unchanged() {
        for (data, expected) in [
            (json!({"kind":"text","text":"Привет 😀"}), "Привет 😀"),
            (json!({"roots":[]}), r#"{"roots":[]}"#),
        ] {
            let output = serde_json::to_value(result(data.clone())).unwrap();
            assert_eq!(output["structuredContent"], data);
            assert_eq!(output["content"], json!([{"type":"text","text":expected}]));
        }
    }

    #[test]
    fn binary_summary_stays_on_one_line_for_unusual_logical_names() {
        let output = serde_json::to_value(result(json!({"kind":"binary","root":"work",
            "path":"work/line\nbreak\r.bin","uri":"mcp://local-files/work/line%0Abreak%0D.bin",
            "mimeType":"application/octet-stream","size":3,"blob":"/wAB"})))
        .unwrap();
        assert_eq!(
            output["content"][0]["text"],
            "work/line\\nbreak\\r.bin (application/octet-stream, 3 bytes)"
        );
        assert_eq!(
            output["structuredContent"]["path"],
            "work/line\nbreak\r.bin"
        );
    }
}
