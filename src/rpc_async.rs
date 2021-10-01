//! Basic asynchronus [JSON-RPC](https://www.jsonrpc.org/specification) implementation

use crate::LockExt;
use futures::{future::BoxFuture, FutureExt, Stream, TryStreamExt};
use serde::{
    de::{self, DeserializeOwned, IgnoredAny, Visitor},
    ser::SerializeMap,
    Deserialize, Serialize,
};
use serde_json::{Map, Value};
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::TryFrom,
    fmt,
    io::Write,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter,
    },
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

impl From<RpcRequest> for RpcMessage {
    fn from(request: RpcRequest) -> Self {
        RpcMessage::Request(request)
    }
}

impl From<RpcResponse> for RpcMessage {
    fn from(response: RpcResponse) -> Self {
        RpcMessage::Response(response)
    }
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
    Map(Map<String, Value>),
    Null,
}

impl RpcParams {
    pub fn take_by_name<V: DeserializeOwned>(
        &mut self,
        name: impl AsRef<str>,
    ) -> Result<V, RpcError> {
        let name = name.as_ref();
        self.as_map()
            .and_then(|kwargs| kwargs.remove(name))
            .ok_or_else(|| RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!("missing required argument: {}", name),
            })
            .and_then(|param| {
                serde_json::from_value(param).map_err(|err| RpcError {
                    kind: RpcErrorKind::InvalidParams,
                    data: err.to_string(),
                })
            })
    }

    pub fn take_by_index<V: DeserializeOwned>(&mut self, index: usize) -> Result<V, RpcError> {
        match self {
            RpcParams::List(args) if index < args.len() => {
                serde_json::from_value(args[index].take()).map_err(|err| RpcError {
                    kind: RpcErrorKind::InvalidParams,
                    data: err.to_string(),
                })
            }
            _ => Err(RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!("missing required argument: {}", index),
            }),
        }
    }

    pub fn as_map(&mut self) -> Option<&mut Map<String, Value>> {
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

    pub fn into_value(self) -> Value {
        match self {
            Self::Map(map) => map.into(),
            Self::List(list) => list.into(),
            Self::Null => Value::Null,
        }
    }
}

impl TryFrom<Value> for RpcParams {
    type Error = RpcError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value).map_err(|error| RpcError {
            kind: RpcErrorKind::InvalidParams,
            data: format!("failed to conver value into params: {}", error),
        })
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum RpcErrorKind {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    PeerDisconnected,
    SerdeError,
    IOError,
    ServeError,
    Other { code: i32, message: String },
}

impl RpcErrorKind {
    fn new(code: i32, message: String) -> Self {
        use RpcErrorKind::*;
        match code {
            -32700 => ParseError,
            -32600 => InvalidRequest,
            -32601 => MethodNotFound,
            -32602 => InvalidParams,
            -32603 => InternalError,
            1000 => PeerDisconnected,
            1001 => SerdeError,
            1002 => IOError,
            1003 => ServeError,
            _ => Other { code, message },
        }
    }

    fn code(&self) -> i32 {
        use RpcErrorKind::*;
        match self {
            ParseError => -32700,
            InvalidRequest => -32600,
            MethodNotFound => -32601,
            InvalidParams => -32602,
            InternalError => -32603,
            PeerDisconnected => 1000,
            SerdeError => 1001,
            IOError => 1002,
            ServeError => 1003,
            Other { code, .. } => *code,
        }
    }

    fn message(&self) -> &str {
        use RpcErrorKind::*;
        match self {
            ParseError => "Parse error",
            InvalidRequest => "Invalid request",
            MethodNotFound => "Method not found",
            InvalidParams => "Invalid params",
            InternalError => "Internal error",
            PeerDisconnected => "Peer disconnected",
            SerdeError => "Faield to (de)serialize",
            IOError => "Imput/Output Error",
            ServeError => "RpcPeer::serve called second time",
            Other { message, .. } => message.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct RpcError {
    kind: RpcErrorKind,
    data: String,
}

impl RpcError {
    fn new(code: i32, message: String, data: String) -> Self {
        Self {
            kind: RpcErrorKind::new(code, message),
            data,
        }
    }

    fn kind(&self) -> &RpcErrorKind {
        &self.kind
    }

    fn data(&self) -> &str {
        self.data.as_str()
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.data.is_empty() {
            write!(f, "{}", self.kind().message())
        } else {
            write!(f, "{}: {}", self.kind().message(), self.data())
        }
    }
}

impl From<RpcErrorKind> for RpcError {
    fn from(kind: RpcErrorKind) -> Self {
        Self {
            kind,
            data: String::new(),
        }
    }
}

impl From<serde_json::Error> for RpcError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            kind: RpcErrorKind::SerdeError,
            data: error.to_string(),
        }
    }
}

