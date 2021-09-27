#![allow(dead_code)]

use crate::LockExt;
use anyhow::Error;
use futures::{Future, Stream};
use serde::{
    de::{self, IgnoredAny, Visitor},
    ser::SerializeMap,
    Deserialize, Serialize,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader as AsyncBufReader},
    sync::{mpsc, oneshot},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRequest {
    pub method: String,
    pub params: RpcParams,
    pub id: RpcId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcResponse {
    pub result: Result<Value, RpcError>,
    pub id: RpcId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcMessage {
    Request(RpcRequest),
    Response(RpcResponse),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum RpcId {
    String(String),
    Int(i64),
    Null,
}

impl RpcId {
    pub fn is_null(&self) -> bool {
        matches!(self, RpcId::Null)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcParams {
    List(Vec<Value>),
    Map(HashMap<String, Value>),
    Null,
}

impl RpcParams {
    pub fn take_by_name<V: FromRpcParam>(&mut self, name: impl AsRef<str>) -> Result<V, RpcError> {
        let name = name.as_ref();
        self.as_map()
            .and_then(|kwargs| kwargs.remove(name))
            .ok_or_else(|| RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!("Missing required argument: {}", name),
            })
            .and_then(|param| V::from_param(param))
    }

    pub fn take_by_index<V: FromRpcParam>(&mut self, index: usize) -> Result<V, RpcError> {
        match self {
            RpcParams::List(args) if index < args.len() => V::from_param(args[index].take()),
            _ => Err(RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!("Missing required argument: {}", index),
            }),
        }
    }

    pub fn as_map(&mut self) -> Option<&mut HashMap<String, Value>> {
        match self {
            RpcParams::Map(kwargs) => Some(kwargs),
            _ => None,
        }
    }

    pub fn as_list(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            RpcParams::List(args) => Some(args),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, &RpcParams::Null)
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum RpcErrorKind {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    Other { code: i32, message: String },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct RpcError {
    kind: RpcErrorKind,
    data: String,
}

impl From<RpcErrorKind> for RpcError {
    fn from(kind: RpcErrorKind) -> Self {
        Self {
            kind,
            data: String::new(),
        }
    }
}

pub trait FromRpcParam: Sized {
    fn from_param(value: Value) -> Result<Self, RpcError>;
}

impl FromRpcParam for Value {
    fn from_param(value: Value) -> Result<Self, RpcError> {
        Ok(value)
    }
}

impl FromRpcParam for String {
    fn from_param(value: Value) -> Result<Self, RpcError> {
        match value {
            Value::String(string) => Ok(string),
            _ => Err(RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: "string argument expected".to_owned(),
            }),
        }
    }
}

impl FromRpcParam for bool {
    fn from_param(value: Value) -> Result<Self, RpcError> {
        value.as_bool().ok_or_else(|| RpcError {
            kind: RpcErrorKind::InvalidParams,
            data: "bool argument expected".to_owned(),
        })
    }
}

impl Serialize for RpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RpcId::Int(id) => serializer.serialize_i64(*id),
            RpcId::String(id) => serializer.serialize_str(id.as_str()),
            RpcId::Null => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for RpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RpcIdVisit;

        impl<'de> Visitor<'de> for RpcIdVisit {
            type Value = RpcId;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or a whole integer")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RpcId::String(v.to_owned()))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RpcId::Int(v))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RpcId::Int(v as i64))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RpcId::Null)
            }
        }

        deserializer.deserialize_any(RpcIdVisit)
    }
}

impl Serialize for RpcParams {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RpcParams::List(params) => params.serialize(serializer),
            RpcParams::Map(params) => params.serialize(serializer),
            RpcParams::Null => serializer.serialize_unit(),
        }
    }
}

impl<'de> Deserialize<'de> for RpcParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RpcParamsVisit;

        impl<'de> Visitor<'de> for RpcParamsVisit {
            type Value = RpcParams;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("dictionary or list")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut params = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(param) = seq.next_element()? {
                    params.push(param);
                }
                Ok(RpcParams::List(params))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut params = HashMap::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((key, value)) = map.next_entry()? {
                    params.insert(key, value);
                }
                Ok(RpcParams::Map(params))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RpcParams::Null)
            }
        }

        deserializer.deserialize_any(RpcParamsVisit)
    }
}

