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
fn task_history_cli_records_extract_and_show_reads_it() {
    let tmp = tempfile::tempdir().unwrap();
    let app_db = tmp.path().join("app.sqlite");
    let source = tmp.path().join("source");
    let archive = tmp.path().join("archive");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("image.jpg"), b"fake jpg").unwrap();

    let app_db_arg = app_db.to_string_lossy().to_string();
    let source_arg = source.to_string_lossy().to_string();
    let archive_arg = archive.to_string_lossy().to_string();

    let init = run_wechat_archiver(&["tasks", "init-db", "--app-db", &app_db_arg, "--json"]);
    assert!(
        init.status.success(),
        "init-db failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let extract = run_wechat_archiver(&[
        "extract",
        "--type",
        "image",
        "--source",
        &source_arg,
        "--archive",
        &archive_arg,
        "--app-db",
        &app_db_arg,
        "--dry-run",
        "--json",
    ]);
    assert!(
        extract.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&extract.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&extract.stdout).unwrap();
    assert_eq!(summary["dry_run"], true);
    assert_eq!(summary["scanned_files"], 1);
    assert_eq!(summary["candidates"], 1);
    assert_eq!(summary["would_archive"], 1);
    assert!(!archive.exists());

    let list = run_wechat_archiver(&["tasks", "list", "--app-db", &app_db_arg, "--json"]);
    assert!(
        list.status.success(),
        "tasks list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let records: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let records = records.as_array().unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    let task_id = record["task_id"].as_str().unwrap();
    assert_eq!(record["status"], "completed");
    assert_eq!(record["task_name"], "抽取图片");
    assert_eq!(record["task_kind"], "extract_images");
    assert_eq!(record["source_dir"], source_arg);
    assert_eq!(record["archive_dir"], archive_arg);
    assert_eq!(record["dry_run"], true);
    assert_eq!(record["progress"]["scanned_files"], 1);
    assert_eq!(record["progress"]["candidates"], 1);
    assert_eq!(record["params_summary_json"]["task_kind"], "extract_images");
    assert_eq!(record["params_summary_json"]["media_types"][0], "image");
    assert_eq!(
        record["params_summary_json"]["image_aes_key_provided"],
        false
    );
    assert_eq!(record["params_summary_json"]["image_xor_key"], 136);
    assert_eq!(record["result_summary"]["would_archive"], 1);

    let show = run_wechat_archiver(&["tasks", "show", "--app-db", &app_db_arg, task_id, "--json"]);
    assert!(
        show.status.success(),
        "tasks show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown["task_id"], task_id);
    assert_eq!(shown["status"], "completed");
    assert_eq!(shown["task_kind"], "extract_images");
    assert_eq!(shown["progress"]["scanned_files"], 1);
    assert_eq!(shown["progress"]["candidates"], 1);
    assert_eq!(shown["params_summary_json"], record["params_summary_json"]);
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
