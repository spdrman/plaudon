//! `plaudon-sandbox-probe` — a real helper that reports what the sandbox let
//! it do (§6.3A, §16.2 row "Malicious audio decoder input").
//!
//! This binary is the counterpart of
//! [`plaudon_security::sandbox::SandboxedHelper`]. It speaks the same
//! length-prefixed [`plaudon_security::sandbox::ipc`] protocol that a real
//! decoder or inference helper speaks, and it deliberately *attempts* the
//! things a compromised decoder would attempt:
//!
//! * read a file it was granted and a file it was not;
//! * write a file it was granted;
//! * open a TCP connection;
//! * execute another program;
//! * allocate unbounded memory;
//! * burn CPU and wall clock.
//!
//! Every attempt is **reported**, never fatal: the probe answers
//! `Refused { .. }` with the operating-system error instead of crashing, so an
//! integration test can distinguish "the sandbox denied this" from "the helper
//! fell over". The one exception is the non-graceful allocation mode, whose
//! whole purpose is to let the resource cap terminate the process.
//!
//! The probe performs **no work before its first frame arrives**. That is a
//! load-bearing property on Windows, where the parent assigns the process to
//! its job object immediately after `spawn` rather than creating it suspended;
//! see `sandbox::windows` for the full note.
//!
//! It is shipped as a normal `[[bin]]` of `plaudon-security` so the
//! integration tests can locate it through `CARGO_BIN_EXE_plaudon-sandbox-probe`
//! and so it is built with the same lints and profile as the library.

use std::io::{Read, Write};
use std::process::Command;
use std::time::{Duration, Instant};

use plaudon_security::sandbox::ipc::{
    self, FrameError, HelperRequest, HelperResponse, Operation, Refusal, RefusalKind,
};

/// Chunk size used when touching a large allocation, so resident size grows
/// steadily rather than in one jump.
const TOUCH_CHUNK: usize = 4 * 1024 * 1024;

/// Timeout for the deliberate outbound connection attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

fn main() {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    // Memory handed out by `AllocateBytes` is kept alive here; dropping it
    // between requests would make the cap untestable.
    let mut retained: Vec<Vec<u8>> = Vec::new();

    loop {
        let request = match ipc::read_message::<_, HelperRequest>(&mut reader) {
            Ok(request) => request,
            // The parent closed the pipe: exit cleanly.
            Err(FrameError::Closed) => break,
            Err(err) => {
                let _ = ipc::write_frame(
                    &mut writer,
                    &HelperResponse::Refused {
                        operation: Operation::Hello,
                        refusal: Refusal {
                            kind: RefusalKind::Other,
                            os_error: None,
                            detail: err.to_string(),
                        },
                    },
                );
                break;
            }
        };

        if matches!(request, HelperRequest::Shutdown) {
            let _ = ipc::write_frame(
                &mut writer,
                &HelperResponse::Echo {
                    payload: "shutdown".to_owned(),
                },
            );
            break;
        }

        let response = handle(request, &mut retained);
        if ipc::write_frame(&mut writer, &response).is_err() {
            break;
        }
    }
}

fn handle(request: HelperRequest, retained: &mut Vec<Vec<u8>>) -> HelperResponse {
    let operation = request.operation();
    match request {
        HelperRequest::Hello => HelperResponse::Hello {
            limits: ipc::current_limits(),
            pid: std::process::id(),
        },
        HelperRequest::Echo { payload } => HelperResponse::Echo { payload },
        HelperRequest::ReadFile { path } => read_file(&path, operation),
        HelperRequest::WriteFile { path, bytes } => write_file(&path, &bytes, operation),
        HelperRequest::ConnectTcp { host, port } => connect_tcp(&host, port, operation),
        HelperRequest::SpawnChild => spawn_child(operation),
        HelperRequest::AllocateBytes { bytes, graceful } => {
            allocate(bytes, graceful, retained, operation)
        }
        // The probe is not an inference helper: it carries no model runtime,
        // so it says so rather than pretending to have embedded anything.
        HelperRequest::LoadModel { .. } | HelperRequest::EmbedWindow { .. } => {
            HelperResponse::Unsupported { operation }
        }
        HelperRequest::BurnCpu { seconds } => burn_cpu(seconds),
        HelperRequest::SleepMs { ms } => {
            std::thread::sleep(Duration::from_millis(ms));
            HelperResponse::Slept { ms }
        }
        // Handled by the caller before dispatch.
        HelperRequest::Shutdown => HelperResponse::Unsupported { operation },
    }
}

