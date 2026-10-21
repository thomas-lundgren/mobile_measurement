//! Layer direction (planning/app_workload_layering.md): traffic engines move
//! bytes and report what moved; workloads (`app_traffic`) decide what to move
//! and when; the server side does exactly what each request on the wire asks.
//! So no engine module, and no server process, may reach the workload layer.
//! Comments may name it; code may not. Tests may (they drive a UE).

use std::path::Path;

/// The engines and the measurement layer under them.
const ENGINES: &[&str] = &[
    "src/endpoint.rs",
    "src/exchange.rs",
    "src/burst.rs",
    "src/packet.rs",
    "src/udp_timing.rs",
    "src/log_writer.rs",
    "src/timestamp.rs",
];

/// Server processes that run engines for the UE's flows.
const SERVER_SRC: &[&str] = &["../server/data-generator/src", "../server/test-control/src"];

/// Lines of code (not comments) in `path` that name the workload layer.
fn offending(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let code = l.split("//").next().unwrap_or("");
            code.contains("app_traffic")
        })
        .map(|(i, l)| format!("{}:{}: {}", path.display(), i + 1, l.trim()))
        .collect()
}

fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(rust_files(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

#[test]
fn engines_and_servers_do_not_reach_the_workloads() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = ENGINES.iter().map(|f| root.join(f)).collect();
    for dir in SERVER_SRC {
        files.extend(rust_files(&root.join(dir)));
    }
    let bad: Vec<String> = files.iter().flat_map(|f| offending(f)).collect();
    assert!(
        bad.is_empty(),
        "the engine/server layer names the workload layer (app_traffic):\n{}",
        bad.join("\n")
    );
}

/// The check itself: a use of the workload layer is caught, a comment is not.
#[test]
fn the_check_sees_code_not_comments() {
    let dir = std::env::temp_dir().join(format!("layering_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("x.rs");
    std::fs::write(
        &f,
        "//! not app_traffic\nuse crate::app_traffic::Agent; // x\n",
    )
    .unwrap();
    assert_eq!(offending(&f).len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
