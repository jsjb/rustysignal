use std::cell::RefCell;
use std::rc::Rc;
use std::str;
#[cfg(feature = "ssl")]
use std::thread::sleep;
#[cfg(feature = "ssl")]
use std::time::Duration;

use serde_json::Value;

#[cfg(feature = "ssl")]
use ws::util::TcpStream;
use ws::{CloseCode, Handler, Handshake, Message, Result};

#[cfg(feature = "ssl")]
use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod, SslStream};

#[cfg(feature = "auth")]
use rusqlite::{OpenFlags, NO_PARAMS};

use network::Network;
use node::Node;

struct Server {
    node: Rc<RefCell<Node>>,
    #[cfg(feature = "ssl")]
    ssl: Rc<SslAcceptor>,
    #[cfg(feature = "auth")]
    db: Rc<rusqlite::Connection>,
    network: Rc<RefCell<Network>>,
}

impl Server {
    #[cfg(feature = "push")]
    fn handle_push_requests(&mut self, json_message: &Value) {
        match json_message["action"].as_str() {
            Some("subscribe-push") => match json_message["subscriptionData"].as_str() {
                Some(data) => {
                    self.network.borrow_mut().add_subscription(data, &self.node);
                }
                _ => println!("No subscription data"),
            },
            Some("connection-request") => match json_message["endpoint"].as_str() {
                Some(endpoint) => {
                    let user_sending_request = self.node.borrow().owner.clone().unwrap();
                    self.network
                        .borrow()
                        .send_push(&user_sending_request, &endpoint);
                }
                _ => println!("No endpoint for connection request"),
            },
            _ => { /* Do nothing if the user is not interested in the push */ }
        };
    }
}

impl Handler for Server {
    fn on_open(&mut self, handshake: Handshake) -> Result<()> {
        // Get the aruments from a URL
        // i.e localhost:8000/?user=testuser

        // skip()ing everything before the first '?' allows us to run the
        // server behind a reverse proxy like nginx with minimal fuss
        let url_arguments = handshake
            .request
            .resource()
            .split(|c| c == '?' || c == '=' || c == '&')
            .skip(1);
        // Beeing greedy by not collecting pairs
        // Instead every even number (including 0) will be an identifier
        // and every odd number will be the assigned value
        let argument_vector: Vec<&str> = url_arguments.collect();

        if argument_vector.len() >= 2 && argument_vector[0] == "user" {
            let username: &str = argument_vector[1];

            #[cfg(feature = "auth")]
            {
                println!("authenticating {}...", username);

                if argument_vector.len() >= 4 && argument_vector[2] == "pass" {
                    let password: &[u8] = argument_vector[3].as_bytes();

                    let db_hash = self.db.query_row(
                        "SELECT `password` FROM `rustysignal_users` WHERE `name` = ?",
                        &[username],
                        |row| row.get(0),
                    );

                    let db_hash: String = match db_hash {
                        Ok(hash) => hash,
                        Err(e) => {
                            println!("When looking up username/password, got error '{}'", e);
                            String::new()
                        }
                    };

                    let passwords_match: bool = if !db_hash.eq("") {
                        match bcrypt::verify(password, &db_hash) {
                            Ok(b) => b,
                            Err(e) => {
                                println!("When comparing password hashes, got error '{}'", e);
                                false
                            }
                        }
                    } else {
                        false
                    };

                    if passwords_match {
                        self.network.borrow_mut().add_user(username, &self.node);
                        let sender = &self.node.borrow().sender;
                        sender.send("\"Authentication succeeded.\"\n").unwrap();
                    } else {
                        println!(
                            "Node attempting to log in as user {} provided the wrong password, or username doesn't exist",
                            username
                        );
                        let sender = &self.node.borrow().sender;
                        sender.send("\"Authentication failed.\"\n").unwrap();
                        sender.close(ws::CloseCode::Policy).unwrap();
                    }
                } else {
                    println!(
                        "Node attempting to log in as user {} did not provide a password",
                        username
                    );
                    let sender = &self.node.borrow().sender;
                    sender.send("\"Authentication failed.\"\n").unwrap();
                    sender.close(ws::CloseCode::Policy).unwrap();
                }
            }

            #[cfg(not(feature = "auth"))]
            self.network.borrow_mut().add_user(username, &self.node);
        } else {
            println!("New node didn't provide a username");
        }

        println!(
            "Network expanded to {:?} connected nodes",
            self.network.borrow().size()
        );
        Ok(())
    }

