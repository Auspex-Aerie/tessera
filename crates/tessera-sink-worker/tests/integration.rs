//! End-to-end Sink tests that spawn the *real* `tessera-sink-worker`
//! process (true multiprocess path, per the locked v0.1 spawn model).
//!
//! These live in the worker bin crate so Cargo auto-builds the
//! executable and hands us its path via `CARGO_BIN_EXE_tessera-sink-worker`;
//! the Sink owner is driven through `tessera-sink` (a dev-dependency).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tessera_sink::{Sink, SinkConfig};

/// Absolute path to the worker executable Cargo built for this test.
fn worker_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tessera-sink-worker"))
}

/// Unique base description per test invocation so concurrent / repeated
/// runs never collide on a SHM region name.
fn unique_desc(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("tessera-sink-it/{tag}/{}/{nanos}", std::process::id())
}

fn config(tag: &str, worker_count: u32, pool_slot_count: u32, pool_slot_size_bytes: u32) -> SinkConfig {
    SinkConfig {
        description: unique_desc(tag),
        worker_count,
        pool_slot_count,
        pool_slot_size_bytes,
        ttl_micros: 60_000_000,
        acquire_timeout_micros: 15_000_000,
        control_slot_count: 64,
        control_slot_size_bytes: 8192,
        ack_slot_count: 256,
        ack_slot_size_bytes: 8192,
        worker_bin_path: Some(worker_bin()),
        force_recreate: false,
    }
}

#[test]
fn single_chunk_writes_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hello.bin");
    let payload = b"hello tessera sink";

    let mut sink = Sink::start(config("single", 1, 4, 4096)).expect("start");
    sink.submit(path.to_str().unwrap(), payload, false).expect("submit");
    sink.flush().expect("flush");
    drop(sink);

    let written = std::fs::read(&path).expect("read output");
    assert_eq!(written, payload);
}

#[test]
fn multi_chunk_reassembles_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("multi.bin");
    // 64-byte slots, ~200-byte payload → 4 chunks.
    let payload: Vec<u8> = (0..200u32).map(|i| (i % 251) as u8).collect();

    let mut sink = Sink::start(config("multi", 1, 8, 64)).expect("start");
    sink.submit(path.to_str().unwrap(), &payload, false).expect("submit");
    sink.flush().expect("flush");
    drop(sink);

    assert_eq!(std::fs::read(&path).expect("read"), payload);
}

#[test]
fn large_payload_recycles_leases() {
    // Only 2 pool slots but 10 chunks → submit must drain acks to free
    // slots mid-job. Exercises acquire_with_drain + the cross-process
    // ack plane under back-pressure.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("large.bin");
    let payload: Vec<u8> = (0..10_240u32).map(|i| (i * 7 % 256) as u8).collect();

    let mut sink = Sink::start(config("large", 2, 2, 1024)).expect("start");
    sink.submit(path.to_str().unwrap(), &payload, false).expect("submit");
    sink.flush().expect("flush");
    drop(sink);

    assert_eq!(std::fs::read(&path).expect("read"), payload);
}

#[test]
fn multiple_jobs_across_workers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut sink = Sink::start(config("multi-job", 3, 8, 4096)).expect("start");

    let mut expected = Vec::new();
    for i in 0..12u32 {
        let path = dir.path().join(format!("file-{i}.bin"));
        let payload = format!("payload number {i} — {}", "x".repeat(i as usize * 10)).into_bytes();
        sink.submit(path.to_str().unwrap(), &payload, false).expect("submit");
        expected.push((path, payload));
    }
    sink.flush().expect("flush");
    drop(sink);

    for (path, payload) in expected {
        assert_eq!(std::fs::read(&path).expect("read"), payload, "mismatch for {path:?}");
    }
}

#[test]
fn empty_payload_writes_empty_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.bin");

    let mut sink = Sink::start(config("empty", 1, 4, 4096)).expect("start");
    sink.submit(path.to_str().unwrap(), b"", false).expect("submit");
    sink.flush().expect("flush");
    drop(sink);

    let meta = std::fs::metadata(&path).expect("file should exist");
    assert_eq!(meta.len(), 0);
}

#[test]
fn fsync_path_writes_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("durable.bin");
    let payload = b"durable bytes via fsync";

    let mut sink = Sink::start(config("fsync", 1, 4, 4096)).expect("start");
    sink.submit(path.to_str().unwrap(), payload, true).expect("submit");
    sink.flush().expect("flush");
    drop(sink);

    assert_eq!(std::fs::read(&path).expect("read"), payload);
}

#[test]
fn no_temp_files_left_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("clean.bin");

    let mut sink = Sink::start(config("clean", 2, 4, 256)).expect("start");
    let payload: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    sink.submit(path.to_str().unwrap(), &payload, false).expect("submit");
    sink.flush().expect("flush");
    drop(sink);

    // Exactly one entry — the final file. No leftover ".*.tmp" dotfiles.
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["clean.bin".to_string()], "stray files: {entries:?}");
}
