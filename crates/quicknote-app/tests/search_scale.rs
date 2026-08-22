//! Issue #19 的 10,000 张 / 100 MiB 搜索规模验收。

use quicknote_app::{Application, ApplicationConfig, NoteLifecycle};
use rusqlite::{Connection, TransactionBehavior, params};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

const NOTE_COUNT: usize = 10_000;
const TOTAL_BODY_BYTES: usize = 100 * 1024 * 1024;
const SEARCH_RUNS: usize = 20;
const SEARCH_P95_BUDGET: Duration = Duration::from_millis(200);

#[test]
#[ignore = "规模验收会创建 100 MiB 临时数据库，按发布门槛显式运行"]
fn searches_ten_thousand_notes_and_one_hundred_mib_without_truncation() {
    let directory = TempDir::new().expect("创建规模测试目录");
    // 先让生产迁移创建 schema；大规模数据只作为验收夹具直接批量写入。
    drop(Application::open(ApplicationConfig::new(directory.path())).expect("创建生产 schema"));
    seed_scale_fixture(directory.path().join("quicknote.db"));

    let app = Application::open(ApplicationConfig::new(directory.path())).expect("打开规模数据集");
    let mut samples = Vec::with_capacity(SEARCH_RUNS);
    for _ in 0..SEARCH_RUNS {
        let started = Instant::now();
        let english = app.search("nEeDlE-09998").expect("英文正文子串搜索");
        samples.push(started.elapsed());
        assert_eq!(english.len(), 1);
        assert_eq!(english[0].lifecycle, NoteLifecycle::Archived);
        assert!(english[0].matched_in_body);
    }
    assert_eq!(app.search("中文针脚").expect("中文搜索").len(), 1);
    assert!(
        app.search("trashed-only")
            .expect("搜索排除回收站")
            .is_empty()
    );
    samples.sort_unstable();
    // 采用 nearest-rank P95；20 个样本时第 19 个有序样本就是 P95。
    let p95_index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
    let p95 = samples[p95_index];
    eprintln!(
        "10,000 张 / 100 MiB 英文正文子串搜索 P95：{:.3} ms；样本：{samples:?}",
        p95.as_secs_f64() * 1_000.0
    );
    assert!(
        p95 <= SEARCH_P95_BUDGET,
        "搜索 P95 {} ms 超过 {} ms 发布门槛",
        p95.as_millis(),
        SEARCH_P95_BUDGET.as_millis()
    );
}

fn seed_scale_fixture(database_path: std::path::PathBuf) {
    let mut connection = Connection::open(database_path).expect("打开规模测试数据库");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("开始规模夹具事务");
    let base_bytes = TOTAL_BODY_BYTES / NOTE_COUNT;
    let extra_bytes = TOTAL_BODY_BYTES % NOTE_COUNT;
    let mut first_active = None;

    for index in 0..NOTE_COUNT {
        let id = Uuid::now_v7();
        let target_bytes = base_bytes + usize::from(index < extra_bytes);
        let marker = if index == 42 {
            format!("中文针脚 needle-{index:05} ")
        } else if index == NOTE_COUNT - 1 {
            "trashed-only ".to_owned()
        } else {
            format!("needle-{index:05} ")
        };
        let mut body = marker;
        body.push_str(&"x".repeat(target_bytes.saturating_sub(body.len())));
        let (lifecycle, archived_at_ms, trashed_at_ms) = if index == NOTE_COUNT - 1 {
            ("trashed", Some(1_i64), Some(i64::MAX))
        } else if index >= NOTE_COUNT / 2 {
            ("archived", Some(1_i64), None)
        } else {
            ("active", None, None)
        };
        transaction
            .execute(
                "INSERT INTO notes(
                    id, body, derived_title, content_revision, lifecycle,
                    created_at_ms, updated_at_ms, archived_at_ms, trashed_at_ms, due_at_ms
                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5, ?5, ?6, ?7, NULL)",
                params![
                    id.as_bytes().as_slice(),
                    body,
                    format!("规模便签 {index:05}"),
                    lifecycle,
                    index as i64 + 1,
                    archived_at_ms,
                    trashed_at_ms,
                ],
            )
            .expect("插入规模便签");
        if first_active.is_none() && lifecycle == "active" {
            first_active = Some(id);
        }
    }
    transaction
        .execute(
            "INSERT INTO current_note(singleton, note_id) VALUES (1, ?1)",
            params![
                first_active
                    .expect("至少一张活跃便签")
                    .as_bytes()
                    .as_slice()
            ],
        )
        .expect("设置规模夹具当前便签");
    transaction.commit().expect("提交规模夹具");
}
