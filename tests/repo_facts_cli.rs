//! `klaar repo-facts` prints the same deterministic facts the library scan produces.

use std::process::Command;

#[test]
fn repo_facts_prints_stable_json_for_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/api.rs"), "pub fn charge() {}\n").unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "pub mod api;\n").unwrap();

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_klaar"))
            .args(["repo-facts", "--path"])
            .arg(dir.path())
            .output()
            .unwrap()
    };
    let (a, b) = (run(), run());
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    assert_eq!(a.stdout, b.stdout, "output is deterministic");
    let facts: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    assert_eq!(facts["schema_version"], 1);
    assert_eq!(facts["files"].as_array().unwrap().len(), 2);
    assert_eq!(facts["dependencies"].as_array().unwrap().len(), 1);
}
