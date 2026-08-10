# vendor/frida

A copy of [`frida` 0.17.2](https://crates.io/crates/frida) with one fix.

`call_on_message` in `src/script.rs` receives `user_data`, which
`handle_message` sets to a `*mut CallbackHandler`. For `frida:rpc` messages it
casts it back to that type, correctly. For **every other message** it does:

```rust
let handler: &mut I = &mut *(user_data as *mut I);
handler.on_message(...);
```

`I` is the caller's handler type, so this reinterprets the `CallbackHandler`'s
memory as a different struct and calls a method on it. Every `send()` from the
agent faulted the host with `0xC0000005`, module `unknown` — no message ever
reached us. The handler is in `CallbackHandler::script_handler`, where
`add_handler` put it, so the fix is to read it from there.

Drop this directory and go back to the crates.io dependency once upstream
carries the fix.
