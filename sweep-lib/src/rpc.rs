//! Basic asynchronous [JSON-RPC](https://www.jsonrpc.org/specification) implementation
use crate::common::LockExt;
use futures::{
    FutureExt, Stream, TryFutureExt, TryStreamExt,
    future::{self, BoxFuture},
};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, DeserializeSeed, IgnoredAny, Visitor},
    ser::SerializeMap,
};
use serde_json::{Map, Value};
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    future::Future,
    io::Write,
    marker::PhantomData,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    sync::{mpsc, oneshot},
};
use tracing_futures::Instrument;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRequest {
    pub method: String,
    pub params: Value,
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

/// Convenience structure to access RPC handler function parameters by name or index
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcParams {
    List(Vec<Value>),
    Map(Map<String, Value>),
    Null,
}

impl RpcParams {
    /// Take attribute by the name and deserialize it
    pub fn take_by_name<V: DeserializeOwned>(
        &mut self,
        name: impl AsRef<str>,
    ) -> Result<V, RpcError> {
        self.take_by_name_seed(PhantomData::<V>, name)
    }

    /// Take attribute by the name and deserialize it with seed
    pub fn take_by_name_seed<'de, S>(
        &mut self,
        seed: S,
        name: impl AsRef<str>,
    ) -> Result<S::Value, RpcError>
    where
        S: DeserializeSeed<'de>,
    {
        let name = name.as_ref();
        self.as_map()
            .and_then(|kwargs| kwargs.remove(name))
            .ok_or_else(|| RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!("missing required keyword argument: name={}", name),
            })
            .and_then(|param| {
                seed.deserialize(param).map_err(|err| RpcError {
                    kind: RpcErrorKind::InvalidParams,
                    data: format!("[arg_name={}][deserialize] {}", name, err),
                })
            })
    }

    /// Take attribute by the index and deserialize it
    pub fn take_by_index<V: DeserializeOwned>(&mut self, index: usize) -> Result<V, RpcError> {
        self.take_by_index_seed(PhantomData::<V>, index)
    }

    /// Take attribute by the index and deserialize it with seed
    pub fn take_by_index_seed<'de, S>(
        &mut self,
        seed: S,
        index: usize,
    ) -> Result<S::Value, RpcError>
    where
        S: DeserializeSeed<'de>,
    {
        match self {
            RpcParams::List(args) if index < args.len() => seed
                .deserialize(args[index].take())
                .map_err(|err| RpcError {
                    kind: RpcErrorKind::InvalidParams,
                    data: format!("[arg_index={}][deserialize] {}", index, err),
                }),
            _ => Err(RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!("missing required positional argument: index={}", index),
            }),
        }
    }

    /// Take attribute by the name or the index and deserialize it
    pub fn take<V: DeserializeOwned>(
        &mut self,
        index: usize,
        name: impl AsRef<str>,
    ) -> Result<V, RpcError> {
        self.take_seed(PhantomData::<V>, index, name)
    }

    /// Take attribute by the name or the index and deserialize it with seed
    pub fn take_seed<'de, S>(
        &mut self,
        seed: S,
        index: usize,
        name: impl AsRef<str>,
    ) -> Result<S::Value, RpcError>
    where
        S: DeserializeSeed<'de>,
    {
        match self {
            RpcParams::List(_) => self.take_by_index_seed(seed, index),
            RpcParams::Map(_) => self.take_by_name_seed(seed, name.as_ref()),
            RpcParams::Null => Err(RpcError {
                kind: RpcErrorKind::InvalidParams,
                data: format!(
                    "missing required argument: index={} name={}",
                    index,
                    name.as_ref()
                ),
            }),
        }
    }

    /// Take optional attribute by the name or the index and deserialize it
    pub fn take_opt<V: DeserializeOwned>(
        &mut self,
        index: usize,
        name: impl AsRef<str>,
    ) -> Result<Option<V>, RpcError> {
        self.take_opt_seed(PhantomData::<V>, index, name)
    }

    /// Take optional attribute by the name or the index and deserialize it with seed
    pub fn take_opt_seed<'de, S>(
        &mut self,
        seed: S,
        index: usize,
        name: impl AsRef<str>,
    ) -> Result<Option<S::Value>, RpcError>
    where
        S: DeserializeSeed<'de>,
    {
        let result = match self {
            RpcParams::List(attrs) if !matches!(attrs.get(index), Some(Value::Null) | None) => {
                Some(self.take_by_index_seed(seed, index)?)
            }
            RpcParams::Map(attrs)
                if !matches!(attrs.get(name.as_ref()), Some(Value::Null) | None) =>
            {
                Some(self.take_by_name_seed(seed, name)?)
            }
            _ => None,
        };
        Ok(result)
    }

    /// View parameters as a keywords map
    pub fn as_map(&mut self) -> Option<&mut Map<String, Value>> {
        match self {
            RpcParams::Map(kwargs) => Some(kwargs),
            _ => None,
        }
    }

    /// View parameters as an argument list
    pub fn as_list(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            RpcParams::List(args) => Some(args),
            _ => None,
        }
    }

    /// Check if parameters are empty
    pub fn is_null(&self) -> bool {
        matches!(self, &RpcParams::Null)
    }

    /// Convert parameters into JSON value
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
    SurfNTerm,
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
            -32000 => ServeError,
            1000 => PeerDisconnected,
            1001 => SerdeError,
            1002 => IOError,
            1003 => SurfNTerm,
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
            ServeError => -32000,
            PeerDisconnected => 1000,
            SerdeError => 1001,
            IOError => 1002,
            SurfNTerm => 1003,
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
            IOError => "Imput/Output error",
            ServeError => "Serve error",
            SurfNTerm => "surf-n-term error",
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

