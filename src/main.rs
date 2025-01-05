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

const SOCKET_PATH: &str = "/tmp/overwatch";

fn handle_advisor(mut stream: UnixStream) {
    println!("Accepted incoming connection");
    let write_lock = Arc::new(Mutex::new(()));

    let mut heartbeat_stream = stream.try_clone().unwrap();
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
    let msg_len = deserialize_length(stream).unwrap();
    let mut msg_bytes = vec![0u8; msg_len];
    let _ = stream.read_exact(&mut msg_bytes);

    let msg_str = String::from_utf8(msg_bytes).unwrap();
    let msg = json::parse(msg_str.as_str()).unwrap();
    println!("RX: {}", msg);
    return msg;
}

fn socket_send_msg(stream: &mut UnixStream, msg: JsonValue) {
    println!("TX: {}", msg);
    let msg_str = json::stringify(msg);
    let msg_bytes = msg_str.into_bytes();
    let msg_len_bytes = serialize_length(msg_bytes.len());

    stream.write_all(&msg_len_bytes).unwrap();
    stream.write_all(&msg_bytes).unwrap();
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

fn main() {
    if fs::exists(SOCKET_PATH).unwrap() {
        eprintln!(
            "Socket file at {} already exists. This file will be forcibly deleted.",
            SOCKET_PATH
        );
        match fs::remove_file(SOCKET_PATH) {
            Ok(_) => (),
            Err(e) => {
                eprintln!("Error when trying to delete existing socket: {}", e);
            }
        }
    }
    let listener = UnixListener::bind(SOCKET_PATH).unwrap();
    println!("Listening at {}", SOCKET_PATH);

    let _ = ctrlc::set_handler(|| {
        let _ = fs::remove_file(SOCKET_PATH);
        process::exit(1);
    });

    loop {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    thread::spawn(move || handle_advisor(stream));
                }
                Err(e) => eprintln!("Error accepting connection: {}", e),
            }
        }
    }
}
