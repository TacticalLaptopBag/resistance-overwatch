mod cons;
mod models;

use std::{
    collections::HashSet, fs, io::{self, BufRead, Read, Write}, os::unix::{fs::PermissionsExt, net::{UnixListener, UnixStream}}, process, sync::{Arc, Mutex}, thread, time::Duration
};

use log::{debug, error, info, warn};
use models::{AdvisorMsg, OverwatchMsg};
use notify_rust::Notification;
use resistance_civil_protection::CivilProtection;
use simplelog::TermLogger;
use syslog::{BasicLogger, Facility, Formatter3164};
use url::Url;
use resistance_civil_protection;

fn handle_advisor(mut stream: UnixStream, cp: Arc<Mutex<CivilProtection>>, blockset: Arc<Mutex<HashSet<String>>>) {
    info!("Accepted incoming connection");
    let write_lock = Arc::new(Mutex::new(()));

    let mut heartbeat_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to clone handle to socket: {}", e);
            return;
        }
    };
    let heartbeat_write_lock = Arc::clone(&write_lock);

    // Heartbeat thread
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(10));
        let msg = OverwatchMsg::Heartbeat {  };

        let _guard = match heartbeat_write_lock.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        match socket_send_msg(&mut heartbeat_stream, msg) {
            Ok(_) => (),
            Err(e) => {
                error!("Failed to write message to Advisor: {}", e);
                break;
            }
        }
    });

    loop {
        let msg = match socket_read_msg(&mut stream) {
            Ok(msg) => msg,
            Err(e) => {
                error!("Failed to read message from Advisor: {}", e);
                break;
            }
        };

        socket_handle_msg(msg, &cp, &blockset);
    }
}

fn socket_read_msg(stream: &mut UnixStream) -> Result<AdvisorMsg, String> {
    let msg_len = match deserialize_length(stream) {
        Ok(len) => len,
        Err(e) => return Err(format!("Failed to read message length from Advisor: {}", e)),
    };
    let mut msg_bytes = vec![0u8; msg_len];
    match stream.read_exact(&mut msg_bytes) {
        Ok(_) => (),
        Err(e) => return Err(format!("Failed to read message from Advisor: {}", e)),
    };

    let msg_str = match String::from_utf8(msg_bytes) {
        Ok(str) => str,
        Err(e) => {
            return Err(format!(
                "Incoming message from Advisor was not encoded in UTF-8: {}",
                e
            ))
        }
    };
    let msg = match serde_json::from_str(msg_str.as_str()) {
        Ok(msg) => msg,
        Err(e) => {
            return Err(format!(
                "Incoming message from Advisor was in an invalid format: {}",
                e
            ))
        }
    };

    debug!("RX: {:?}", msg);
    return Ok(msg);
}

fn socket_send_msg(stream: &mut UnixStream, msg: OverwatchMsg) -> Result<(), io::Error> {
    debug!("TX: {:?}", msg);
    let msg_str = serde_json::to_string(&msg)?;
    let msg_bytes = msg_str.into_bytes();
    let msg_len_bytes = serialize_length(msg_bytes.len());

    stream.write_all(&msg_len_bytes)?;
    stream.write_all(&msg_bytes)?;
    stream.flush()?;
    return Ok(());
}

fn socket_handle_msg(msg: AdvisorMsg, cp_lock: &Arc<Mutex<CivilProtection>>, blockset_lock: &Arc<Mutex<HashSet<String>>>) {
    match msg {
        AdvisorMsg::Heartbeat { incognito } => {
            debug!("Heartbeat received. Incognito mode is {}", if incognito { "allowed" } else { "not allowed" });
        }
        AdvisorMsg::Navigation { url } => {
            let parsed_url = Url::parse(url.as_str()).unwrap();
            let host_str = parsed_url.host_str().unwrap();
            debug!("User navigated to {}", host_str);

            let blockset = blockset_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            if blockset.contains(host_str) {
                info!("User navigated to blocked url: {}", host_str);
                let cp = cp_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());

                match cp.notify_squadmates() {
                    Ok(_) => info!("Email notification sent to Squadmates"),
                    Err(e) => error!("Failed to notify Squadmates: {}", e),
                };
            }
        },
    }
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
        cons::TERM_LOG_LEVEL,
        simplelog::Config::default(),
        simplelog::TerminalMode::Stderr,
        simplelog::ColorChoice::Auto,
    );

    multi_log::MultiLogger::init(vec![syslog_box, term_logger], cons::LOG_LEVEL).unwrap_or_else(|e| {
        error!("Failed to initialize loggers: {}", e);
        process::exit(1);
    });
}

fn get_blocked_set() -> HashSet<String> {
    info!("Loading blocklist...");
    // This takes approximately 12 seconds and consumes nearly 1 GB of RAM with the 402MB
    // blocklist.
    // NOTE: The above was done in Debug. In Release, it takes 3 seconds and only consumes ~600 MB
    // RAM with the 402 MB blocklist. 
    let mut blocklist = HashSet::new();
    let blocklist_file = fs::File::open("/var/cache/resistance/blocklist.txt").unwrap();
    let blocklist_lines = io::BufReader::new(blocklist_file).lines();
    for line in blocklist_lines {
        match line {
            Ok(line) => {
                // `127.0.0.1 `
                if line.len() <= 10 {
                    warn!("Blocklist line is less than 10 characters long. Is it in the correct format?");
                    continue;
                }
                blocklist.insert(line[10..].to_owned());
            },
            Err(e) => {
                warn!("Failed to read line from blocklist: {}", e);
            }
        }
    }
    info!("Blocklist loaded.");
    return blocklist;
}

fn main() {
    init_logging();

    info!("Initializing Civil Protection...");
    let mut cp = CivilProtection::new();
    cp.login().unwrap_or_else(|_| {
        Notification::new()
            .summary("Resistance")
            .body("Unable to login to email. Please set this up using `cmacm`.")
            .show()
            .unwrap();
        error!("Failed to login to Civil Protection!");
        process::exit(1);
    });
    info!("Logged in with Civil Protection.");

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
    fs::set_permissions(cons::SOCKET_PATH, fs::Permissions::from_mode(0o777)).unwrap_or_else(|e| {
        error!("Failed to set socket file permissions: {}", e);
        process::exit(1);
    });
    info!("Listening at {}", cons::SOCKET_PATH);

    ctrlc::set_handler(|| {
        fs::remove_file(cons::SOCKET_PATH).unwrap_or_else(|e| {
            warn!("Failed to delete socket file while handling signal: {}", e);
        });
        process::exit(1);
    })
    .unwrap_or_else(|e| {
        warn!("Failed to set signal handlers: {}", e);
    });

    let cp_mutex = Arc::new(Mutex::new(cp));
    let blockset = Arc::new(Mutex::new(get_blocked_set()));

    loop {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let stream_cp = Arc::clone(&cp_mutex);
                    let stream_blockset = Arc::clone(&blockset);
                    thread::spawn(move || handle_advisor(stream, stream_cp, stream_blockset));
                }
                Err(e) => error!("Error accepting connection: {}", e),
            }
        }
    }
}