impl Serialize for RpcError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use RpcErrorKind::*;
        let (code, message) = match &self.kind {
            ParseError => (-32700, "Parse error"),
            InvalidRequest => (-32600, "Invalid request"),
            MethodNotFound => (-32601, "Method not found"),
            InvalidParams => (-32602, "Invalid params"),
            InternalError => (-32603, "Internal error"),
            Other { code, message } => (*code, message.as_ref()),
        };
        let mut attrs = serializer.serialize_map(Some(3))?;
        attrs.serialize_entry("code", &code)?;
        attrs.serialize_entry("message", message)?;
        if !self.data.is_empty() {
            attrs.serialize_entry("data", &self.data)?;
        }
        attrs.end()
    }
}

impl<'de> Deserialize<'de> for RpcError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RpcErrorVisotor;

        impl<'de> de::Visitor<'de> for RpcErrorVisotor {
            type Value = RpcError;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("dictionary with `code` and `message` keys")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: de::MapAccess<'de>,
            {
                let mut code = None;
                let mut message = None;
                let mut data = String::new();
                while let Some(key) = map.next_key()? {
                    match key {
                        "code" => {
                            code.replace(map.next_value()?);
                        }
                        "message" => {
                            message.replace(map.next_value()?);
                        }
                        "data" => {
                            data = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                let code = code.ok_or_else(|| de::Error::missing_field("code"))?;
                let message = message.ok_or_else(|| de::Error::missing_field("message"))?;
                use RpcErrorKind::*;
                let kind = match code {
                    -32700 => ParseError,
                    -32600 => InvalidRequest,
                    -32601 => MethodNotFound,
                    -32602 => InvalidParams,
                    -32603 => InternalError,
                    _ => Other { code, message },
                };

                Ok(RpcError { kind, data })
            }
        }

        deserializer.deserialize_map(RpcErrorVisotor)
    }
}

impl Serialize for RpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut attrs = serializer.serialize_map(Some(3))?;
        attrs.serialize_entry("jsonrpc", "2.0")?;
        match &self.result {
            Ok(value) => attrs.serialize_entry("result", value)?,
            Err(error) => attrs.serialize_entry("error", error)?,
        }
        if !self.id.is_null() {
            attrs.serialize_entry("id", &self.id)?;
        }
        attrs.end()
    }
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match RpcMessage::deserialize(deserializer)? {
            RpcMessage::Response(response) => Ok(response),
            _ => Err(de::Error::custom("RpcResponse expected found RpcRequest")),
        }
    }
}

impl Serialize for RpcRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut attrs = serializer.serialize_map(Some(3))?;
        attrs.serialize_entry("jsonrpc", "2.0")?;
        attrs.serialize_entry("method", &self.method)?;
        if !self.params.is_null() {
            attrs.serialize_entry("params", &self.params)?;
        }
        if !self.id.is_null() {
            attrs.serialize_entry("id", &self.id)?;
        }
        attrs.end()
    }
}

impl<'de> Deserialize<'de> for RpcRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match RpcMessage::deserialize(deserializer)? {
            RpcMessage::Request(request) => Ok(request),
            _ => Err(de::Error::custom("RpcRequest expected found RpcResponse")),
        }
    }
}

