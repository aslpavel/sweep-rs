use anyhow::Error;
use serde_json::value::Value;
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
