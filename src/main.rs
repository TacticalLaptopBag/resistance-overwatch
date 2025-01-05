mod cons;

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use json::JsonValue;
use log::{error, warn, LevelFilter};
use simplelog::TermLogger;
use syslog::{BasicLogger, Facility, Formatter3164};

fn handle_advisor(mut stream: UnixStream) {
    println!("Accepted incoming connection");
    let write_lock = Arc::new(Mutex::new(()));

    let mut heartbeat_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to clone handle to socket: {}", e);
            process::exit(1);
        }
    };
    let heartbeat_write_lock = Arc::clone(&write_lock);

    // Heartbeat thread
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(10));
        let mut msg = JsonValue::new_object();
        msg["type"] = "heartbeat".into();

        let _guard = match heartbeat_write_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        socket_send_msg(&mut heartbeat_stream, msg);
    });

    loop {
        let _msg = socket_read_msg(&mut stream);
    }
}

fn socket_read_msg(stream: &mut UnixStream) -> JsonValue {
    let msg_len = match deserialize_length(stream) {
        Ok(len) => len,
        Err(e) => {
            error!("Failed to read message length from Advisor: {}", e);
            process::exit(1);
        }
    };
    let mut msg_bytes = vec![0u8; msg_len];
    let _ = stream.read_exact(&mut msg_bytes);

    let msg_str = match String::from_utf8(msg_bytes) {
        Ok(str) => str,
        Err(e) => {
            error!(
                "Incoming message from Advisor was not encoded in UTF-8: {}",
                e
            );
            process::exit(1);
        }
    };
    let msg = match json::parse(msg_str.as_str()) {
        Ok(msg) => msg,
        Err(e) => {
            // TODO: Pass error up
            error!(
                "Incoming message from Advisor was in an invalid format: {}",
                e
            );
            process::exit(1);
        }
    };
    println!("RX: {}", msg);
    return msg;
}

fn socket_send_msg(stream: &mut UnixStream, msg: JsonValue) {
    println!("TX: {}", msg);
    let msg_str = json::stringify(msg);
    let msg_bytes = msg_str.into_bytes();
    let msg_len_bytes = serialize_length(msg_bytes.len());

    stream
        .write_all(&msg_len_bytes)
        .and(stream.write_all(&msg_bytes))
        .and(stream.flush())
        .unwrap_or_else(|e| {
            error!("Failed to write message to Advisor: {}", e);
            process::exit(1);
        });
}

pub fn serialize_length(len: usize) -> [u8; 4] {
    return (len as u32).to_ne_bytes();
}

pub fn deserialize_length<T: Read>(stream: &mut T) -> io::Result<usize> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_ne_bytes(len_buf) as usize;
    return Ok(len);
}

fn init_logging() {
    // Setup system logging
    let formatter = Formatter3164 {
        facility: Facility::LOG_DAEMON,
        hostname: None,
        process: "resistance-overwatch".into(),
        pid: process::id(),
    };

    let syslog_logger = match syslog::unix(formatter) {
        Ok(logger) => logger,
        Err(e) => {
            error!("Failed to connect to syslog: {}", e);
            process::exit(1);
        }
    };

    let syslog_box = Box::new(BasicLogger::new(syslog_logger));

    // Setup terminal logging to stderr
    let term_logger = TermLogger::new(
        LevelFilter::Warn,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Auto,
    );

    match multi_log::MultiLogger::init(vec![syslog_box, term_logger], cons::LOG_LEVEL) {
        Ok(_) => (),
        Err(e) => {
            error!("Failed to initialize loggers: {}", e);
            process::exit(1);
        }
    }
}

fn main() {
    init_logging();

    if fs::exists(cons::SOCKET_PATH).unwrap_or(false) {
        warn!(
            "Socket file at {} already exists. This file will be forcibly deleted.",
            cons::SOCKET_PATH
        );
        fs::remove_file(cons::SOCKET_PATH).unwrap_or_else(|e| {
            warn!("Failed to delete existing socket file: {}", e);
        });
    }

    let listener = UnixListener::bind(cons::SOCKET_PATH).unwrap_or_else(|e| {
        error!("Failed to bind to {}: {}", cons::SOCKET_PATH, e);
        process::exit(1);
    });
    println!("Listening at {}", cons::SOCKET_PATH);

    ctrlc::set_handler(|| {
        fs::remove_file(cons::SOCKET_PATH).unwrap_or_else(|e| {
            warn!("Failed to delete socket file while handling signal: {}", e);
        });
        process::exit(1);
    })
    .unwrap_or_else(|e| {
        warn!("Failed to set signal handlers: {}", e);
    });

    loop {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    thread::spawn(move || handle_advisor(stream));
                }
                Err(e) => error!("Error accepting connection: {}", e),
            }
        }
    }
}