    #[cfg(feature = "ssl")]
    fn upgrade_ssl_server(&mut self, sock: TcpStream) -> ws::Result<SslStream<TcpStream>> {
        println!("Server node upgraded");
        // TODO  This is weird, but the sleep is needed...
        sleep(Duration::from_millis(200));
        self.ssl.accept(sock).map_err(From::from)
    }

    fn on_message(&mut self, msg: Message) -> Result<()> {
        let text_message: &str = msg.as_text()?;
        let json_message: Value = serde_json::from_str(text_message).unwrap_or(Value::default());

        // !!! WARNING !!!
        // The word "protocol" match is protcol specific.
        // Thus a client should make sure to send a viable protocol
        let protocol = match json_message["protocol"].as_str() {
            Some(desired_protocol) => Some(desired_protocol),
            _ => None,
        };

        // The words below are protcol specific.
        // Thus a client should make sure to use a viable protocol
        let ret = match protocol {
            Some("one-to-all") => self.node.borrow().sender.broadcast(text_message),
            Some("one-to-self") => self.node.borrow().sender.send(text_message),
            Some("one-to-one") => match json_message["endpoint"].as_str() {
                Some(endpoint) => {
                    let network = self.network.borrow();
                    let endpoint_node = network
                        .nodemap
                        .borrow()
                        .get(endpoint)
                        .and_then(|node| node.upgrade());

                    match endpoint_node {
                        Some(node) => {
                            println!(
                                "sending message from {} to {}",
                                self.node
                                    .borrow()
                                    .owner
                                    .as_ref()
                                    .unwrap_or(&String::from("<unknown>")),
                                endpoint
                            );
                            node.borrow().sender.send(text_message)
                        }
                        _ => {
                            println!(
                                "Node {} tried to send a message to {}, who's not here",
                                self.node
                                    .borrow()
                                    .owner
                                    .as_ref()
                                    .unwrap_or(&String::from("<unknown>")),
                                endpoint
                            );
                            self.node.borrow().sender.send(format!(
                                "\"Could not find a node with the name {}\"",
                                endpoint
                            ))
                        }
                    }
                }
                _ => {
                    println!(
                        "user {} tried to send a one-to-one message to nobody!",
                        self.node.borrow().owner.as_ref().unwrap()
                    );
                    self.node
                        .borrow()
                        .sender
                        .send("\"No field 'endpoint' provided\"")
                }
            },
            _ => {
                println!(
                    "user {} tried to send a message with an invalid or missing protocol",
                    self.node
                        .borrow()
                        .owner
                        .as_ref()
                        .unwrap_or(&String::from("<unknown>"))
                );
                self.node.borrow().sender.send(
                    "\"Invalid protocol, valid protocols include: 'one-to-one', 'one-to-self', 'one-to-all'\"",
                )
            }
        };

        #[cfg(feature = "push")]
        self.handle_push_requests(&json_message);

        return ret;
    }

    fn on_close(&mut self, code: CloseCode, reason: &str) {
        // Remove the node from the network
        if let Some(owner) = &self.node.borrow().owner {
            match code {
                CloseCode::Normal => println!("{:?} is done with the connection.", owner),
                CloseCode::Away => println!("{:?} left the site.", owner),
                CloseCode::Abnormal => println!("Closing handshake for {:?} failed!", owner),
                _ => println!("{:?} encountered an error: {:?}", owner, reason),
            };

            self.network.borrow_mut().remove(owner)
        }

        println!(
            "Network shrinked to {:?} connected nodes\n",
            self.network.borrow().size()
        );
    }

    fn on_error(&mut self, err: ws::Error) {
        println!("The server encountered an error: {:?}", err);
    }
}

