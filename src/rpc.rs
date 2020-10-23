use anyhow::Error;
use serde_json::{Map, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    sync::mpsc::{channel, Receiver},
};
use surf_n_term::Key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RPCRequest {
    CandidatesExtend { items: Vec<String> },
    CandidatesClear,
    NiddleSet(String),
    Current,
    Terminate,
    KeyBinding { key: Vec<Key>, tag: Value },
    PromptSet(String),
}

impl RPCRequest {
    pub fn from_value(value: Value) -> Result<Self, String> {
        let mut map = match value {
            Value::Object(map) => map,
            _ => return Err(format!("request must be an object: {}", value)),
        };
        let method = match map.get_mut("method").map(|v| v.take()) {
            Some(Value::String(method)) => method,
            _ => return Err("request method must be a string and present".to_string()),
        };
        match method.as_ref() {
            "candidates_extend" => {
                let items = match map.get_mut("items").map(|v| v.take()) {
                    Some(Value::Array(items)) => items
                        .into_iter()
                        .map(|item| match item {
                            Value::String(item) => Ok(item),
                            _ => Err("candidate_extend items must be strings".to_string()),
                        })
                        .collect::<Result<_, _>>()?,
                    _ => {
                        return Err("candidates_extend request must include items field".to_string())
                    }
                };
                Ok(RPCRequest::CandidatesExtend { items })
            }
            "niddle_set" => {
                let niddle = match map.get_mut("niddle").map(|v| v.take()) {
                    Some(Value::String(niddle)) => niddle,
                    _ => return Err("niddle_set request must include niddle field".to_string()),
                };
                Ok(RPCRequest::NiddleSet(niddle))
            }
            "candidates_clear" => Ok(RPCRequest::CandidatesClear),
            "terminate" => Ok(RPCRequest::Terminate),
            "key_binding" => {
                let key = match map.get_mut("key").map(|v| v.take()) {
                    Some(Value::String(key)) => match Key::chord(key) {
                        Err(_) => return Err("key_binding faild to parse key".to_string()),
                        Ok(key) => key,
                    },
                    _ => return Err("key_binding requrest must include ".to_string()),
                };
                let tag = match map.get_mut("tag").map(|v| v.take()) {
                    Some(tag) => tag,
                    _ => return Err("key_binding request must include tag field".to_string()),
                };
                Ok(RPCRequest::KeyBinding { key, tag })
            }
            "prompt_set" => {
                let prompt = match map.get_mut("prompt").map(|v| v.take()) {
                    Some(Value::String(prompt)) => prompt,
                    _ => return Err("prompt_set request must include prompt field".to_string()),
                };
                Ok(RPCRequest::PromptSet(prompt))
            }
            "current" => Ok(RPCRequest::Current),
            _ => Err(format!("unknown request method: {}", method)),
        }
    }

    #[cfg(test)]
    pub fn to_value(&self) -> Value {
        use serde_json::json;
        use std::fmt::Write;

        match self {
            RPCRequest::CandidatesExtend { items } => json!({
                "method": "candidates_extend",
                "items": items,
            }),
            RPCRequest::CandidatesClear => json!({ "method": "candidates_clear" }),
            RPCRequest::Terminate => json!({ "method": "terminate" }),
            RPCRequest::NiddleSet(niddle) => json!({ "method": "niddle_set", "niddle": niddle}),
            RPCRequest::KeyBinding { key, tag } => {
                let mut chord = String::new();
                for (index, key) in key.iter().enumerate() {
                    if index != 0 {
                        chord.push_str(" ");
                    }
                    write!(&mut chord, "{:?}", *key).unwrap();
                }
                json!({ "method": "key_binding", "key": chord, "tag": tag })
            }
            RPCRequest::PromptSet(prompt) => json!({ "method": "prompt_set", "prompt": prompt }),
            RPCRequest::Current => json!({ "method": "current" }),
        }
    }
}

