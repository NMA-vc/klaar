pub mod tools;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl RpcResponse {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<Value>, code: i32, message: impl Into<String>, data: Option<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }

    pub fn method_not_found(id: Option<Value>, method: &str) -> Self {
        Self::err(id, -32601, format!("Method not found: {}", method), None)
    }

    pub fn invalid_params(id: Option<Value>, msg: impl Into<String>) -> Self {
        Self::err(id, -32602, msg.into(), None)
    }

    #[allow(dead_code)]
    pub fn internal_error(id: Option<Value>, msg: impl Into<String>) -> Self {
        Self::err(id, -32603, "Internal error", Some(msg.into()))
    }
}

// ---------------------------------------------------------------------------
// MCP content types
// ---------------------------------------------------------------------------

pub fn text_content(text: impl Into<String>) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": false
    })
}

pub fn error_content(text: impl Into<String>) -> Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": true
    })
}
