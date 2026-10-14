//! Incoming JSON-RPC envelopes shared by Codex app-server and Grok ACP.
//!
//! The decoder accepts either vendor's envelope. Outgoing builders stay with each adapter:
//! ACP includes `jsonrpc: "2.0"`, while Codex's app-server omits it.

use serde_json::Value;

/// The JSON-RPC error code for unsupported server requests.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// One incoming request, response or notification from a CLI speaking JSON-RPC.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The answer to one of rozu's requests.
    Response {
        id: Value,
        result: Result<Value, String>,
    },
    /// A server → client request rozu must answer.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

impl Incoming {
    /// `None` for something that isn't a JSON-RPC message.
    pub fn parse(message: Value) -> Option<Self> {
        let Value::Object(mut map) = message else {
            return None;
        };
        let method = map.get("method").and_then(Value::as_str).map(str::to_owned);
        let params = map.remove("params").unwrap_or(Value::Null);
        match (method, map.remove("id")) {
            (Some(method), Some(id)) => Some(Self::Request { id, method, params }),
            (Some(method), None) => Some(Self::Notification { method, params }),
            (None, Some(id)) => {
                let result = match (map.remove("result"), map.remove("error")) {
                    (_, Some(error)) => Err(error["message"]
                        .as_str()
                        .map_or_else(|| error.to_string(), str::to_owned)),
                    (Some(result), None) => Ok(result),
                    (None, None) => return None,
                };
                Some(Self::Response { id, result })
            }
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_messages() {
        assert_eq!(
            Incoming::parse(json!({"id": 1, "result": {"ok": true}})),
            Some(Incoming::Response {
                id: json!(1),
                result: Ok(json!({"ok": true}))
            })
        );
        assert_eq!(
            Incoming::parse(json!({"error": {"code": -1, "message": "no"}, "id": 2})),
            Some(Incoming::Response {
                id: json!(2),
                result: Err("no".into())
            })
        );
        assert!(matches!(
            Incoming::parse(json!({"id": "s1", "method": "execCommandApproval", "params": {}})),
            Some(Incoming::Request { method, .. }) if method == "execCommandApproval"
        ));
        assert!(matches!(
            Incoming::parse(json!({"method": "turn/started"})),
            Some(Incoming::Notification {
                params: Value::Null,
                ..
            })
        ));
        assert_eq!(Incoming::parse(json!([1])), None);
        assert_eq!(Incoming::parse(json!({"id": 3})), None);
    }

    #[test]
    fn both_vendor_envelopes_preserve_ids_and_error_precedence() {
        for id in [json!(17), json!("permission-2"), Value::Null] {
            for versioned in [false, true] {
                let mut message = json!({
                    "id": id,
                    "result": {"ignored": true},
                    "error": {"code": -32601, "message": "unsupported request"},
                });
                if versioned {
                    message["jsonrpc"] = json!("2.0");
                }
                assert_eq!(
                    Incoming::parse(message),
                    Some(Incoming::Response {
                        id: id.clone(),
                        result: Err("unsupported request".into()),
                    })
                );
            }
        }
    }
}