/// Create request receiver channel
///
/// This function will spawn a thread which will parse requests from input
/// `BufRead` object, notify function will also be called on each request received.
///
/// Message protocol
/// <decimal string parsable as usize>\n
/// <json encoded RPCRequest>
/// ...
pub fn rpc_requests<I, N>(input: I, mut notify: N) -> Receiver<Result<RPCRequest, String>>
where
    I: Read + Send + 'static,
    N: FnMut() -> bool + Send + 'static,
{
    let (send, recv) = channel();
    let mut input = BufReader::new(input);
    std::thread::spawn(move || -> Result<(), Error> {
        let mut size_buf = String::new();
        let mut request_buf = Vec::new();
        loop {
            // parse request size
            size_buf.clear();
            input.read_line(&mut size_buf)?;
            if size_buf.is_empty() {
                break;
            }
            let size = match size_buf.trim().parse::<usize>() {
                Ok(size) => size,
                Err(_) => {
                    send.send(Err(format!("failed to parse request size: {}", size_buf)))?;
                    break;
                }
            };

            // parse request
            request_buf.clear();
            request_buf.resize_with(size, || 0u8);
            input.read_exact(request_buf.as_mut_slice())?;
            let request = RPCRequest::from_value(serde_json::from_slice(request_buf.as_slice())?);
            send.send(request)?;
            if !notify() {
                break;
            }
        }
        notify();
        Ok(())
    });
    recv
}

/// Encode JSON Value as a RPC message
pub fn rpc_encode<W: Write>(mut out: W, value: Value) -> Result<(), Error> {
    let message = serde_json::to_vec(&value)?;
    writeln!(&mut out, "{}", message.len())?;
    out.write_all(message.as_slice())?;
    out.flush()?;
    Ok(())
}

pub enum RPCErrorKind {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    Other(i32, String),
}

pub struct RPCError {
    kind: RPCErrorKind,
    data: Option<Value>,
}

impl RPCError {
    pub fn new(kind: RPCErrorKind, data: Option<impl Into<Value>>) -> Self {
        Self {
            kind,
            data: data.map(|data| data.into()),
        }
    }

    pub fn new_other(code: i32, message: String, data: Option<Value>) -> Self {
        Self {
            kind: RPCErrorKind::Other(code, message),
            data,
        }
    }
}

impl From<RPCError> for Value {
    fn from(error: RPCError) -> Self {
        use RPCErrorKind::*;
        let (code, message) = match &error.kind {
            ParseError => (-32700, "Parse error"),
            InvalidRequest => (-32600, "Invalid request"),
            MethodNotFound => (-32601, "Method not found"),
            InvalidParams => (-32602, "Invalid params"),
            InternalError => (-32603, "Internal error"),
            Other(code, message) => (*code, message.as_ref()),
        };
        let mut object = Map::new();
        object.insert("code".to_string(), code.into());
        object.insert("message".to_string(), message.into());
        if let Some(data) = error.data {
            object.insert("data".to_string(), data);
        }
        object.into()
    }
}

pub struct RPCHandler<M> {
    method_handler: M,
}

fn rpc_response(id: Value, result: Result<Value, RPCError>) -> Value {
    let mut object = Map::with_capacity(3);
    object.insert("jsonrpc".to_string(), "2.0".into());
    object.insert("id".to_string(), id);
    match result {
        Ok(value) => object.insert("result".to_string(), value),
        Err(error) => object.insert("error".to_string(), error.into()),
    };
    object.into()
}

