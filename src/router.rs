//! Routes agent messages to Coderpack and verdicts back.
//!
//! The one invariant that matters: **nothing here blocks**.  The agent stops
//! the game thread while it waits for a verdict, so if this side blocked while
//! waiting for Coderpack, a mod issuing a command from inside an event handler would
//! deadlock the game.  So `on_message` only writes; verdicts are posted by the
//! JVM reader thread, and a watchdog answers on Coderpack's behalf when it is late.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frida::{Message, ScriptHandler};
use serde_json::Value;

use crate::codec::Frame;

/// What the poster thread (which owns the Script) is asked to do.
pub enum ToAgent {
    Verdict { seq: u64, cancel: bool, set: Vec<(String, String)> },
    Command { seq: u64, name: String, fields: Vec<(String, String)> },
    Asking(bool),
}

#[derive(Clone, Default)]
pub struct Pending(Arc<Mutex<HashMap<u64, Instant>>>);

impl Pending {
    pub fn open(&self, seq: u64) {
        self.0.lock().unwrap().insert(seq, Instant::now());
    }

    pub fn close(&self, seq: u64) {
        self.0.lock().unwrap().remove(&seq);
    }

    /// Sequence numbers Coderpack has not answered within `deadline`.
    pub fn overdue(&self, deadline: Duration) -> Vec<u64> {
        let now = Instant::now();
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, since)| now.duration_since(**since) > deadline)
            .map(|(seq, _)| *seq)
            .collect()
    }
}

pub struct Router {
    pub to_sal: Sender<String>,
    pub pending: Pending,
    /// Events dropped because Coderpack could not keep up, reported once it does.
    pub dropped: Arc<Mutex<u64>>,
}

impl ScriptHandler for Router {
    fn on_message(&mut self, message: Message, _data: Option<Vec<u8>>) {
        match message {
            Message::Send(send) => self.dispatch(send.payload),
            Message::Log(log) => eprintln!("[agent] {}", log.payload),
            Message::Error(err) => {
                eprintln!("[agent] Error: {} at {}:{}", err.description,
                          err.file_name, err.line_number);
            }
            // Everything frida-rust could not type.  An exception thrown
            // inside a hook callback lands here rather than in Error, because
            // it carries no fileName and the typed variant requires one -- so
            // the one message that matters most used to print as a wall of
            // escaped JSON with the real reason buried in it.
            Message::Other(value) => eprintln!("[agent] {}", other(&value)),
        }
    }
}

/// Digs the description and the stack out of an untyped agent message.
fn other(value: &Value) -> String {
    let Some(text) = value.get("data").and_then(Value::as_str) else {
        return value.to_string();
    };
    let Ok(inner) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    let Some(description) = inner.get("description").and_then(Value::as_str) else {
        return text.to_string();
    };
    // The stack is one line per frame in the bundled script, which is the only
    // way to tell which hook was running when it went wrong.
    match inner.get("stack").and_then(Value::as_str) {
        Some(stack) => {
            let frames: Vec<&str> = stack
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            format!("{description}
    {}", frames.join("
    "))
        }
        None => description.to_string(),
    }
}

impl Router {
    fn dispatch(&self, payload: frida::SendPayload) {
        // The agent reuses frida-rust's SendPayload shape: `type` is the frame
        // kind, `id` the sequence, `result` the event name, `returns` the
        // fields.  Odd names, but it avoids a fallback parse path here.
        let verb = match payload.r#type.as_str() {
            "evt" => "EVT",
            "ask" => "ASK",
            "res" => "RES",
            "log" => {
                eprintln!("[agent] {}", payload.result);
                return;
            }
            other => {
                eprintln!("[agent] Unknown frame type: {other}");
                return;
            }
        };

        let mut frame = Frame::new(verb, payload.id as u64, &payload.result);
        frame.fields = flatten(&payload.returns);

        if verb == "ASK" {
            self.pending.open(frame.seq);
        }
        if self.to_sal.send(frame.encode()).is_err() {
            *self.dropped.lock().unwrap() += 1;
        }
    }
}

/// JSON object -> flat string fields.  Nested values are not part of the wire
/// format: an event that needs structure is an event that needs splitting.
fn flatten(value: &Value) -> Vec<(String, String)> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                Value::Null => return None,
                Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
                Value::String(s) => s.clone(),
                // A nested object survives this as a blob of JSON in one
                // field, which reads like it worked and is not: Coderpack looks
                // fields up by name and finds nothing.  Say so.
                Value::Object(_) | Value::Array(_) => {
                    eprintln!(
                        "[agent] Field {key} is nested, but the protocol accepts only flat fields; \
                         Coderpack will ignore its contents"
                    );
                    value.to_string()
                }
                other => other.to_string(),
            };
            Some((key.clone(), text))
        })
        .collect()
}

/// Turns a frame from Coderpack into work for the poster thread.
pub fn from_sal(frame: &Frame, pending: &Pending) -> Option<ToAgent> {
    match frame.verb.as_str() {
        "END" => {
            pending.close(frame.seq);
            Some(ToAgent::Verdict {
                seq: frame.seq,
                cancel: frame.field("cancel") == Some("1"),
                set: frame.rewrites(),
            })
        }
        "CMD" => Some(ToAgent::Command {
            seq: frame.seq,
            name: frame.name.clone(),
            fields: frame.fields.clone(),
        }),
        "LOG" => {
            eprintln!("[coderpack] {} {}", frame.name, frame.encode());
            None
        }
        "BYE" => Some(ToAgent::Asking(false)),
        other => {
            eprintln!("[coderpack] Unknown frame type: {other}");
            None
        }
    }
}
