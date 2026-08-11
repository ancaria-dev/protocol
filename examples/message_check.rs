//! Proves a message from an injected script actually reaches the host.
//!
//! This is the regression the game exposed and nothing else did: upstream
//! frida 0.17.2 casts the callback's user_data to the wrong type, so the first
//! `send()` faulted the process with no message ever arriving. Attaching to a
//! throwaway process reproduces the whole path in a second.
//!
//!     cargo run --example message_check

use std::process::{Child, Command};
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

use frida::{DeviceManager, Frida, Message, ScriptHandler, ScriptOption};

struct Collector(Sender<String>);

impl ScriptHandler for Collector {
    fn on_message(&mut self, message: Message, _data: Option<Vec<u8>>) {
        if let Message::Send(send) = message {
            let _ = self.0.send(format!("{} {}", send.payload.r#type, send.payload.result));
        }
    }
}

fn victim() -> Child {
    Command::new("python")
        .args(["-c", "import time; time.sleep(20)"])
        .spawn()
        .expect("python is needed for this check")
}

fn main() {
    let mut child = victim();
    std::thread::sleep(Duration::from_millis(400));

    let (tx, rx) = channel();
    let frida = unsafe { Frida::obtain() };
    let manager = DeviceManager::obtain(&frida);
    let device = manager.get_local_device().expect("local device");
    let session = device.attach(child.id()).expect("attach");

    let mut options = ScriptOption::new().set_name("message-check");
    let mut script = session
        .create_script(
            r#"send({ type: "evt", id: 0, result: "ping", returns: {} });"#,
            &mut options,
        )
        .expect("create script");
    script.handle_message(Collector(tx)).expect("handler");
    script.load().expect("load");

    let outcome = rx.recv_timeout(Duration::from_secs(5));
    let _ = script.unload();
    let _ = child.kill();

    match outcome {
        Ok(message) if message == "evt ping" => println!("OK: Received {message:?}"),
        Ok(message) => {
            println!("FAIL: Unexpected message {message:?}");
            std::process::exit(1);
        }
        Err(_) => {
            println!("FAIL: No message arrived within 5 s");
            std::process::exit(1);
        }
    }
}