impl<M> RPCHandler<M>
where
    M: FnMut(String, Option<Value>) -> Result<Value, RPCError>,
{
    pub fn new(method_handler: M) -> Self {
        Self { method_handler }
    }

    /// Handle JSON-RPC requests according to [sepecification](https://www.jsonrpc.org/specification)
    pub fn handle(&mut self, request: impl AsRef<[u8]>) -> Option<Value> {
        // parse request
        let request_object = match serde_json::from_slice(request.as_ref()) {
            Ok(request) => request,
            Err(error) => {
                let error = RPCError::new(RPCErrorKind::ParseError, Some(error.to_string()));
                return Some(rpc_response(Value::Null, Err(error)));
            }
        };
        let (calls, is_batch) = match request_object {
            Value::Array(calls) if !calls.is_empty() => (calls, true),
            call @ Value::Object(_) => (vec![call], false),
            _ => {
                let error = RPCError::new(
                    RPCErrorKind::InvalidRequest,
                    Some("Request can only be non-empty Array or Object"),
                );
                return Some(rpc_response(Value::Null, Err(error)));
            }
        };
        let mut response = Vec::new();
        for call in calls {
            // make sure call is a JSON object
            let mut call_object = match call {
                Value::Object(object) => object,
                _ => {
                    let error = RPCError::new(
                        RPCErrorKind::InvalidRequest,
                        Some("Request can only be an Object"),
                    );
                    response.push(rpc_response(Value::Null, Err(error)));
                    continue;
                }
            };
            // extract id
            let id = match call_object.get_mut("id").map(|v| v.take()) {
                None => None,
                Some(id) => {
                    if id.is_number() || id.is_string() || id.is_null() {
                        Some(id)
                    } else {
                        let error = RPCError::new(
                            RPCErrorKind::InvalidRequest,
                            Some("Request id can only be String, Number of Null"),
                        );
                        response.push(rpc_response(Value::Null, Err(error)));
                        continue;
                    }
                }
            };
            // check json RPC version
            match call_object.get("jsonrpc") {
                Some(Value::String(version)) => {
                    if version != "2.0" {
                        let error = RPCError::new(
                            RPCErrorKind::InvalidRequest,
                            Some(format!("Unsupported protocol version: {}", version)),
                        );
                        response.push(rpc_response(id.unwrap_or(Value::Null), Err(error)));
                        continue;
                    }
                }
                _ => {
                    let error = RPCError::new(
                        RPCErrorKind::InvalidRequest,
                        Some("Protocol version must be provided and be a string"),
                    );
                    response.push(rpc_response(id.unwrap_or(Value::Null), Err(error)));
                    continue;
                }
            };
            // get method and params
            let method = match call_object.get_mut("method").map(|v| v.take()) {
                Some(Value::String(method)) => method,
                _ => {
                    let error = RPCError::new(
                        RPCErrorKind::InvalidRequest,
                        Some("Method must be provided and be a string"),
                    );
                    response.push(rpc_response(id.unwrap_or(Value::Null), Err(error)));
                    continue;
                }
            };
            let params = call_object.get_mut("params").map(|v| v.take());
            // execute method
            let result = (self.method_handler)(method, params);
            if let Some(id) = id {
                response.push(rpc_response(id, result));
            }
        }
        if is_batch {
            Some(response.into())
        } else {
            response.into_iter().next()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Cursor,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    #[test]
    fn test_rpc_request_encode_decode() -> Result<(), Error> {
        let reference = vec![
            RPCRequest::CandidatesClear,
            RPCRequest::CandidatesExtend {
                items: vec!["one".to_string(), "two".to_string()],
            },
            RPCRequest::Terminate,
            RPCRequest::NiddleSet("test".to_string()),
            RPCRequest::KeyBinding {
                key: Key::chord("ctrl+c")?,
                tag: "test".into(),
            },
            RPCRequest::PromptSet("prompt".to_string()),
            RPCRequest::Current,
        ];

        let mut buf = Cursor::new(Vec::new());
        for request in reference.iter() {
            rpc_encode(buf.get_mut(), request.to_value())?;
        }

        let count = Arc::new(AtomicUsize::new(0));
        let requests = rpc_requests(buf, {
            let count = count.clone();
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                true
            }
        });

        let result = requests.iter().collect::<Result<_, _>>();
        assert_eq!(result, Ok(reference));
        assert_eq!(count.load(Ordering::SeqCst), 8);

        Ok(())
    }
}
