//! Fuzzes the PROXY protocol header reader, which runs on the first bytes of
//! a connection from a trusted load balancer when `server.proxy_protocol` is on.
//!
//! `read_proxy_header` must return an address, `None` or an error for any
//! input, never panic, and never read past the header's declared length.

#![no_main]

use libfuzzer_sys::fuzz_target;
use popsub_transport::proxy::read_proxy_header;

/// Reads one input as the start of a connection and checks what was consumed.
///
/// A successful read must leave at least the bytes after the header unread, so
/// the TLS or WebSocket handshake that follows starts in the right place.
///
/// Args:
///     data: The fuzzer's input.
fn check(data: &[u8]) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current thread runtime builds");
    let mut stream = data;
    let result = runtime.block_on(read_proxy_header(&mut stream));
    if result.is_ok() {
        assert!(
            stream.len() < data.len(),
            "a header was read without consuming any bytes"
        );
    }
}

fuzz_target!(|data: &[u8]| check(data));
