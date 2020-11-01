use anyhow::Error;
use crossbeam::channel::{unbounded, Receiver};
use serde_json::{json, Map, Value};
use std::{
    convert::TryFrom,
    io::{BufRead, BufReader, Read, Write},
    str::FromStr,
};

/// Create request receiver channel
///
/// This function will spawn a thread which will parse requests from input
/// `BufRead` object, notify function will also be called on each request received.
///
/// Message protocol
/// <decimal string parsable as usize>\n
/// <json encoded RPCRequest>
/// ...
pub fn rpc_decode<I, N>(input: I, mut notify: N) -> Receiver<Result<RPCRequest, RPCError>>
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
                    send.send(Err(RPCError::new(
                        RPCErrorKind::ParseError,
                        None,
                        Some("failed to parse request size"),
                    )))?;
                    break;
                }
            };

            // parse request
            request_buf.clear();
            request_buf.resize_with(size, || 0u8);
            input.read_exact(request_buf.as_mut_slice())?;
            send.send(RPCRequest::try_from(request_buf.as_slice()))?;
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

pub fn rpc_call<W: Write>(
    out: W,
    method: impl AsRef<str>,
    params: impl Into<Value>,
) -> Result<(), Error> {
    rpc_encode(
        out,
        json!({
            "jsonrpc": "2.0",
            "method": method.as_ref(),
            "params": params.into(),
        }),
    )
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
