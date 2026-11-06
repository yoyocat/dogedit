//! M0/M1 基准：生成 N GB 日志，测 LargeDoc 打开/索引/行读取。
//! 用法: cargo run -p dg-large --release --example bench_open -- [gb]
use dg_large::LargeDoc;
use std::io::Write;
use std::time::Instant;

fn main() {
    let gb: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let path = std::env::temp_dir().join("evlarge-bench.log");
    let line = b"2026-10-25 08:30:12.345 INFO  [worker-3] processed batch id=98765 rows=1234 took=45ms status=ok\n";
    let target = (gb * 1024.0 * 1024.0 * 1024.0) as u64;
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) != target {
        eprintln!("生成 {gb} GB 测试文件…");
        let t = Instant::now();
        let mut f = std::fs::File::create(&path).unwrap();
        let mut chunk = Vec::with_capacity(line.len() * 8192);
        let mut written = 0u64;
        while written < target {
            chunk.extend_from_slice(line);
            if chunk.len() >= line.len() * 8192 || written + chunk.len() as u64 >= target {
                f.write_all(&chunk).unwrap();
                written += chunk.len() as u64;
                chunk.clear();
            }
        }
        drop(f);
        eprintln!("写入 {} MB，耗时 {:?}", target / 1048576, t.elapsed());
    }
    let t = Instant::now();
    let doc = LargeDoc::open(&path).unwrap();
    let open_t = t.elapsed();
    let immediate = doc.line_count();
    let t = Instant::now();
    let mut s = String::new();
    doc.line_text(0, &mut s);
    let first_line_t = t.elapsed();
    let t = Instant::now();
    let done = doc.wait_for_scan(std::time::Duration::from_secs(600));
    let scan_t = t.elapsed();
    let full = doc.line_count();
    // 尾部与随机行读取
    let t = Instant::now();
    let mut probe = String::new();
    for i in (0..full).step_by(full / 1000.max(1)) {
        doc.line_text(i, &mut probe);
    }
    let random_t = t.elapsed();
    eprintln!(
        "open={:?} (首屏+尾区 {} 行)  首行读取={:?}  全量索引={:?} (done={done})  总行数={}  1000 次随机行={:?}",
        open_t, immediate, first_line_t, scan_t, full, random_t
    );
    let _ = s;

    // M4 流式搜索：字面量 + 正则（先等行表，正则逐行路径）。
    use dg_large::{SearchEvent, SearchOptions};
    let bench_search = |name: &str, opts: SearchOptions, needle: &str| {
        let t = Instant::now();
        let h = doc.start_search(needle, opts).unwrap();
        let mut n = 0usize;
        let mut truncated = false;
        while let Some(ev) = h.recv_timeout(std::time::Duration::from_secs(600)) {
            match ev {
                SearchEvent::Matches(v) => n += v.len(),
                SearchEvent::Done { truncated: tr } => {
                    truncated = tr;
                    break;
                }
                _ => {}
            }
        }
        eprintln!(
            "search[{name}] {n} hits (truncated={truncated}) {:?}",
            t.elapsed()
        );
    };
    bench_search(
        "literal-cs",
        SearchOptions {
            case_sensitive: true,
            limit: 1 << 30,
            ..Default::default()
        },
        "id=98765",
    );
    bench_search(
        "literal",
        SearchOptions {
            case_sensitive: false,
            limit: 1 << 30, // 测吞吐：不设 10 万上限
            ..Default::default()
        },
        "ERROR",
    );
    bench_search(
        "regex",
        SearchOptions {
            regex: true,
            limit: 1 << 30,
            ..Default::default()
        },
        r"id=\d+",
    );

    let _ = std::fs::remove_file(&path);
}