impl From<anyhow::Error> for RpcError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            kind: RpcErrorKind::InternalError,
            data: error.to_string(),
        }
    }
}

impl From<surf_n_term::Error> for RpcError {
    fn from(error: surf_n_term::Error) -> Self {
        Self {
            kind: RpcErrorKind::SurfNTerm,
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

        impl Visitor<'_> for RpcIdVisit {
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
                let mut params = Value::Null;

                // response
                let mut result: Option<Value> = None;
                let mut error: Option<RpcError> = None;

                // common
                let mut id = RpcId::Null;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
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

                let result = match (result, error, method) {
                    (Some(result), None, None) => RpcResponse {
                        result: Ok(result),
                        id,
                    }
                    .into(),
                    (None, Some(error), None) => RpcResponse {
                        result: Err(error),
                        id,
                    }
                    .into(),
                    (None, None, Some(method)) => RpcRequest { method, params, id }.into(),
                    _ => return Err(de::Error::missing_field("{result|error|method}")),
                };
                Ok(result)
            }
        }

        deserializer.deserialize_map(RpcMessageVisitor)
    }
}

pub type RpcHandler =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, RpcError>> + Sync + Send>;

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

impl Default for RpcPeer {
    fn default() -> Self {
        Self::new()
    }
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

    /// Register callback for the provided method name
    pub fn register<H, HP, HF, HR>(
        &self,
        method: impl Into<String>,
        callback: H,
    ) -> Option<RpcHandler>
    where
        H: Fn(HP) -> HF + Send + Sync + 'static,
        HF: Future<Output = Result<HR, RpcError>> + Send + 'static,
        HR: Into<Value> + Send + 'static,
        HP: DeserializeOwned,
    {
        let handler = Arc::new(move |params| {
            let params = match serde_json::from_value(params) {
                Ok(params) => params,
                Err(error) => return future::err(RpcError::from(error)).boxed(),
            };
            callback(params).map_ok(Into::into).boxed()
        });
        self.regesiter_handler(method, handler)
    }

    /// Register handler for the provided method name
    pub fn regesiter_handler(
        &self,
        method: impl Into<String>,
        handler: RpcHandler,
    ) -> Option<RpcHandler> {
        self.inner
            .with_mut(move |inner| inner.handlers.insert(method.into(), handler))
    }

    /// Send event to the other peer
    ///
    /// Serializes params to `serde_json::Value` and issues `call_with_value`
    pub fn notify<V: Serialize>(
        &self,
        method: impl Into<String>,
        params: V,
    ) -> Result<(), RpcError> {
        let params = serde_json::to_value(params)?;
        self.notify_with_value(method, params)
    }

    /// Send event to the other peer
    pub fn notify_with_value(
        &self,
        method: impl Into<String>,
        params: impl Into<Value>,
    ) -> Result<(), RpcError> {
        let method = method.into();
        let params = params.into();
        let id = RpcId::Null;
        tracing::debug!(
            method = %method,
            params = %params,
            "[RpcPeer.notify_with_value]"
        );
        self.submit_message(RpcRequest { method, params, id })
    }