impl From<std::io::Error> for RpcError {
    fn from(error: std::io::Error) -> Self {
        Self {
            kind: RpcErrorKind::IOError,
            data: error.to_string(),
        }
    }
}

impl std::error::Error for RpcError {}

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
                let mut params = Map::with_capacity(map.size_hint().unwrap_or(0));
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
        let mut attrs = serializer.serialize_map(Some(3))?;
        attrs.serialize_entry("code", &self.kind().code())?;
        attrs.serialize_entry("message", self.kind().message())?;
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
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
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
                Ok(RpcError::new(code, message, data))
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

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
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

pub type RpcHandler =
    Arc<dyn Fn(RpcParams) -> BoxFuture<'static, Result<Value, RpcError>> + Sync + Send>;

pub struct RpcPeerInner {
    handlers: HashMap<String, RpcHandler>,
    requests_next_id: i64,
    requests: HashMap<RpcId, oneshot::Sender<Result<Value, RpcError>>>,
    write_sender: mpsc::UnboundedSender<RpcMessage>,
    write_receiver: Option<mpsc::UnboundedReceiver<RpcMessage>>,
}

#[derive(Clone)]
pub struct RpcPeer {
    inner: Arc<Mutex<RpcPeerInner>>,
}

impl RpcPeer {
    pub fn new() -> Self {
        let (write_sender, write_receiver) = mpsc::unbounded_channel();
        let inner = Arc::new(Mutex::new(RpcPeerInner {
            handlers: HashMap::new(),
            requests_next_id: 0,
            requests: HashMap::new(),
            write_sender,
            write_receiver: Some(write_receiver),
        }));
        Self { inner }
    }

    /// Register handler for the provided name
    pub fn regesiter(&self, method: impl Into<String>, handler: RpcHandler) -> Option<RpcHandler> {
        self.inner
            .with(move |inner| inner.handlers.insert(method.into(), handler))
    }

    /// Send event t to the other peer
    pub fn notify(
        &self,
        method: impl Into<String>,
        params: impl Into<RpcParams>,
    ) -> Result<(), RpcError> {
        self.submit_message(RpcRequest {
            method: method.into(),
            params: params.into(),
            id: RpcId::Null,
        })
    }

