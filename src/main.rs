extern crate ws;
#[macro_use]
extern crate serde_json;

extern crate clap;
extern crate env_logger;

extern crate base64;
extern crate futures;
extern crate serde;
extern crate tokio;

#[cfg(feature = "ssl")]
extern crate openssl;
#[cfg(feature = "auth")]
extern crate rusqlite;
#[cfg(feature = "push")]
extern crate web_push;

mod server;

mod network;
mod node;

fn main() {
    server::run()
}
