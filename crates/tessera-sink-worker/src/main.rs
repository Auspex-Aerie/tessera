//! `tessera-sink-worker` — the executable a [`tessera_sink::Sink`] owner
//! spawns (one process per worker).
//!
//! This is a deliberately thin shell: it parses the argv contract that
//! `tessera_sink::spawn::build_worker_command` produced and hands off to
//! the topology-agnostic [`tessera_sink::run_worker`]. All the real work
//! (region attach, chunk streaming, hash verification, atomic rename)
//! lives in the library, so an in-process host could drive the identical
//! entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    let params = match tessera_sink::spawn::parse_worker_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tessera-sink-worker: invalid arguments: {e}");
            return ExitCode::from(2);
        }
    };

    match tessera_sink::run_worker(params) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tessera-sink-worker: {e}");
            ExitCode::FAILURE
        }
    }
}
