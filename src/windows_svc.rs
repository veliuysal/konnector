#![cfg(windows)]

use crate::application;
use crate::env_file;
use crate::paths;
use std::{
    ffi::OsString,
    sync::mpsc,
    time::Duration,
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

define_windows_service!(ffi_service_main, service_main);

/// Returns true when this process was launched by the Service Control Manager
/// and the service has finished (dispatcher ran to completion).
pub fn try_start_dispatcher() -> bool {
    match service_dispatcher::start(paths::SERVICE_NAME, ffi_service_main) {
        Ok(()) => true,
        Err(_) => false,
    }
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = run_service() {
        eprintln!("konnector service error: {error}");
    }
}

fn run_service() -> Result<(), String> {
    env_file::load_if_present(&paths::env_file());

    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let event_handler = move |control| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(paths::SERVICE_NAME, event_handler)
        .map_err(|error| format!("cannot register service control handler: {error}"))?;

    status_handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
        .map_err(|error| format!("cannot set running status: {error}"))?;

    let server = std::thread::Builder::new()
        .name("konnector-server".into())
        .spawn(|| {
            std::env::set_var("KONNECTOR_SERVICE", "1");
            application::run();
        })
        .map_err(|error| format!("cannot start server thread: {error}"))?;

    let _ = shutdown_rx.recv();

    status_handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::StopPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 1,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
        .ok();

    // Pingora's run_forever has no public stop handle; exit the process on service stop.
    let _ = server;
    std::process::exit(0);
}
