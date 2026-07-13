use crate::configs::TcpProxyConfig;
use crate::configs::LogLevel;
use crate::request_logging;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct ListenerHandle {
    shutdown: Arc<AtomicBool>,
    joins: Vec<JoinHandle<()>>,
}

pub struct TcpProxyManager {
    listeners: Mutex<Vec<ListenerHandle>>,
    logging: Mutex<LogLevel>,
}

impl TcpProxyManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            listeners: Mutex::new(Vec::new()),
            logging: Mutex::new(LogLevel::Off),
        })
    }

    pub fn apply(self: &Arc<Self>, configs: Vec<TcpProxyConfig>, logging: LogLevel) {
        *self
            .logging
            .lock()
            .expect("tcp proxy lock poisoned") = logging;
        self.stop_all();
        for config in configs {
            let stem = if config.source_file.is_empty() {
                config.name.clone()
            } else {
                config.source_file.clone()
            };
            crate::file_log::prepare_site(
                &stem,
                &format!("tcp enabled; listen={} name={}", config.listen, config.name),
            );
            // Enabled TCP YAMLs stay loggable even if root logging is off.
            let level = if logging.is_enabled() {
                logging
            } else {
                LogLevel::Info
            };
            self.start_one(config, level);
        }
    }

    fn stop_all(&self) {
        let mut listeners = self.listeners.lock().expect("tcp proxy lock poisoned");
        for handle in listeners.drain(..) {
            handle.shutdown.store(true, Ordering::SeqCst);
            for join in handle.joins {
                let _ = join.join();
            }
        }
    }

    fn start_one(self: &Arc<Self>, config: TcpProxyConfig, logging: LogLevel) {
        let upstream = match config.upstream.address() {
            Ok(address) => address,
            Err(error) => {
                log::error!("tcp {}: {error}", config.name);
                return;
            }
        };
        let listen_addrs = match config.listen_addresses() {
            Ok(addresses) => addresses,
            Err(error) => {
                log::error!("tcp {}: {error}", config.name);
                return;
            }
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let name = config.name.clone();
        let source_file = if config.source_file.is_empty() {
            name.clone()
        } else {
            config.source_file.clone()
        };
        let mut joins = Vec::with_capacity(listen_addrs.len());
        for listen in listen_addrs {
            let shutdown = shutdown.clone();
            let upstream = upstream.clone();
            let name = name.clone();
            let source_file = source_file.clone();
            let thread_name = name.clone();
            let join = thread::Builder::new()
                .name(format!("tcp-{name}"))
                .spawn(move || {
                    run_listener(
                        &thread_name,
                        &source_file,
                        &listen,
                        &upstream,
                        shutdown,
                        logging,
                    )
                })
                .unwrap_or_else(|error| panic!("cannot start tcp proxy {name}: {error}"));
            joins.push(join);
        }
        self.listeners
            .lock()
            .expect("tcp proxy lock poisoned")
            .push(ListenerHandle { shutdown, joins });
    }
}

fn run_listener(
    name: &str,
    source_file: &str,
    listen: &str,
    upstream: &str,
    shutdown: Arc<AtomicBool>,
    logging: LogLevel,
) {
    let listener = match TcpListener::bind(listen) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!("tcp {name}: cannot bind {listen}: {error}");
            return;
        }
    };
    if let Err(error) = listener.set_nonblocking(true) {
        log::error!("tcp {name}: cannot configure nonblocking listener: {error}");
        return;
    }
    log::info!("tcp {name}: listening on {listen} -> {upstream}");

    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((client, address)) => {
                let upstream = upstream.to_owned();
                let name = name.to_owned();
                let source_file = source_file.to_owned();
                thread::spawn(move || {
                    relay_connection(&name, &source_file, client, &upstream, address, logging);
                });
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                log::warn!("tcp {name}: accept error: {error}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    log::info!("tcp {name}: stopped {listen}");
}

fn relay_connection(
    name: &str,
    source_file: &str,
    mut client: TcpStream,
    upstream: &str,
    client_addr: SocketAddr,
    logging: LogLevel,
) {
    let started = Instant::now();
    let log_enabled = request_logging::access_logging_enabled(logging);

    let mut server = match connect_upstream(upstream) {
        Ok(server) => server,
        Err(error) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    0,
                    0,
                    started.elapsed(),
                    Some(&error.to_string()),
                );
            }
            return;
        }
    };

    let mut client_read = match client.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    0,
                    0,
                    started.elapsed(),
                    Some(&error.to_string()),
                );
            }
            return;
        }
    };
    let mut server_read = match server.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    0,
                    0,
                    started.elapsed(),
                    Some(&error.to_string()),
                );
            }
            return;
        }
    };

    let client_to_server = thread::spawn(move || io::copy(&mut client, &mut server));
    let server_to_client = thread::spawn(move || io::copy(&mut server_read, &mut client_read));

    let bytes_up = match client_to_server.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    0,
                    0,
                    started.elapsed(),
                    Some(&error.to_string()),
                );
            }
            return;
        }
        Err(_) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    0,
                    0,
                    started.elapsed(),
                    Some("client -> upstream relay failed"),
                );
            }
            return;
        }
    };
    let bytes_down = match server_to_client.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    bytes_up,
                    0,
                    started.elapsed(),
                    Some(&error.to_string()),
                );
            }
            return;
        }
        Err(_) => {
            if log_enabled {
                request_logging::log_tcp_connection(
                    name,
                    source_file,
                    client_addr,
                    upstream,
                    bytes_up,
                    0,
                    started.elapsed(),
                    Some("upstream -> client relay failed"),
                );
            }
            return;
        }
    };

    if log_enabled {
        request_logging::log_tcp_connection(
            name,
            source_file,
            client_addr,
            upstream,
            bytes_up,
            bytes_down,
            started.elapsed(),
            None,
        );
    }
}

fn connect_upstream(upstream: &str) -> io::Result<TcpStream> {
    let mut last_error = None;
    for address in upstream.to_socket_addrs()? {
        match TcpStream::connect(address) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(ErrorKind::NotFound, format!("no upstream address resolved for {upstream}"))
    }))
}

pub fn reload(manager: &Arc<TcpProxyManager>) {
    let root = crate::configs::server();
    let configs = crate::validation::filter_valid_tcp(crate::configs::load_tcp_lenient());
    if configs.is_empty() {
        log::info!("tcp proxy reload: no valid tcp configs");
    } else {
        log::info!("tcp proxy reload: {} service(s)", configs.len());
    }
    manager.apply(configs, root.logging);
}
