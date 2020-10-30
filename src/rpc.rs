use anyhow::Error;
use crossbeam::channel::{unbounded, Receiver};
use serde_json::{json, Map, Value};
use std::{
    convert::TryFrom,
    io::{BufRead, BufReader, Read, Write},
    str::FromStr,
};
use surf_n_term::Key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepRequest {
    CandidatesExtend { items: Vec<String> },
    CandidatesClear,
    NiddleSet(String),
    Current,
    Terminate,
    KeyBinding { key: Vec<Key>, tag: Value },
    PromptSet(String),
}

impl SweepRequest {
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
                Ok(SweepRequest::CandidatesExtend { items })
            }
            "niddle_set" => {
                let niddle = match map.get_mut("niddle").map(|v| v.take()) {
                    Some(Value::String(niddle)) => niddle,
                    _ => return Err("niddle_set request must include niddle field".to_string()),
                };
                Ok(SweepRequest::NiddleSet(niddle))
            }
            "candidates_clear" => Ok(SweepRequest::CandidatesClear),
            "terminate" => Ok(SweepRequest::Terminate),
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
                Ok(SweepRequest::KeyBinding { key, tag })
            }
            "prompt_set" => {
                let prompt = match map.get_mut("prompt").map(|v| v.take()) {
                    Some(Value::String(prompt)) => prompt,
                    _ => return Err("prompt_set request must include prompt field".to_string()),
                };
                Ok(SweepRequest::PromptSet(prompt))
            }
            "current" => Ok(SweepRequest::Current),
            _ => Err(format!("unknown request method: {}", method)),
        }
    }

    #[cfg(test)]
    pub fn to_value(&self) -> Value {
        use std::fmt::Write;

        match self {
            SweepRequest::CandidatesExtend { items } => json!({
                "method": "candidates_extend",
                "items": items,
            }),
            SweepRequest::CandidatesClear => json!({ "method": "candidates_clear" }),
            SweepRequest::Terminate => json!({ "method": "terminate" }),
            SweepRequest::NiddleSet(niddle) => json!({ "method": "niddle_set", "niddle": niddle}),
            SweepRequest::KeyBinding { key, tag } => {
                let mut chord = String::new();
                for (index, key) in key.iter().enumerate() {
                    if index != 0 {
                        chord.push_str(" ");
                    }
                    write!(&mut chord, "{:?}", *key).unwrap();
                }
                json!({ "method": "key_binding", "key": chord, "tag": tag })
            }
            SweepRequest::PromptSet(prompt) => json!({ "method": "prompt_set", "prompt": prompt }),
            SweepRequest::Current => json!({ "method": "current" }),
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
pub fn rpc_requests<I, N>(input: I, mut notify: N) -> Receiver<Result<SweepRequest, String>>
where
    I: Read + Send + 'static,
    N: FnMut() -> bool + Send + 'static,
{
    let (send, recv) = unbounded();
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
            let request = SweepRequest::from_value(serde_json::from_slice(request_buf.as_slice())?);
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
    id: Value,
    data: Option<Value>,
}

impl RPCError {
    pub fn new(kind: RPCErrorKind, id: Option<Value>, data: Option<impl Into<Value>>) -> Self {
        Self {
            kind,
            id: id.unwrap_or(Value::Null),
            data: data.map(|data| data.into()),
        }
    }

    pub fn new_other(code: i32, message: String, id: Option<Value>, data: Option<Value>) -> Self {
        Self {
            kind: RPCErrorKind::Other(code, message),
            id: id.unwrap_or(Value::Null),
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
        let mut error_obj = Map::new();
        error_obj.insert("code".to_string(), code.into());
        error_obj.insert("message".to_string(), message.into());
        if let Some(data) = error.data {
            error_obj.insert("data".to_string(), data);
        }
        json!({
            "jsonrpc": "2.0",
            "error": error_obj,
            "id": error.id,
        })
    }
}

pub struct RPCRequest {
    pub method: String,
    pub params: Value,
    pub id: Option<Value>,
}

impl RPCRequest {
    pub fn response_ok(self, result: impl Into<Value>) -> Option<Value> {
        if let Some(id) = self.id {
            Some(json!({
                "jsonrpc": "2.0",
                "result": result.into(),
                "id": id,
            }))
        } else {
            None
        }
    }

    pub fn response_err(self, kind: RPCErrorKind, data: Option<impl Into<Value>>) -> Value {
        RPCError::new(kind, self.id, data).into()
    }

    pub fn from_value(value: Value) -> Result<Self, RPCError> {
        // make sure call is an object
        let mut request = match value {
            Value::Object(object) => object,
            _ => {
                return Err(RPCError::new(
                    RPCErrorKind::InvalidRequest,
                    None,
                    Some("Request can only be an Object"),
                ));
            }
        };
        // extract id
        let id = request
            .get_mut("id")
            .map(|value| {
                let id = value.take();
                if id.is_number() || id.is_string() || id.is_null() {
                    Ok(id)
                } else {
                    Err(RPCError::new(
                        RPCErrorKind::InvalidRequest,
                        None,
                        Some("Request id can only be String, Number of Null"),
                    ))
                }
            })
            .transpose()?;
        // check json RPC version
        match request.get("jsonrpc") {
            Some(Value::String(version)) => {
                if version != "2.0" {
                    return Err(RPCError::new(
                        RPCErrorKind::InvalidRequest,
                        id,
                        Some(format!("Unsupported protocol version: {}", version)),
                    ));
                }
            }
            _ => {
                return Err(RPCError::new(
                    RPCErrorKind::InvalidRequest,
                    id,
                    Some("Protocol version must be provided and be a string"),
                ));
            }
        };
        // extract method name
        let method = match request.get_mut("method").map(|v| v.take()) {
            Some(Value::String(method)) => method,
            _ => {
                return Err(RPCError::new(
                    RPCErrorKind::InvalidRequest,
                    id,
                    Some("Method must be provided and be a string"),
                ));
            }
        };
        // extract params
        let params = request
            .get_mut("params")
            .map_or_else(|| Value::Null, |v| v.take());
        Ok(Self { method, params, id })
    }
}

impl<'a> TryFrom<&'a [u8]> for RPCRequest {
    type Error = RPCError;

    fn try_from(slice: &'a [u8]) -> Result<Self, Self::Error> {
        match serde_json::from_slice(slice) {
            Ok(value) => Self::from_value(value),
            Err(error) => Err(RPCError::new(
                RPCErrorKind::ParseError,
                None,
                Some(error.to_string()),
            )),
        }
    }
}

impl FromStr for RPCRequest {
    type Err = RPCError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.as_bytes())
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
            SweepRequest::CandidatesClear,
            SweepRequest::CandidatesExtend {
                items: vec!["one".to_string(), "two".to_string()],
            },
            SweepRequest::Terminate,
            SweepRequest::NiddleSet("test".to_string()),
            SweepRequest::KeyBinding {
                key: Key::chord("ctrl+c")?,
                tag: "test".into(),
            },
            SweepRequest::PromptSet("prompt".to_string()),
            SweepRequest::Current,
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