    /// Issue rpc call and wait for response
    ///
    /// Serializes params to `serde_json::Value` and issues `call_with_value`
    pub async fn call<V: Serialize>(
        &self,
        method: impl Into<String>,
        params: V,
    ) -> Result<Value, RpcError> {
        let params = serde_json::to_value(params)?;
        self.call_with_value(method, params).await
    }

    /// Issue rpc call and wait for response
    pub async fn call_with_value(
        &self,
        method: impl Into<String>,
        params: impl Into<Value>,
    ) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        let id = self.inner.with_mut(|inner| {
            let id = RpcId::Int(inner.requests_next_id);
            inner.requests_next_id += 1;
            inner.requests.insert(id.clone(), tx);
            id
        });
        let method = method.into();
        let params = params.into();
        let span = tracing::debug_span!(
            "[RpcPeer.call_with_value]",
            method = %method,
            params = %params,
            id = ?id,
        );
        self.submit_message(RpcRequest {
            method: method.clone(),
            params,
            id: id.clone(),
        })?;
        let result = rx.instrument(span).await.map_err(|_| RpcError {
            kind: RpcErrorKind::PeerDisconnected,
            data: "one shot channeld was destroyed".to_owned(),
        })?;
        tracing::debug!(
            method = %method,
            result = ?result,
            id = ?id,
            "[RpcPeer.call_with_value]",
        );
        result
    }

    /// Start serving rpc requests
    pub fn serve<'a, R, W>(
        &self,
        read: R,
        write: W,
    ) -> impl Future<Output = Result<(), RpcError>> + 'a
    where
        R: AsyncRead + 'a,
        W: AsyncWrite + 'a,
    {
        let peer = self.clone();

        async move {
            tokio::pin!(read);
            let write_receiver = peer
                .inner
                .with_mut(|inner| inner.write_receiver.take())
                .ok_or_else(|| RpcError::from(RpcErrorKind::ServeError))?;

            let writer = rpc_writer(write, write_receiver);
            let reader = rpc_reader(read).try_for_each(|message| peer.handle_message(message));
            tokio::select! {
                result = reader => result,
                result = writer => result,
            }
        }
    }

    /// Submit message to be send to the other peer
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

    /// Handle incoming rpc message
    async fn handle_message(&self, message: RpcMessage) -> Result<(), RpcError> {
        match message {
            RpcMessage::Response(response) => {
                if response.id == RpcId::Null {
                    // propagate errors with no id
                    return response.result.map(|_| ());
                }
                if let Some(future) = self
                    .inner
                    .with_mut(|inner| inner.requests.remove(&response.id))
                {
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
                        let span = tracing::debug_span!(
                            "[RpcPeer.handle_message]",
                            method = %request.method,
                            params = %request.params,
                            id = ?request.id,
                        );
                        let result = handler(request.params).instrument(span).await;
                        tracing::debug!(
                            method = %request.method,
                            result = ?result,
                            id = ?request.id,
                            "[RpcPeer.handle_message]"
                        );
                        if request.id != RpcId::Null {
                            let method = request.method;
                            let response = RpcResponse {
                                result: result.map_err(|error| RpcError {
                                    kind: error.kind,
                                    data: format!("[{}] {}", method, error.data),
                                }),
                                id: request.id,
                            };
                            let _ = peer.submit_message(response);
                        }
                    });
                } else if request.id != RpcId::Null {
                    tracing::warn!(
                        method = %request.method,
                        params = %request.params,
                        id = ?request.id,
                        "[RpcPeer.handle_message]"
                    );
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
    let mut message_data = Vec::new();

    let write = BufWriter::new(write);
    tokio::pin!(write);

    while let Some(message) = messages.recv().await {
        // clear buffers
        message_data.clear();
        // serialize
        serde_json::to_writer(&mut message_data, &message)?;
        writeln!(&mut message_data)?;
        // write
        write.write_all(message_data.as_ref()).await?;
        write.flush().await?;
    }
    Ok(())
}

/// Read stream of RpcMessages from AsyncRead
fn rpc_reader<'a, R>(read: R) -> impl Stream<Item = Result<RpcMessage, RpcError>> + 'a
where
    R: AsyncRead + Unpin + 'a,
{
    struct State<R> {
        reader: BufReader<R>,
        message_buf: Vec<u8>,
    }
    futures::stream::try_unfold(
        State {
            reader: BufReader::new(read),
            message_buf: Vec::new(),
        },
        |mut state| {
            async move {
                // read message
                state.message_buf.clear();
                state
                    .reader
                    .read_until(b'\n', &mut state.message_buf)
                    .await
                    .map_err(|error| RpcError {
                        kind: RpcErrorKind::ParseError,
                        data: format!("failed to read message: {}", error),
                    })?;

                // eof
                if state.message_buf.is_empty() {
                    return Ok(None);
                }

                // parse message
                let message =
                    serde_json::from_slice(state.message_buf.as_ref()).map_err(|error| {
                        RpcError {
                            kind: RpcErrorKind::ParseError,
                            data: format!("failed parse message: {}", error),
                        }
                    })?;

                Ok(Some((message, state)))
            }
        },
    )
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

        let expected = "{\"result\":\"value\",\"id\":3}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(response, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.id = RpcId::Null;
        let expected = "{\"result\":\"value\"}";
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.result = Err(RpcErrorKind::InvalidRequest.into());
        let expected = "{\"error\":{\"code\":-32600,\"message\":\"Invalid request\"}}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(response, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        response.id = RpcId::String("string_id".to_owned());
        response.result = Err(RpcError {
            kind: RpcErrorKind::MethodNotFound,
            data: "no method bla".to_owned(),
        });
        let expected = "{\"error\":{\"code\":-32601,\"message\":\"Method not found\",\"data\":\"no method bla\"},\"id\":\"string_id\"}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(response, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&response)?);
        assert_eq!(response, serde_json::from_str::<RpcResponse>(expected)?);

        // request
        let mut request = RpcRequest {
            method: "func".to_owned(),
            params: json!([3.1, 127]),
            id: RpcId::Int(1),
        };
        let expected = "{\"method\":\"func\",\"params\":[3.1,127],\"id\":1}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(request, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        request.id = RpcId::Null;
        request.params = json!({"key": "value"});
        let expected = "{\"method\":\"func\",\"params\":{\"key\":\"value\"}} ";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(request, serde_json::from_value(value)?);
        assert_eq!(
            expected[..expected.len() - 1],
            serde_json::to_string(&request)?
        );
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        request.params = Value::Null;
        let expected = " {\"method\":\"func\"}";
        let value: Value = serde_json::from_str(expected)?;
        assert_eq!(request, serde_json::from_value(value)?);
        assert_eq!(expected[1..], serde_json::to_string(&request)?);
        assert_eq!(request, serde_json::from_str::<RpcRequest>(expected)?);

        Ok(())
    }

    #[test]
    fn test_rpc_params() -> Result<(), RpcError> {
        // list params
        let mut list_params = RpcParams::try_from(json!([1, null, "3", 4, 5]))?;

        // success
        let first: usize = list_params.take_by_index(0)?;
        assert_eq!(first, 1);
        let second: Option<i32> = list_params.take_opt(1, "second")?;
        assert_eq!(second, None);
        let third: String = list_params.take(2, "third")?;
        assert_eq!(third, "3".to_owned());
        let fourth: Option<usize> = list_params.take_opt(3, "fourth")?;
        assert_eq!(fourth, Some(4));
        let fourth: Option<usize> = list_params.take_opt(3, "fourth")?;
        assert_eq!(fourth, None); // already taken

        // errors
        let fifth_error: Result<String, _> = list_params.take_by_index(4);
        assert!(fifth_error.is_err()); // type error
        let six_error: Result<String, _> = list_params.take_by_index(5);
        assert!(six_error.is_err()); // out of range
        let seven_error: Result<String, _> = list_params.take_by_name("bla");
        assert!(seven_error.is_err()); // named parameter on a list

        // dict params
        let mut dict_params = RpcParams::try_from(json!({
            "first": 1,
            "second": Value::Null,
            "third": "3",
            "fourth": 4,
            "fifth": 5,
        }))?;

        // success
        let first: usize = dict_params.take_by_name("first")?;
        assert_eq!(first, 1);
        let second: Option<i32> = dict_params.take_opt(1, "second")?;
        assert_eq!(second, None);
        let third: String = dict_params.take(2, "third")?;
        assert_eq!(third, "3".to_owned());
        let fourth: Option<usize> = dict_params.take_opt(3, "fourth")?;
        assert_eq!(fourth, Some(4));
        let fourth: Option<usize> = dict_params.take_opt(3, "fourth")?;
        assert_eq!(fourth, None); // already taken

        // errors
        let fifth_error: Result<String, _> = dict_params.take_by_name("fifth");
        assert!(fifth_error.is_err()); // type error
        let six_error: Result<String, _> = dict_params.take_by_name("sixth");
        assert!(six_error.is_err()); // missing
        let seven_error: Result<String, _> = dict_params.take_by_index(10);
        assert!(seven_error.is_err()); // index parameter on a dict

        let mut null_params = RpcParams::try_from(Value::Null)?;
        let optional: Option<usize> = null_params.take_opt(3, "three")?;
        assert_eq!(optional, None);
        let index_error: Result<usize, _> = null_params.take_by_index(1);
        assert!(index_error.is_err());
        let name_error: Result<usize, _> = null_params.take_by_name("bla");
        assert!(name_error.is_err());

        Ok(())
    }

    #[tokio::test]
    async fn test_rpc_peer_duplex() -> Result<(), RpcError> {
        test_rpc_peer(tokio::io::duplex(16)).await
    }

    #[tokio::test]
    async fn test_rpc_peer_socket() -> Result<(), RpcError> {
        test_rpc_peer(tokio::net::UnixStream::pair()?).await
    }

    async fn test_rpc_peer<C>((a_channel, b_channel): (C, C)) -> Result<(), RpcError>
    where
        C: AsyncRead + AsyncWrite + Send + 'static,
    {
        let a = RpcPeer::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        a.regesiter_handler("name", Arc::new(|_params| async { Ok("a".into()) }.boxed()));
        a.register("add", |mut params: RpcParams| async move {
            let a: i64 = params.take_by_name("a")?;
            let b: i64 = params.take_by_name("b")?;
            Ok(a + b)
        });
        a.register("send", move |params: Value| {
            let tx = tx.clone();
            async move {
                tx.send(params.clone()).unwrap();
                Ok(params)
            }
        });
        #[derive(Serialize, Deserialize)]
        struct ConcatParams {
            a: String,
            b: String,
        }
        a.register("concat", |mut params: ConcatParams| async move {
            params.a.push_str(&params.b);
            Ok(params.a)
        });
        a.register("index_or_name", |mut params: RpcParams| async move {
            let value: String = params.take(0, "value")?;
            Ok(value)
        });

        let b = RpcPeer::new();
        b.regesiter_handler("name", Arc::new(|_params| async { Ok("b".into()) }.boxed()));

        // serve
        let (read, write) = tokio::io::split(a_channel);
        tokio::spawn(a.serve(read, write));
        let (read, write) = tokio::io::split(b_channel);
        tokio::spawn(b.serve(read, write));

        // basic
        let hello_result = b.call("name", Value::Null).await?;
        assert_eq!(json!("a"), hello_result);
        let hello_result = a.call("name", RpcParams::Null).await?;
        assert_eq!(json!("b"), hello_result);

        // add
        let add_result = b.call("add", json!({ "a": 1, "b": 2 })).await?;
        assert_eq!(json!(3), add_result);
        let add_error = b.call("add", json!({ "a": 1 })).await.unwrap_err();
        assert_eq!(add_error.kind, RpcErrorKind::InvalidParams);

        // invalid method
        let method_error = b.call("blabla", RpcParams::Null).await.unwrap_err();
        assert_eq!(method_error.kind, RpcErrorKind::MethodNotFound);

        // send
        let msg = json!({"val": 127});
        let send_result = b.call("send", msg.clone()).await?;
        assert_eq!(msg, send_result);
        assert_eq!(msg, rx.recv().await.unwrap());

        // send notify
        let msg = json!({"val": 11});
        b.notify("send", msg.clone())?;
        assert_eq!(msg, rx.recv().await.unwrap());

        // concat
        let concat_result = b
            .call(
                "concat",
                ConcatParams {
                    a: "one ".to_string(),
                    b: "two".to_string(),
                },
            )
            .await?;
        assert_eq!(json!("one two"), concat_result);
        let concat_result = b
            .call("concat", json!({ "a": "three ", "b": "four" }))
            .await?;
        assert_eq!(json!("three four"), concat_result);

        // index or name
        let value = b.call("index_or_name", json!(["index"])).await?;
        assert_eq!(json!("index"), value);
        let value = b.call("index_or_name", json!({"value": "name"})).await?;
        assert_eq!(json!("name"), value);
        let error = b.call("index_or_name", Value::Null).await.unwrap_err();
        assert_eq!(error.kind, RpcErrorKind::InvalidParams);

        Ok(())
    }
}