pub fn run() {
    // Setup logging
    env_logger::init();

    // setup command line arguments
    let mut app = clap::App::new("Rustysignal")
        .version("2.0.0")
        .author("Rasmus Viitanen <rasviitanen@gmail.com>")
        .about("A signaling server implemented in Rust that can be used for e.g. WebRTC, see https://github.com/rasviitanen/rustysignal")
        .arg(
            clap::Arg::with_name("ADDR")
            .help("Address on which to bind the server e.g. 127.0.0.1:3012")
            .required(true)
        );

    if cfg!(feature = "ssl") {
        app = app
            .arg(
                clap::Arg::with_name("CERT")
                    .help("Path to the SSL certificate.")
                    .required(true),
            )
            .arg(
                clap::Arg::with_name("KEY")
                    .help("Path to the SSL certificate key.")
                    .required(true),
            );
    }

    if cfg!(feature = "push") {
        app = app.arg(
            clap::Arg::with_name("VAPIDKEY")
                .help("A NIST P256 EC private key to create a VAPID signature, used for push")
                .required(true),
        );
    }

    if cfg!(feature = "auth") {
        app = app.arg(
            clap::Arg::with_name("AUTHDB")
            .help("Path to SQLite database file for authentication (it will be created if not found)")
            .required(true)
        );
    }

    let matches = app.get_matches();

    #[cfg(feature = "auth")]
    let authdbpath = std::path::Path::new(matches.value_of("AUTHDB").unwrap());

    #[cfg(feature = "auth")]
    let dbconn: rusqlite::Connection =
        match rusqlite::Connection::open_with_flags(authdbpath, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(db) => db,
            Err(_) => {
                // regular open() is equivalent to READ_WRITE | CREATE, so this attempts to create
                // the db and close the connection.
                let db = rusqlite::Connection::open(authdbpath).unwrap();
                db.execute(
                    "CREATE TABLE rustysignal_users(name TEXT, password TEXT);",
                    NO_PARAMS,
                )
                .unwrap();
                db.close().unwrap();
                rusqlite::Connection::open_with_flags(authdbpath, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap()
            }
        };

    #[cfg(feature = "auth")]
    let db = Rc::new(dbconn);

    #[cfg(feature = "ssl")]
    let acceptor = Rc::new({
        println!("Building acceptor");
        let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
        builder
            .set_private_key_file(matches.value_of("KEY").unwrap(), SslFiletype::PEM)
            .unwrap();
        builder
            .set_certificate_chain_file(matches.value_of("CERT").unwrap())
            .unwrap();

        builder.build()
    });

    println!("------------------------------------");
    #[cfg(not(feature = "ssl"))]
    {
        println!("rustysignal is listening on address");
        println!("ws://{}", matches.value_of("ADDR").unwrap());
        println!("To use SSL you need to reinstall rustysignal using 'cargo install rustysignal --features ssl --force");
        #[cfg(not(feature = "push"))]
        {
            println!("To enable push notifications, you need to reinstall rustysignal using 'cargo install rustysignal --features push --force");
            println!("For both, please reinstall using 'cargo install rustysignal --features 'ssl push' --force");
        }
    }

    #[cfg(feature = "ssl")]
    {
        println!("rustysignal is listening on securily on address");
        println!("wss://{}", matches.value_of("ADDR").unwrap());
        println!("To disable SSL you need to reinstall rustysignal using 'cargo install rustysignal --force");
        #[cfg(not(feature = "push"))]
        println!("To enable push notifications, you need to reinstall rustysignal using 'cargo install rustysignal --features 'ssl push' --force");
    }
    println!("-------------------------------------");

    let network = Rc::new(RefCell::new(Network::default()));

    #[cfg(feature = "push")]
    network
        .borrow_mut()
        .set_vapid_path(matches.value_of("VAPIDKEY").unwrap());

    #[cfg(feature = "ssl")]
    let encrypt_server = true;
    #[cfg(not(feature = "ssl"))]
    let encrypt_server = false;

    ws::Builder::new()
        .with_settings(ws::Settings {
            encrypt_server: encrypt_server,
            ..ws::Settings::default()
        })
        .build(|sender: ws::Sender| {
            println!("Building server");
            let node = Node::new(sender);
            Server {
                node: Rc::new(RefCell::new(node)),
                #[cfg(feature = "ssl")]
                ssl: acceptor.clone(),
                #[cfg(feature = "auth")]
                db: db.clone(),
                network: network.clone(),
            }
        })
        .unwrap()
        .listen(matches.value_of("ADDR").unwrap())
        .unwrap();
}
