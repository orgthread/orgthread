//! A live check against the real Dropbox. It only reads, and it is ignored in a
//! normal run. On the server, with the Dropbox settings in the environment and
//! a test file that was written at least twice:
//! `ORGTHREAD_LIVE_FILE=<path from the root> live_dropbox --ignored --nocapture`

use orgthread::dropbox::Dropbox;
use orgthread::fingerprint::fingerprint;
use orgthread::storage::Storage;
use std::path::Path;

#[test]
#[ignore]
fn the_history_of_a_test_file_gives_its_versions_and_limit() {
    let file = std::env::var("ORGTHREAD_LIVE_FILE").expect("ORGTHREAD_LIVE_FILE");
    let dropbox = Dropbox::new(Path::new("live-check-locks")).expect("a Dropbox link");

    let versions = dropbox
        .revisions(&file, 100)
        .expect("the list of revisions");
    println!("list_revisions, limit 100: {} versions", versions.len());
    assert!(versions.len() >= 2, "write the test file twice first");
    assert!(versions.len() <= 100);

    let now = dropbox.read(&file).unwrap().expect("the test file");
    assert_eq!(
        versions[0].hash,
        fingerprint(&now),
        "the newest is the current text"
    );
    for v in versions.iter().take(2) {
        let bytes = dropbox
            .read_revision(&v.rev)
            .unwrap()
            .expect("a kept version");
        assert_eq!(fingerprint(&bytes), v.hash, "each version matches its hash");
        assert_eq!(bytes.len() as u64, v.size);
    }
    println!("download of rev:<rev>: 2 versions, each matches its fingerprint");

    let (status, text) = dropbox.probe_revisions(&file, 101).unwrap();
    println!("list_revisions, limit 101: {status} {text}");
    let (status, text) = dropbox.probe_revisions(&file, 100).unwrap();
    println!("list_revisions, limit 100: {status} {text}");
}