    /// Issue rpc call and wait for response
    pub async fn call(
        &self,
        method: impl Into<String>,
        params: impl Into<RpcParams>,
    ) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        let id = self.inner.with(|inner| {
            let id = RpcId::Int(inner.requests_next_id);
            inner.requests_next_id += 1;
            inner.requests.insert(id.clone(), tx);
            id
        });
        self.submit_message(RpcRequest {
            method: method.into(),
            params: params.into(),
            id,
        })?;
        rx.await.map_err(|_| RpcError {
            kind: RpcErrorKind::PeerDisconnected,
            data: "one shot channeld was destroyed".to_owned(),
        })?
    }

    /// Start serving rpc requests
    pub fn serve<R, W>(&self, read: R, write: W) -> BoxFuture<'static, Result<(), RpcError>>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let peer = self.clone();
        async move {
            let write_receiver = peer
                .inner
                .with(|inner| inner.write_receiver.take())
                .ok_or_else(|| RpcError::from(RpcErrorKind::ServeError))?;
            let writer = rpc_writer(write, write_receiver);
            let reader = rpc_reader(read).try_for_each(|message| peer.handle_message(message));
            tokio::pin!(reader, writer);
            tokio::select! {
                result = reader => result,
                result = writer => result,
            }
        }
        .boxed()
    }

    /// Sumbit message to be send to the other peer
    fn submit_message(&self, message: impl Into<RpcMessage>) -> Result<(), RpcError> {
        // not that we use unbound queue
        let message = message.into();
        self.inner
            .with(move |inner| inner.write_sender.send(message))
            .map_err(|error| RpcError {
                kind: RpcErrorKind::PeerDisconnected,
                data: format!("failed to send message: {}", error),
            })
    }

    /// Handle incomming rpc message
    async fn handle_message(&self, message: RpcMessage) -> Result<(), RpcError> {
        match message {
            RpcMessage::Response(response) => {
                if response.id == RpcId::Null {
                    // propagate errors with no id
                    return response.result.map(|_| ());
                }
                if let Some(future) = self.inner.with(|inner| inner.requests.remove(&response.id)) {
                    let _ = future.send(response.result);
                }
            }
            RpcMessage::Request(request) => {
                let handler = self
                    .inner
                    .with(|inner| inner.handlers.get(&request.method).cloned());
                if let Some(handler) = handler {
                    let peer = self.clone();
                    tokio::spawn(async move {
                        let result = handler(request.params).await;
                        if request.id != RpcId::Null {
                            let response = RpcResponse {
                                result,
                                id: request.id,
                            };
                            let _ = peer.submit_message(response);
                        }
                    });
                } else {
                    if request.id != RpcId::Null {
                        let response = RpcResponse {
                            result: Err(RpcError {
                                kind: RpcErrorKind::MethodNotFound,
                                data: format!("no shuch method: {}", request.method),
                            }),
                            id: request.id,
                        };
                        self.submit_message(response)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Write stream of messages from message receiver
async fn rpc_writer<W>(
    write: W,
    mut messages: mpsc::UnboundedReceiver<RpcMessage>,
) -> Result<(), RpcError>
where
    W: AsyncWrite,
{
    let mut message_len = Vec::new();
    let mut message_data = Vec::new();

    let write = BufWriter::new(write);
    tokio::pin!(write);

    while let Some(message) = messages.recv().await {
        // clear buffers
        message_len.clear();
        message_data.clear();
        // serialize
        serde_json::to_writer(&mut message_data, &message)?;
        writeln!(&mut message_len, "{}", message_data.len())?;
        // write
        write.write_all(message_len.as_ref()).await?;
        write.write_all(message_data.as_ref()).await?;
        write.flush().await?;
    }
    Ok(())
}

/// Read stream of RpcMessages from AsyncRead
fn rpc_reader<R>(read: R) -> impl Stream<Item = Result<RpcMessage, RpcError>>
where
    R: AsyncRead + Unpin,
{
    struct State<I> {
        reader: BufReader<I>,
        size_buf: String,
        message_buf: Vec<u8>,
    }
    let init = State {
        reader: BufReader::new(read),
        size_buf: String::new(),
        message_buf: Vec::new(),
    };
    futures::stream::try_unfold(init, |mut state| {
        async move {
            // read size
            state.size_buf.clear();
            state
                .reader
                .read_line(&mut state.size_buf)
                .await
                .map_err(|error| RpcError {
                    kind: RpcErrorKind::ParseError,
                    data: format!("failed to read message size: {}", error),
                })?;
            if state.size_buf.is_empty() {
                return Ok(None);
            }
            let size: usize = state
                .size_buf
                .trim()
                .parse::<usize>()
                .map_err(|error| RpcError {
                    kind: RpcErrorKind::ParseError,
                    data: format!("failed parse message size: {}", error),
                })?;

            // read message
            state.message_buf.clear();
            state.message_buf.resize_with(size, || 0u8);
            state
                .reader
                .read_exact(&mut state.message_buf)
                .await
                .map_err(|error| RpcError {
                    kind: RpcErrorKind::ParseError,
                    data: format!("failed to read message: {}", error),
                })?;

            // parse message
            let message =
                serde_json::from_slice(state.message_buf.as_ref()).map_err(|error| RpcError {
                    kind: RpcErrorKind::ParseError,
                    data: format!("failed parse message: {}", error),
                })?;

            Ok(Some((message, state)))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;
    use serde_json::json;

    #[test]
    fn test_rpc_serde() -> Result<(), Error> {
        // response
        let mut response = RpcResponse {
            result: Ok("value".into()),
            id: RpcId::Int(3),
        };

        let expected = "{\"jsonrpc\":\"2.0\",\"result\":\"value\",\"id\":3}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(response, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.id = RpcId::Null;
        let expected = "{\"jsonrpc\":\"2.0\",\"result\":\"value\"}";
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.result = Err(RpcErrorKind::InvalidRequest.into());
        let expected =
            "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32600,\"message\":\"Invalid request\"}}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(response, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.id = RpcId::String("string_id".to_owned());
        response.result = Err(RpcError {
            kind: RpcErrorKind::MethodNotFound,
            data: "no method bla".to_owned(),
        });
        let expected = "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32601,\"message\":\"Method not found\",\"data\":\"no method bla\"},\"id\":\"string_id\"}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(response, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        // request
        let mut request = RpcRequest {
            method: "func".to_owned(),
            params: RpcParams::List(vec![3.141.into(), 127.into()]),
            id: RpcId::Int(1),
        };
        let expected = "{\"jsonrpc\":\"2.0\",\"method\":\"func\",\"params\":[3.141,127],\"id\":1}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(request, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        request.id = RpcId::Null;
        let mut params = Map::new();
        params.insert("key".to_owned(), "value".into());
        request.params = RpcParams::Map(params);
        let expected = "{\"jsonrpc\":\"2.0\",\"method\":\"func\",\"params\":{\"key\":\"value\"}} ";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(request, serde_json::from_value(value)?);
        assert_eq!(
            expected[..expected.len() - 1],
            serde_json::to_string(&request)?
        );
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        request.params = RpcParams::Null;
        let expected = " {\"jsonrpc\":\"2.0\",\"method\":\"func\"}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(request, serde_json::from_value(value)?);
        assert_eq!(expected[1..], serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        Ok(())
    }

    #[tokio::test]
    async fn test_prc_peer() -> Result<(), RpcError> {
        let a = RpcPeer::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        a.regesiter(
            "hello",
            Arc::new(|_params| async { Ok("a".into()) }.boxed()),
        );
        a.regesiter(
            "add",
            Arc::new(|mut params| {
                async move {
                    let a: i64 = params.take_by_name("a")?;
                    let b: i64 = params.take_by_name("b")?;
                    Ok((a + b).into())
                }
                .boxed()
            }),
        );
        a.regesiter(
            "send",
            Arc::new({
                move |mut params| {
                    let tx = tx.clone();
                    async move {
                        let val: Value = params.take_by_name("val")?;
                        tx.send(val.clone()).unwrap();
                        Ok(val)
                    }.boxed()
                }
            })
        );

        let b = RpcPeer::new();
        b.regesiter(
            "hello",
            Arc::new(|_params| async { Ok("b".into()) }.boxed()),
        );

        // connect and serve
        let (a_stream, b_stream) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(a_stream);
        tokio::spawn(a.serve(read, write));
        let (read, write) = tokio::io::split(b_stream);
        tokio::spawn(b.serve(read, write));

        // basic
        let hello_result = b.call("hello", RpcParams::Null).await?;
        assert_eq!(json!("a"), hello_result);
        let hello_result = a.call("hello", RpcParams::Null).await?;
        assert_eq!(json!("b"), hello_result);

        // add
        let add_result = b
            .call("add", RpcParams::try_from(json!({ "a": 1, "b": 2 }))?)
            .await?;
        assert_eq!(json!(3), add_result);
        let add_error = b
            .call("add", RpcParams::try_from(json!({ "a": 1 }))?)
            .await
            .unwrap_err();
        assert_eq!(add_error.kind, RpcErrorKind::InvalidParams);

        // invalid method
        let method_error = b.call("blabla", RpcParams::Null).await.unwrap_err();
        assert_eq!(method_error.kind, RpcErrorKind::MethodNotFound);

        // send
        let send_result = b.call("send", RpcParams::try_from(json!({"val": 127}))?).await?;
        assert_eq!(json!(127), send_result);
        assert_eq!(json!(127), rx.recv().await.unwrap());

        // send notify
        b.notify("send", RpcParams::try_from(json!({"val": 11}))?)?;
        assert_eq!(json!(11), rx.recv().await.unwrap());


        Ok(())
    }
}
