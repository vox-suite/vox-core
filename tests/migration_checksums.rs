/**
* Integration tests verifying migration checksums match baseline fixtures.
*/
use sha2::{Digest, Sha384};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[test]
fn applied_migrations_are_byte_for_byte_immutable() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected = include_str!("fixtures/migration_checksums.sha384")
        .lines()
        .map(|line| {
            let (checksum, name) = line
                .split_once("  ")
                .expect("checksum fixture must separate hash and filename");
            (name.to_owned(), checksum.to_owned())
        })
        .collect::<BTreeMap<_, _>>();
    let actual = fs::read_dir(root.join("migrations"))
        .expect("read migrations")
        .map(|entry| entry.expect("read migration entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("migration filename")
                .to_string_lossy()
                .into_owned();
            let bytes = fs::read(path).expect("read migration");
            (name, hex::encode(Sha384::digest(bytes)))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
}
