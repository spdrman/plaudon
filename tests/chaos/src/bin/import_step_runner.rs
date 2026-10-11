//! Performs a real import up to a chosen durable step, then aborts the
//! process without unwinding, cleanup or flushing.
//!
//! Usage: `import-step-runner <db-path> <archive-root> <step> <content>`

use std::io::Write;

use plaudon_chaos_tests::ImportStep;
use plaudon_domain::media::QualityClass;
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::db::Database;
use plaudon_persistence::journal::ImportJournal;
use plaudon_persistence::migrations::MigrationRunner;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(
        args.len(),
        5,
        "usage: import-step-runner <db> <archive> <step> <content>"
    );
    let step = ImportStep::parse(&args[3]).expect("unknown step");
    let content = args[4].clone().into_bytes();

    let db = Database::open(&args[1]).await.expect("open database");
    MigrationRunner::new().run(&db).await.expect("migrate");
    let archive = MediaArchive::open(&args[2]).expect("open archive");
    let journal = ImportJournal::new(db, archive);

    if step == ImportStep::BeforeStaging {
        crash();
    }

    let staged = journal
        .stage(&mut std::io::Cursor::new(content), "wav")
        .expect("stage");
    println!("{}", staged.intent_id);
    println!("{}", staged.path.display());
    std::io::stdout().flush().expect("flush");
    if step == ImportStep::AfterStaging {
        crash();
    }

    let intent = journal
        .record_intent(
            &staged,
            "wav",
            QualityClass::UserSuppliedFile,
            None,
            None,
            Timestamp::from_millis(1),
        )
        .await
        .expect("record intent");
    if step == ImportStep::AfterIntent {
        crash();
    }

    journal
        .rename_into_archive(&intent, Timestamp::from_millis(2))
        .await
        .expect("rename");
    if step == ImportStep::AfterRename {
        crash();
    }

    journal
        .commit(&intent, Timestamp::from_millis(3))
        .await
        .expect("commit");
    crash();
}

/// Terminate immediately: no destructors, no buffered writes, no unwinding.
fn crash() -> ! {
    std::process::abort();
}