fn refused(operation: Operation, err: &std::io::Error) -> HelperResponse {
    HelperResponse::Refused {
        operation,
        refusal: Refusal::from_io(err),
    }
}

fn read_file(path: &str, operation: Operation) -> HelperResponse {
    use sha2::Digest as _;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => return refused(operation, &err),
    };
    let mut hasher = sha2::Sha256::new();
    // Heap-allocated: this probe is a plain read loop, not a signal handler
    // or other allocation-free hot path.
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buffer[..n]);
                total = total.saturating_add(n as u64);
            }
            Err(err) => return refused(operation, &err),
        }
    }
    HelperResponse::FileRead {
        bytes: total,
        sha256: hex::encode(hasher.finalize()),
    }
}

fn write_file(path: &str, bytes: &[u8], operation: Operation) -> HelperResponse {
    match std::fs::File::create(path).and_then(|mut file| {
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    }) {
        Ok(()) => HelperResponse::FileWritten {
            bytes: bytes.len() as u64,
        },
        Err(err) => refused(operation, &err),
    }
}

fn connect_tcp(host: &str, port: u16, operation: Operation) -> HelperResponse {
    use std::net::{TcpStream, ToSocketAddrs as _};
    let addrs = match (host, port).to_socket_addrs() {
        Ok(addrs) => addrs.collect::<Vec<_>>(),
        Err(err) => return refused(operation, &err),
    };
    let mut last: Option<std::io::Error> = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let local_port = stream.local_addr().map(|a| a.port()).unwrap_or_default();
                return HelperResponse::TcpConnected { local_port };
            }
            Err(err) => last = Some(err),
        }
    }
    match last {
        Some(err) => refused(operation, &err),
        None => HelperResponse::Refused {
            operation,
            refusal: Refusal {
                kind: RefusalKind::NetworkUnreachable,
                os_error: None,
                detail: "no address resolved".to_owned(),
            },
        },
    }
}

fn spawn_child(operation: Operation) -> HelperResponse {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.args(["/C", "exit", "0"]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("/bin/echo");
        command.arg("plaudon-sandbox-probe");
        command
    };
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match command.status() {
        Ok(status) => HelperResponse::ChildSpawned {
            exit_code: status.code().unwrap_or(-1),
        },
        Err(err) => refused(operation, &err),
    }
}

fn allocate(
    bytes: u64,
    graceful: bool,
    retained: &mut Vec<Vec<u8>>,
    operation: Operation,
) -> HelperResponse {
    let Ok(total) = usize::try_from(bytes) else {
        return HelperResponse::Refused {
            operation,
            refusal: Refusal {
                kind: RefusalKind::ResourceExhausted,
                os_error: None,
                detail: "requested size does not fit in this address space".to_owned(),
            },
        };
    };

    let mut done = 0_usize;
    while done < total {
        let chunk = TOUCH_CHUNK.min(total - done);
        let mut block: Vec<u8> = Vec::new();
        if graceful {
            // The audited path a decoder should use: a fallible reservation,
            // reported rather than fatal.
            if block.try_reserve_exact(chunk).is_err() {
                return HelperResponse::Refused {
                    operation,
                    refusal: Refusal {
                        kind: RefusalKind::ResourceExhausted,
                        os_error: None,
                        detail: format!("reservation of {chunk} bytes refused after {done} bytes"),
                    },
                };
            }
        }
        // Resizing touches every page, so the memory is really resident and a
        // resident-set cap can see it.
        block.resize(chunk, 0xA5);
        retained.push(block);
        done += chunk;
    }
    HelperResponse::Allocated { bytes }
}

fn burn_cpu(seconds: u64) -> HelperResponse {
    let started = Instant::now();
    let budget = Duration::from_secs(seconds);
    let mut accumulator: u64 = 0x9E37_79B9_7F4A_7C15;
    loop {
        for _ in 0..200_000_u32 {
            accumulator = accumulator
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            accumulator ^= accumulator >> 7;
        }
        if started.elapsed() >= budget {
            break;
        }
    }
    // Keep the loop from being optimised away without pretending the value
    // means anything.
    if accumulator == 0 {
        std::hint::black_box(&accumulator);
    }
    HelperResponse::CpuBurned {
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}
