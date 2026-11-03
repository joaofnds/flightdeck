use super::*;
use crate::canonical::insert;

#[test]
fn first_child_counts_fresh_and_adopted_directory_barriers() {
    // Goal: insertion charges mkdir's internal barriers as well as its own explicit syncs.
    // Method: prepare a fresh child, then repeat the retained stage and compare actual work.
    let fixture = Fixture::new();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let source = root.create_directory("insertion-source").unwrap();
    source.create_file("metadata").unwrap();
    let mut work = ReadWork::default();
    let mut plan = insert::begin(
        &root,
        &source,
        Namespace::Ordinary,
        &insertion_key(0x10, 1),
        1,
        &mut work,
    )
    .unwrap()
    .unwrap();
    let retained = plan.clone();
    let before = work.full_syncs;
    assert!(!insert::step(&root, &source, Identity([99, 100]), &mut plan, &mut work).unwrap());
    assert_eq!(work.full_syncs - before, 6);
    let before = work.full_syncs;
    let mut replay = retained;
    assert!(!insert::step(&root, &source, Identity([99, 100]), &mut replay, &mut work).unwrap());
    assert_eq!(work.full_syncs - before, 4);
    assert_eq!(replay, plan);
}

#[test]
fn absent_child_refuses_a_conflicting_destination_before_creating_build() {
    // Goal: a retained absent-child plan refuses contradictions before making scratch.
    // Method: create its destination after planning and compare the namespace after refusal.
    let fixture = Fixture::new();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let source = root.create_directory("insertion-source").unwrap();
    source.create_file("metadata").unwrap();
    let mut work = ReadWork::default();
    let mut plan = insert::begin(
        &root,
        &source,
        Namespace::Ordinary,
        &insertion_key(0x10, 1),
        1,
        &mut work,
    )
    .unwrap()
    .unwrap();
    let ordinary = root.open_directory("ordinary").unwrap();
    let destination = ordinary
        .create_directory("1")
        .unwrap()
        .device_inode()
        .unwrap();

    assert_eq!(
        insert::step(&root, &source, Identity([99, 100]), &mut plan, &mut work)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    assert_eq!(
        ordinary
            .open_directory("1")
            .unwrap()
            .device_inode()
            .unwrap(),
        destination
    );
    assert_eq!(
        ordinary
            .open_directory("txn-00000000000000630000000000000064-build")
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(plan.stage, 1);
}

#[test]
fn wrapper_refuses_foreign_build_before_moving_backup() {
    // Goal: a contradictory prepared wrapper cannot move its retained old subtree.
    // Method: add an unexpected child after preparation and verify the backup inode survives.
    let fixture = Fixture::new();
    for number in 1..=129 {
        insert_key(&fixture, &insertion_key(0x11, number), false);
    }
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let source = root.open_directory("insertion-source").unwrap();
    let mut work = ReadWork::default();
    let mut plan = insert::begin(
        &root,
        &source,
        Namespace::Ordinary,
        &insertion_key(0x12, 1),
        1,
        &mut work,
    )
    .unwrap()
    .unwrap();
    assert_eq!(plan.kind, 2);
    for _ in 0..2 {
        assert!(!insert::step(&root, &source, Identity([99, 100]), &mut plan, &mut work).unwrap());
    }
    assert_eq!(plan.stage, 3);
    let parent = insert::open_locator(&root, &plan.parent, &mut work).unwrap();
    let backup_name = "txn-00000000000000630000000000000064-backup";
    let backup = parent
        .open_directory(backup_name)
        .unwrap()
        .device_inode()
        .unwrap();
    parent
        .open_directory("txn-00000000000000630000000000000064-build")
        .unwrap()
        .create_directory("f")
        .unwrap();

    assert_eq!(
        insert::step(&root, &source, Identity([99, 100]), &mut plan, &mut work)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    assert_eq!(
        parent
            .open_directory(backup_name)
            .unwrap()
            .device_inode()
            .unwrap(),
        backup
    );
    assert_eq!(plan.stage, 3);
}

fn insert_key(fixture: &Fixture, key: &CanonicalKey, replay: bool) -> ReadWork {
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let source = root.create_directory("insertion-source").unwrap();
    match source.open_file("metadata") {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            source
                .create_file("metadata")
                .unwrap()
                .write_all(b"immutable")
                .unwrap();
        }
        Err(error) => panic!("{error}"),
    }
    let mut work = ReadWork::default();
    if let Some(mut plan) =
        insert::begin(&root, &source, Namespace::Ordinary, key, 1, &mut work).unwrap()
    {
        let mut finished = false;
        for _ in 0..10 {
            let before = plan.clone();
            finished =
                insert::step(&root, &source, Identity([99, 100]), &mut plan, &mut work).unwrap();
            if replay {
                // A successful effect with a lost slot update must be adoptable from old intent.
                let mut previous = before.clone();
                let adopted = insert::step(
                    &root,
                    &source,
                    Identity([99, 100]),
                    &mut previous,
                    &mut work,
                )
                .unwrap();
                if before.stage < 4 {
                    assert_eq!(adopted, finished);
                    assert_eq!(previous, plan);
                } else if adopted {
                    finished = true;
                }
            }
            if finished {
                break;
            }
        }
        assert!(finished);
    }
    work
}
fn insertion_key(first: u8, tail: u64) -> CanonicalKey {
    let mut bytes = [0; 40];
    bytes[0] = first;
    bytes[32..].copy_from_slice(&tail.to_be_bytes());
    CanonicalKey::from_bytes(Namespace::Ordinary, &bytes).unwrap()
}
fn insertion_root_identity(fixture: &Fixture) -> (u64, u64) {
    PrivateDirectory::open(&fixture.root)
        .unwrap()
        .open_directory("ordinary")
        .unwrap()
        .device_inode()
        .unwrap()
}
fn insertion_contains(fixture: &Fixture, key: CanonicalKey) {
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let mut state = [0; 256];
    let memory = budget();
    let mut cursor = CanonicalCursor::begin(
        &root,
        fixture.run.recording,
        Namespace::Ordinary,
        SeekRange {
            first: key,
            last: key,
            direction: Direction::Forward,
        },
        &mut state,
        &memory,
    )
    .unwrap();
    for _ in 0..128 {
        match cursor.step().unwrap() {
            ReadStep::Found(found) => {
                assert_eq!(found, key);
                return;
            }
            ReadStep::Continue => {}
            ReadStep::End => panic!("missing insertion"),
        }
    }
    panic!("lookup continuation bound");
}
#[test]
fn permanent_root_first_child_all_nibbles_and_reopen() {
    // Goal: first-child and all sixteen root routes preserve the bootstrap inode.
    // Method: insert with repeated structural effects and reopen the actual reader for each key.
    let fixture = Fixture::new();
    let identity = insertion_root_identity(&fixture);
    for digit in 0..16 {
        let key = insertion_key(digit << 4, 1);
        insert_key(&fixture, &key, true);
        insertion_contains(&fixture, key);
        assert_eq!(insertion_root_identity(&fixture), identity);
    }
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let ordinary = root.open_directory("ordinary").unwrap();
    let mut count = 0;
    ordinary
        .visit_names(16, |name| {
            assert_eq!(name.len(), 1);
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 16);
}
#[test]
fn full_child_split_long_odd_prefix_wrapper_and_duplicate_retry() {
    // Goal: split 128 links without replacing the root and wrap a compressed odd prefix.
    // Method: populate one leaf, replay each stage of insertion129, then insert outside its prefix.
    let fixture = Fixture::new();
    let identity = insertion_root_identity(&fixture);
    for number in 1..=128 {
        insert_key(&fixture, &insertion_key(0x11, number), false);
    }
    let extra = insertion_key(0x11, 129);
    insert_key(&fixture, &extra, true);
    insertion_contains(&fixture, extra);
    for number in [1, 64, 128] {
        insertion_contains(&fixture, insertion_key(0x11, number));
    }
    let wrapper = insertion_key(0x12, 1);
    insert_key(&fixture, &wrapper, true);
    insertion_contains(&fixture, wrapper);
    insertion_contains(&fixture, extra);
    insert_key(&fixture, &extra, true);
    assert_eq!(insertion_root_identity(&fixture), identity);
}
#[test]
fn malformed_root_and_root_replacement_descriptor_refuse_without_effects() {
    // Goal: EXEC08 forbids root seals and root-replacement intent before insertion mutation.
    // Method: retain malformed structure and verify neither registered scratch name appears.
    let fixture = Fixture::new();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let ordinary = root.open_directory("ordinary").unwrap();
    let key = insertion_key(0x10, 1);
    ordinary.create_file(&key.file_name()).unwrap();
    let source = root.create_directory("insertion-source").unwrap();
    source.create_file("metadata").unwrap();
    assert_eq!(
        insert::begin(
            &root,
            &source,
            Namespace::Ordinary,
            &key,
            1,
            &mut ReadWork::default()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
    let zero = NodeLocator::from_key(Namespace::Ordinary, key, 0).unwrap();
    let mut plan = crate::transaction::normal::Structure {
        kind: 1,
        stage: 1,
        serial: 1,
        parent: zero,
        old: zero,
        replacement: NodeLocator::from_key(Namespace::Ordinary, key, 1).unwrap(),
        insertion: NodeLocator::from_key(Namespace::Ordinary, key, 80).unwrap(),
        device: 1,
        inode: 1,
        copied: 128,
        children: 0,
        cleanup: 0,
    };
    assert_eq!(
        insert::step(
            &root,
            &source,
            Identity([99, 100]),
            &mut plan,
            &mut ReadWork::default()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
    let mut names = 0;
    ordinary
        .visit_names(1, |_| {
            names += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(names, 1);
}

#[test]
fn retained_old_inode_conflict_cannot_authorize_a_split_build() {
    // Goal: a replaced immediate child is not adopted from a matching name or prefix.
    // Method: retain128 links, register its split, replace only the old directory and step.
    let fixture = Fixture::new();
    for number in 1..=128 {
        insert_key(&fixture, &insertion_key(0x11, number), false);
    }
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let source = root.open_directory("insertion-source").unwrap();
    let key = insertion_key(0x11, 129);
    let mut plan = insert::begin(
        &root,
        &source,
        Namespace::Ordinary,
        &key,
        1,
        &mut ReadWork::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(plan.kind, 1);
    let ordinary = root.open_directory("ordinary").unwrap();
    fs::rename(
        fixture.root.join("ordinary/1"),
        fixture.root.join("retained-old"),
    )
    .unwrap();
    let replacement = ordinary.create_directory("1").unwrap();
    let retained = root.open_directory("retained-old").unwrap();
    for number in 1..=128 {
        let name = insertion_key(0x11, number).file_name();
        retained.link_to(&name, &replacement, &name).unwrap();
    }
    assert_eq!(
        insert::step(
            &root,
            &source,
            Identity([99, 100]),
            &mut plan,
            &mut ReadWork::default()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        ordinary
            .open_directory("txn-00000000000000630000000000000064-build")
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(root.open_directory("retained-old").is_ok());
}
