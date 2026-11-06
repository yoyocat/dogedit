//! Memory probe for the Mem path. Generates a
//! ~50 MB log, then measures peak working set for:
//! 1. the raw load path (`Document::from_bytes` + lazy per-line
//! decode — the R1 model),
//! 2. the pre-R1 full-materialization equivalent (load + one
//! `to_text()`), for an apples-to-apples before/after.
//! Run: `cargo run --release -p dg-app --example mem_probe`

use std::sync::Arc;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};

fn peak_working_set_kib() -> u64 {
    unsafe {
        let mut pmc = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(
            HANDLE(GetCurrentProcess().0),
            &mut pmc,
            pmc.cb,
        );
        if ok.is_ok() {
            pmc.PeakWorkingSetSize as u64 / 1024
        } else {
            0
        }
    }
}

fn current_working_set_kib() -> u64 {
    unsafe {
        let mut pmc = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        let ok = GetProcessMemoryInfo(
            HANDLE(GetCurrentProcess().0),
            &mut pmc,
            pmc.cb,
        );
        if ok.is_ok() {
            pmc.WorkingSetSize as u64 / 1024
        } else {
            0
        }
    }
}

/// A deterministic ~120-byte line; `n` varies the id so lines differ.
fn line(n: usize) -> String {
    format!(
        "2026-10-25 12:00:{:02} INFO [main] request id={} path=/api/v1/items \
         status=200 took 42ms user=session-{} payload=order/shipped qty={} note=ok\n",
        n % 60,
        n,
        n % 9973,
        n % 17,
    )
}

fn generate(path: &std::path::Path, target_mb: usize) -> u64 {
    let one = line(0);
    let per_line = one.len() as u64;
    let lines = (target_mb as u64 * 1024 * 1024) / per_line;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    use std::io::Write;
    for n in 0..lines {
        let _ = f.write_all(line(n as usize).as_bytes());
    }
    let _ = f.flush();
    lines * per_line
}

fn main() {
    let dir = std::env::temp_dir().join("dg_mem_probe");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("probe_50mb.log");
    let size = generate(&path, 50);
    println!(
        "file: {} ({} MiB)   baseline ws: {} KiB, peak: {} KiB",
        path.display(),
        size / 1024 / 1024,
        current_working_set_kib(),
        peak_working_set_kib(),
    );

    // ---- 1. the R1 raw path ----
    let raw = Arc::new(std::fs::read(&path).unwrap());
    let enc = dg_core::encoding::detect_encoding(&raw).0;
    let doc = dg_core::Document::from_bytes(Arc::clone(&raw), enc, 0);
    let lines = doc.line_count();
    // open+paint: first screen's lines through the facade
    let mut s = String::new();
    for l in 0..50.min(lines) {
        doc.line_text(l, &mut s);
    }
    // scroll: sample lines across the whole document
    let step = (lines / 2000).max(1);
    for l in (0..lines).step_by(step) {
        doc.line_text(l, &mut s);
    }
    let total_bytes = doc.total_bytes();
    let peak_raw = peak_working_set_kib();
    // find-all timing on the same document (R3 line-domain scan)
    let t = std::time::Instant::now();
    let hits = dg_core::find::find_all(
        &doc,
        "status=200",
        dg_core::FindOptions::default(),
    )
    .len();
    let find_ms = t.elapsed().as_millis();
    let peak_find = peak_working_set_kib();
    drop(doc);
    drop(raw);
    println!(
        "raw path:   lines={lines} total_bytes={total_bytes}  peak ws: {peak_raw} KiB ({}x file)",
        peak_raw as f64 * 1024.0 / size as f64
    );
    println!(
        "find-all:   {hits} hits of 'status=200' in {find_ms} ms  (peak ws after: {peak_find} KiB, {}x file)",
        peak_find as f64 * 1024.0 / size as f64
    );

    // ---- 2. the pre-R1 materialization equivalent ----
    let raw = Arc::new(std::fs::read(&path).unwrap());
    let enc = dg_core::encoding::detect_encoding(&raw).0;
    let doc = dg_core::Document::from_bytes(Arc::clone(&raw), enc, 0);
    let text = doc.to_text();
    let peak_mat = peak_working_set_kib();
    println!(
        "full text:  bytes={}  peak ws: {peak_mat} KiB ({}x file)  [delta {} KiB]",
        text.len(),
        peak_mat as f64 * 1024.0 / size as f64,
        peak_mat - peak_raw,
    );
    drop(text);
    drop(doc);
    drop(raw);

    let _ = std::fs::remove_file(&path);
}
