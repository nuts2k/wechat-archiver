use std::process::{Command, Output};

fn run_wechat_archiver(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wechat-archiver"))
        .args(args)
        .output()
        .expect("run wechat-archiver")
}

#[test]
fn tasks_init_db_cli_creates_db_and_list_reads_json() {
    let tmp = tempfile::tempdir().unwrap();
    let app_db = tmp.path().join("app.sqlite");
    let app_db_arg = app_db.to_string_lossy().to_string();

    let init = run_wechat_archiver(&["tasks", "init-db", "--app-db", &app_db_arg, "--json"]);
    assert!(
        init.status.success(),
        "init-db failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(app_db.is_file());

    let init_json: serde_json::Value = serde_json::from_slice(&init.stdout).unwrap();
    assert_eq!(init_json["app_db"], app_db_arg);
    assert_eq!(init_json["created"], true);

    let list = run_wechat_archiver(&["tasks", "list", "--app-db", &app_db_arg, "--json"]);
    assert!(
        list.status.success(),
        "tasks list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let records: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 0);
}

#[test]
fn tasks_init_db_cli_rejects_existing_file_without_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let app_db = tmp.path().join("app.sqlite");
    std::fs::write(&app_db, b"existing").unwrap();
    let app_db_arg = app_db.to_string_lossy().to_string();

    let init = run_wechat_archiver(&["tasks", "init-db", "--app-db", &app_db_arg, "--json"]);

    assert!(!init.status.success());
    assert_eq!(std::fs::read(&app_db).unwrap(), b"existing");
    assert!(String::from_utf8_lossy(&init.stderr).contains("app db already exists"));
}

#[test]
fn tasks_init_db_cli_rejects_missing_parent_without_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("missing-parent");
    let app_db = parent.join("app.sqlite");
    let app_db_arg = app_db.to_string_lossy().to_string();

    let init = run_wechat_archiver(&["tasks", "init-db", "--app-db", &app_db_arg, "--json"]);

    assert!(!init.status.success());
    assert!(!parent.exists());
    assert!(!app_db.exists());
    assert!(String::from_utf8_lossy(&init.stderr).contains("parent directory"));
}