impl Serialize for RpcMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RpcMessage::Request(request) => request.serialize(serializer),
            RpcMessage::Response(response) => response.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RpcMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RpcMessageVisitor;

        impl<'de> de::Visitor<'de> for RpcMessageVisitor {
            type Value = RpcMessage;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("dictionary as per https://www.jsonrpc.org/specification")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: de::MapAccess<'de>,
            {
                // request
                let mut method: Option<String> = None;
                let mut params = RpcParams::Null;

                // response
                let mut result: Option<Value> = None;
                let mut error: Option<RpcError> = None;

                // common
                let mut id = RpcId::Null;

                while let Some(key) = map.next_key()? {
                    match key {
                        "jsonrpc" => {
                            let version: String = map.next_value()?;
                            if version != "2.0" {
                                return Err(de::Error::custom(format!(
                                    "invalid version: {}",
                                    version
                                )));
                            }
                        }
                        "method" => {
                            method.replace(map.next_value()?);
                        }
                        "params" => {
                            params = map.next_value()?;
                        }
                        "result" => {
                            result.replace(map.next_value()?);
                        }
                        "error" => {
                            error.replace(map.next_value()?);
                        }
                        "id" => {
                            id = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                if !result.is_none() || !error.is_none() {
                    let result =
                        result.ok_or_else(|| error.expect("programming error parsing rpc message"));
                    let response = RpcResponse { result, id };
                    Ok(RpcMessage::Response(response))
                } else {
                    let method =
                        method.ok_or_else(|| de::Error::missing_field("{result|error|method}"))?;
                    let request = RpcRequest { method, params, id };
                    Ok(RpcMessage::Request(request))
                }
            }
        }

        deserializer.deserialize_map(RpcMessageVisitor)
    }
}

pub fn rpc_decoder_async<I>(reader: I) -> impl Stream<Item = Result<Value, Error>>
where
    I: AsyncRead + Unpin,
{
    struct State<I> {
        reader: AsyncBufReader<I>,
        size_buf: String,
        json_buf: Vec<u8>,
    }
    let init = State {
        reader: AsyncBufReader::new(reader),
        size_buf: String::new(),
        json_buf: Vec::new(),
    };
    futures::stream::try_unfold(init, |mut state| {
        async move {
            // read size
            state.size_buf.clear();
            state.reader.read_line(&mut state.size_buf).await?;
            if state.size_buf.is_empty() {
                return Ok(None);
            }
            let size: usize = state.size_buf.trim().parse::<usize>()?;

            // read JSON
            state.json_buf.clear();
            state.json_buf.resize_with(size, || 0u8);
            state.reader.read_exact(&mut state.json_buf).await?;
            Ok(Some((Value::Null, state)))
        }
    })
}

pub type RpcHandler = Box<dyn FnMut(RpcParams) -> Box<dyn Future<Output = Result<Value, Error>>>>;

pub struct RPCPeerInner {
    handlers: HashMap<String, RpcHandler>,
    requests_last_id: u64,
    requests: HashMap<u64, oneshot::Receiver<Value>>,
    write_enqueue: mpsc::Sender<RpcMessage>,
    write_queue: mpsc::Receiver<RpcMessage>,
}

#[derive(Clone)]
pub struct RPCPeer {
    inner: Arc<Mutex<RPCPeerInner>>,
}

impl RPCPeer {
    /// Register handler for the provided name
    pub fn regesiter(&self, method: String, handler: RpcHandler) -> Option<RpcHandler> {
        self.inner
            .with(move |inner| inner.handlers.insert(method, handler))
    }

    /// Send event to the other peer
    pub fn call_no_wait(&self, _method: impl AsRef<str>, _params: RpcParams) {}

    pub async fn call(&self, _method: impl AsRef<str>, _params: RpcParams) -> Result<Value, Error> {
        todo!()
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        todo!()
    }
}

/*
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
*/

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde() -> Result<(), Error> {
        // response
        let mut response = RpcResponse {
            result: Ok("value".into()),
            id: RpcId::Int(3),
        };

        let expected = "{\"jsonrpc\":\"2.0\",\"result\":\"value\",\"id\":3}";
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.id = RpcId::Null;
        let expected = "{\"jsonrpc\":\"2.0\",\"result\":\"value\"}";
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.result = Err(RpcErrorKind::InvalidRequest.into());
        let expected =
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32600,\"message\":\"Invalid request\"}}";
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.id = RpcId::String("string_id".to_owned());
        response.result = Err(RpcError {
            kind: RpcErrorKind::MethodNotFound,
            data: "no method bla".to_owned(),
        });
        let expected = "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32601,\"message\":\"Method not found\",\"data\":\"no method bla\"},\"id\":\"string_id\"}";
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        // request
        let mut request = RpcRequest {
            method: "func".to_owned(),
            params: RpcParams::List(vec![3.141.into(), 127.into()]),
            id: RpcId::Int(1),
        };
        let expected = "{\"jsonrpc\":\"2.0\",\"method\":\"func\",\"params\":[3.141,127],\"id\":1}";
        assert_eq!(expected, serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        request.id = RpcId::Null;
        let mut params = HashMap::new();
        params.insert("key".to_owned(), "value".into());
        request.params = RpcParams::Map(params);
        let expected = "{\"jsonrpc\":\"2.0\",\"method\":\"func\",\"params\":{\"key\":\"value\"}}";
        assert_eq!(expected, serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        request.params = RpcParams::Null;
        let expected = "{\"jsonrpc\":\"2.0\",\"method\":\"func\"}";
        assert_eq!(expected, serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        Ok(())
    }
}
