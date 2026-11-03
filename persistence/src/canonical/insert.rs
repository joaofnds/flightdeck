//! Registered bounded structural insertion. Namespace roots are permanent routing directories.
use super::node::{Entry, Grammar, child_locator};
use super::tree::{CanonicalKey, Namespace, NodeLocator, ReadWork};
use crate::transaction::normal::{Structure, nibble, prefix_of};
use collector::recording::private::{DirectoryWork, PrivateDirectory};
use collector::ring::Identity;
use std::io;

pub(crate) enum Node {
    Branch(Vec<NodeLocator>),
    Leaf(Vec<CanonicalKey>),
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn root_locator(namespace: Namespace) -> io::Result<NodeLocator> {
    NodeLocator::from_key(
        namespace,
        CanonicalKey::from_bytes(namespace, &vec![0; namespace.width()])?,
        0,
    )
}
fn full(namespace: Namespace, key: &CanonicalKey) -> io::Result<NodeLocator> {
    NodeLocator::from_key(
        namespace,
        *key,
        u8::try_from(namespace.width() * 2).map_err(|_| invalid())?,
    )
}
fn suffix(parent: &NodeLocator, child: &NodeLocator) -> io::Result<String> {
    if !prefix_of(parent, child) || child.nibbles <= parent.nibbles {
        return Err(invalid());
    }
    let mut name = String::with_capacity(usize::from(child.nibbles - parent.nibbles));
    for index in usize::from(parent.nibbles)..usize::from(child.nibbles) {
        name.push(char::from(
            b"0123456789abcdef"[usize::from(nibble(&child.prefix, index))],
        ));
    }
    Ok(name)
}
fn read_node(
    directory: &PrivateDirectory,
    locator: &NodeLocator,
    work: &mut ReadWork,
) -> io::Result<Node> {
    let mut children = Vec::with_capacity(16);
    let mut keys = Vec::with_capacity(128);
    let mut native = DirectoryWork::default();
    let mut grammar = Grammar::new(locator);
    let result = directory.visit_names_counted(128, &mut native, |name| {
        match grammar.admit(name)? {
            Entry::Child(child) => children.push(child),
            Entry::Seal(key) => keys.push(key),
        }
        Ok(())
    });
    work.namespace_attempts += native.open_attempts;
    work.node_attempts += native.open_attempts;
    work.entries += native.names;
    work.native_entries += native.entries;
    work.directory_reads += native.read_calls;
    result?;
    keys.sort_unstable();
    if locator.nibbles == 0 || !children.is_empty() {
        Ok(Node::Branch(children))
    } else {
        Ok(Node::Leaf(keys))
    }
}
pub(crate) fn open_locator(
    root: &PrivateDirectory,
    locator: &NodeLocator,
    work: &mut ReadWork,
) -> io::Result<PrivateDirectory> {
    let mut directory = root.open_node(locator.namespace.name(), work)?;
    let mut current = root_locator(locator.namespace)?;
    for _ in 0..124 {
        if current == *locator {
            return Ok(directory);
        }
        let Node::Branch(children) = read_node(&directory, &current, work)? else {
            return Err(invalid());
        };
        let next = children
            .iter()
            .find(|child| prefix_of(child, locator))
            .ok_or_else(invalid)?;
        let name = suffix(&current, next)?;
        directory = directory.open_node(&name, work)?;
        current = *next;
    }
    Err(invalid())
}
struct Target {
    parent: NodeLocator,
    old: Option<NodeLocator>,
    replacement: NodeLocator,
    keys: Vec<CanonicalKey>,
    kind: u32,
}
fn locate(
    root: &PrivateDirectory,
    namespace: Namespace,
    key: &CanonicalKey,
    work: &mut ReadWork,
) -> io::Result<Target> {
    let insertion = full(namespace, key)?;
    let mut parent = root_locator(namespace)?;
    let mut current = parent;
    let mut directory = open_locator(root, &current, work)?;
    for _ in 0..124 {
        match read_node(&directory, &current, work)? {
            Node::Leaf(mut keys) => {
                if keys.contains(key) || keys.len() < 128 {
                    return Ok(Target {
                        parent,
                        old: Some(current),
                        replacement: current,
                        keys,
                        kind: 0,
                    });
                }
                keys.push(*key);
                let common = locate_common_prefix(namespace, &keys)?;
                return Ok(Target {
                    parent,
                    old: Some(current),
                    replacement: common,
                    keys,
                    kind: 1,
                });
            }
            Node::Branch(children) => {
                let digit = nibble(&key.bytes, usize::from(current.nibbles));
                let found = children
                    .iter()
                    .find(|child| nibble(&child.prefix, usize::from(current.nibbles)) == digit);
                let Some(child) = found else {
                    return Ok(Target {
                        parent: current,
                        old: None,
                        replacement: NodeLocator::from_key(namespace, *key, current.nibbles + 1)?,
                        keys: vec![*key],
                        kind: 2,
                    });
                };
                if !prefix_of(child, &insertion) {
                    let old_key =
                        CanonicalKey::from_bytes(namespace, &child.prefix[..namespace.width()])?;
                    return Ok(Target {
                        parent: current,
                        old: Some(*child),
                        replacement: locate_common_prefix(namespace, &[old_key, *key])?,
                        keys: vec![*key],
                        kind: 2,
                    });
                }
                let name = suffix(&current, child)?;
                parent = current;
                current = *child;
                directory = directory.open_node(&name, work)?;
            }
        }
    }
    Err(invalid())
}
fn locate_common_prefix(namespace: Namespace, keys: &[CanonicalKey]) -> io::Result<NodeLocator> {
    let first = keys.first().ok_or_else(invalid)?;
    let mut count = namespace.width() * 2;
    for key in keys {
        for index in 0..count {
            if nibble(&first.bytes, index) != nibble(&key.bytes, index) {
                count = index;
                break;
            }
        }
    }
    if count == 0 {
        return Err(invalid());
    }
    NodeLocator::from_key(
        namespace,
        *first,
        u8::try_from(count).map_err(|_| invalid())?,
    )
}
pub(crate) fn validate_roots(root: &PrivateDirectory, work: &mut ReadWork) -> io::Result<()> {
    for namespace in [
        Namespace::Ordinary,
        Namespace::Known,
        Namespace::Unknown,
        Namespace::Terminal,
        Namespace::Fragments,
    ] {
        let locator = root_locator(namespace)?;
        let directory = open_locator(root, &locator, work)?;
        let Node::Branch(children) = read_node(&directory, &locator, work)? else {
            return Err(invalid());
        };
        for child in children {
            drop(directory.open_node(&suffix(&locator, &child)?, work)?);
        }
    }
    Ok(())
}
/// With no structural intent, either durably links directly or returns a plan to persist first.
pub(crate) fn begin(
    root: &PrivateDirectory,
    source: &PrivateDirectory,
    namespace: Namespace,
    key: &CanonicalKey,
    serial: u64,
    work: &mut ReadWork,
) -> io::Result<Option<Structure>> {
    validate_roots(root, work)?;
    let target = locate(root, namespace, key, work)?;
    if target.kind == 0 {
        let directory = open_locator(root, &target.replacement, work)?;
        link_metadata(source, "metadata", &directory, &key.file_name(), work)?;
        sync(&directory, work)?;
        return Ok(None);
    }
    let (device, inode) = if let Some(old) = &target.old {
        open_locator(root, old, work)?.device_inode()?
    } else {
        (0, 0)
    };
    let plan = Structure {
        kind: target.kind,
        stage: 1,
        serial,
        parent: target.parent,
        old: target.old.unwrap_or(target.parent),
        replacement: target.replacement,
        insertion: full(namespace, key)?,
        device,
        inode,
        copied: if target.kind == 1 {
            u32::try_from(target.keys.len() - 1).map_err(|_| invalid())?
        } else {
            0
        },
        children: 0,
        cleanup: 0,
    };
    plan.validate()?;
    Ok(Some(plan))
}

pub(crate) fn verify(
    root: &PrivateDirectory,
    source: &PrivateDirectory,
    namespace: Namespace,
    key: &CanonicalKey,
    work: &mut ReadWork,
) -> io::Result<()> {
    let target = locate(root, namespace, key, work)?;
    if target.kind != 0 || !target.keys.contains(key) {
        return Err(invalid());
    }
    let directory = open_locator(root, &target.replacement, work)?;
    compare_link(&directory, &key.file_name(), source, "metadata", work)?;
    sync(&directory, work)
}
fn optional_directory(
    parent: &PrivateDirectory,
    name: &str,
    work: &mut ReadWork,
) -> io::Result<Option<PrivateDirectory>> {
    match parent.open_node(name, work) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn same_old(directory: &PrivateDirectory, plan: &Structure) -> io::Result<bool> {
    Ok(directory.device_inode()? == (plan.device, plan.inode))
}
fn names(transaction: Identity) -> (String, String) {
    let identity = collector::recording::identity_text(transaction);
    (
        format!("txn-{identity}-build"),
        format!("txn-{identity}-backup"),
    )
}
fn leaf_keys(
    directory: &PrivateDirectory,
    locator: &NodeLocator,
    work: &mut ReadWork,
) -> io::Result<Vec<CanonicalKey>> {
    let mut keys = Vec::with_capacity(128);
    let mut native = DirectoryWork::default();
    let mut grammar = Grammar::new(locator);
    let result = directory.visit_names_counted(128, &mut native, |name| {
        let Entry::Seal(key) = grammar.admit(name)? else {
            return Err(invalid());
        };
        keys.push(key);
        Ok(())
    });
    work.entries += native.names;
    work.native_entries += native.entries;
    work.namespace_attempts += native.open_attempts;
    work.node_attempts += native.open_attempts;
    work.directory_reads += native.read_calls;
    result?;
    keys.sort_unstable();
    Ok(keys)
}
fn insertion_key(plan: &Structure) -> io::Result<CanonicalKey> {
    CanonicalKey::from_bytes(
        plan.parent.namespace,
        &plan.insertion.prefix[..plan.parent.namespace.width()],
    )
}
fn split_child(plan: &Structure, key: &CanonicalKey) -> io::Result<NodeLocator> {
    NodeLocator::from_key(
        plan.parent.namespace,
        *key,
        plan.replacement
            .nibbles
            .checked_add(1)
            .ok_or_else(invalid)?,
    )
}
#[derive(Clone, Copy)]
struct LinkFact {
    key: CanonicalKey,
    identity: (u64, u64),
}
fn link_identity(
    directory: &PrivateDirectory,
    name: &str,
    work: &mut ReadWork,
) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    work.namespace_attempts += 1;
    work.metadata_admissions += 1;
    let stat = directory.open_file(name)?.metadata()?;
    Ok((stat.dev(), stat.ino()))
}
fn old_facts(
    directory: &PrivateDirectory,
    plan: &Structure,
    work: &mut ReadWork,
) -> io::Result<Vec<LinkFact>> {
    let keys = leaf_keys(directory, &plan.old, work)?;
    let mut facts = Vec::with_capacity(128);
    for key in keys {
        facts.push(LinkFact {
            key,
            identity: link_identity(directory, &key.file_name(), work)?,
        });
    }
    Ok(facts)
}
fn inspect_build(
    directory: &PrivateDirectory,
    plan: &Structure,
    facts: &[LinkFact],
    source: &PrivateDirectory,
    complete: bool,
    work: &mut ReadWork,
) -> io::Result<()> {
    let new = insertion_key(plan)?;
    let new_identity = link_identity(source, "metadata", work)?;
    let mut count = 0;
    let mut old_present = false;
    if plan.inode == 0 {
        for key in leaf_keys(directory, &plan.replacement, work)? {
            if key != new || link_identity(directory, &key.file_name(), work)? != new_identity {
                return Err(invalid());
            }
            count += 1;
        }
    } else {
        let children = match read_node(directory, &plan.replacement, work)? {
            Node::Branch(children) => children,
            Node::Leaf(keys) if keys.is_empty() && !complete => return Ok(()),
            Node::Leaf(_) => return Err(invalid()),
        };
        for child in children {
            let found = inspect_build_child(directory, &child, plan, facts, &new_identity, work)?;
            count += found.0;
            old_present |= found.1;
        }
    }
    let expected = if plan.kind == 1 { facts.len() + 1 } else { 1 };
    if complete
        && (count != expected
            || (plan.kind == 2 && plan.inode != 0 && plan.stage >= 4 && !old_present))
    {
        return Err(invalid());
    }
    Ok(())
}
fn inspect_build_child(
    directory: &PrivateDirectory,
    child: &NodeLocator,
    plan: &Structure,
    facts: &[LinkFact],
    new_identity: &(u64, u64),
    work: &mut ReadWork,
) -> io::Result<(usize, bool)> {
    let new = insertion_key(plan)?;
    let new_identity = *new_identity;
    let mut count = 0;
    let leaf = directory.open_node(&suffix(&plan.replacement, child)?, work)?;
    if plan.kind == 2 && *child == plan.old {
        if !same_old(&leaf, plan)? {
            return Err(invalid());
        }
        return Ok((0, true));
    }
    let expected = if plan.kind == 1 {
        split_child(
            plan,
            &CanonicalKey::from_bytes(child.namespace, &child.prefix[..child.namespace.width()])?,
        )?
    } else {
        split_child(plan, &new)?
    };
    if *child != expected {
        return Err(invalid());
    }
    for key in leaf_keys(&leaf, child, work)? {
        let identity = if key == new {
            new_identity
        } else {
            facts
                .iter()
                .find(|fact| fact.key == key)
                .ok_or_else(invalid)?
                .identity
        };
        if link_identity(&leaf, &key.file_name(), work)? != identity {
            return Err(invalid());
        }
        count += 1;
    }
    Ok((count, false))
}
fn compare_link(
    left: &PrivateDirectory,
    left_name: &str,
    right: &PrivateDirectory,
    right_name: &str,
    work: &mut ReadWork,
) -> io::Result<()> {
    let first = link_identity(left, left_name, work)?;
    let second = link_identity(right, right_name, work)?;
    if first != second {
        return Err(invalid());
    }
    Ok(())
}
fn link_metadata(
    source: &PrivateDirectory,
    name: &str,
    target: &PrivateDirectory,
    destination: &str,
    work: &mut ReadWork,
) -> io::Result<()> {
    work.links += 1;
    let mut attempts = 0;
    let result = source.link_to_counted(name, target, destination, &mut attempts);
    work.namespace_attempts += attempts;
    work.metadata_admissions += attempts;
    result
}
fn validate_parent(
    root: &PrivateDirectory,
    transaction: Identity,
    plan: &Structure,
    work: &mut ReadWork,
) -> io::Result<()> {
    let parent = open_locator(root, &plan.parent, work)?;
    let (build, backup) = names(transaction);
    let mut mask = 0_u32;
    let mut native = DirectoryWork::default();
    let result = parent.visit_names_counted(18, &mut native, |name| {
        if name != build && name != backup {
            let child = child_locator(&plan.parent, name)?;
            let bit = 1_u32 << nibble(&child.prefix, usize::from(plan.parent.nibbles));
            if mask & bit != 0 {
                return Err(invalid());
            }
            mask |= bit;
        }
        drop(parent.open_node(name, work)?);
        Ok(())
    });
    work.namespace_attempts += native.open_attempts;
    work.node_attempts += native.open_attempts;
    work.directory_reads += native.read_calls;
    work.entries += native.names;
    work.native_entries += native.entries;
    result
}
/// Executes one registered stage; the caller persists its result before yielding.
pub(crate) fn step(
    root: &PrivateDirectory,
    source: &PrivateDirectory,
    transaction: Identity,
    plan: &mut Structure,
    work: &mut ReadWork,
) -> io::Result<bool> {
    plan.validate()?;
    validate_parent(root, transaction, plan, work)?;
    match plan.stage {
        1 => step_prepare(root, source, transaction, plan, work)?,
        2 => step_move_old(root, source, transaction, plan, work)?,
        3 => step_install(root, source, transaction, plan, work)?,
        4 | 5 => return step_cleanup(root, transaction, plan, work),
        _ => return Err(invalid()),
    }
    Ok(false)
}
fn step_prepare(
    root: &PrivateDirectory,
    source: &PrivateDirectory,
    transaction: Identity,
    plan: &mut Structure,
    work: &mut ReadWork,
) -> io::Result<()> {
    let parent = open_locator(root, &plan.parent, work)?;
    if plan.inode == 0
        && optional_directory(&parent, &suffix(&plan.parent, &plan.replacement)?, work)?.is_some()
    {
        return Err(invalid());
    }

    let (build_name, backup_name) = names(transaction);
    if optional_directory(&parent, &backup_name, work)?.is_some() {
        return Err(invalid());
    }
    let facts = step_prepare_facts(&parent, plan, work)?;
    let retained = optional_directory(&parent, &build_name, work)?;
    drop(parent);
    if let Some(build) = retained.as_ref() {
        inspect_build(build, plan, &facts, source, false, work)?;
    }
    drop(retained);
    let parent = open_locator(root, &plan.parent, work)?;
    work.creates += 1;
    work.namespace_attempts += 1;
    work.node_attempts += 1;
    let build = parent.create_directory_counted(&build_name, &mut work.full_syncs)?;
    sync(&build, work)?;
    sync(&parent, work)?;
    let key = insertion_key(plan)?;
    if plan.inode == 0 {
        drop(parent);
        link_metadata(source, "metadata", &build, &key.file_name(), work)?;
    } else {
        let old = if plan.kind == 1 {
            let old = parent.open_node(&suffix(&plan.parent, &plan.old)?, work)?;
            if !same_old(&old, plan)? {
                return Err(invalid());
            }
            Some(old)
        } else {
            None
        };
        drop(parent);
        step_prepare_links(&build, source, plan, &facts, old.as_ref(), work)?;
    }
    inspect_build(&build, plan, &facts, source, true, work)?;
    sync(&build, work)?;
    let parent = open_locator(root, &plan.parent, work)?;
    sync(&parent, work)?;
    plan.children = step_prepare_mask(plan, &key, &facts);
    plan.stage = 2;
    Ok(())
}
fn step_prepare_mask(plan: &Structure, key: &CanonicalKey, facts: &[LinkFact]) -> u32 {
    if plan.inode == 0 {
        0
    } else {
        let mut mask = 1_u32 << nibble(&key.bytes, usize::from(plan.replacement.nibbles));
        if plan.kind == 1 {
            for fact in facts {
                mask |= 1_u32 << nibble(&fact.key.bytes, usize::from(plan.replacement.nibbles));
            }
        } else {
            mask |= 1_u32 << nibble(&plan.old.prefix, usize::from(plan.replacement.nibbles));
        }
        mask
    }
}
fn step_prepare_facts(
    parent: &PrivateDirectory,
    plan: &Structure,
    work: &mut ReadWork,
) -> io::Result<Vec<LinkFact>> {
    let old = if plan.inode != 0 {
        let old = parent.open_node(&suffix(&plan.parent, &plan.old)?, work)?;
        if !same_old(&old, plan)? {
            return Err(invalid());
        }
        Some(old)
    } else {
        None
    };
    let facts = if plan.kind == 1 {
        old_facts(old.as_ref().ok_or_else(invalid)?, plan, work)?
    } else {
        Vec::new()
    };
    if plan.kind == 1 && facts.len() != usize::try_from(plan.copied).map_err(|_| invalid())? {
        return Err(invalid());
    }
    // Drop admission framing before enumerating a retained build: the enumeration and
    // one leaf admission must fit alongside the Store, raw source and caller root.
    drop(old);
    Ok(facts)
}
fn step_prepare_links(
    build: &PrivateDirectory,
    source: &PrivateDirectory,
    plan: &Structure,
    facts: &[LinkFact],
    old: Option<&PrivateDirectory>,
    work: &mut ReadWork,
) -> io::Result<()> {
    let key = insertion_key(plan)?;
    let depth = usize::from(plan.replacement.nibbles);
    for partition in 0..16 {
        let includes_new = nibble(&key.bytes, depth) == partition;
        let first = facts
            .iter()
            .find(|fact| nibble(&fact.key.bytes, depth) == partition);
        let representative = match first {
            Some(fact) => fact.key,
            None if includes_new => key,
            None => continue,
        };
        let child = split_child(plan, &representative)?;
        work.creates += 1;
        work.namespace_attempts += 1;
        work.node_attempts += 1;
        let leaf = build
            .create_directory_counted(&suffix(&plan.replacement, &child)?, &mut work.full_syncs)?;
        for fact in facts
            .iter()
            .filter(|fact| nibble(&fact.key.bytes, depth) == partition)
        {
            let old = old.ok_or_else(invalid)?;
            if link_identity(old, &fact.key.file_name(), work)? != fact.identity {
                return Err(invalid());
            }
            link_metadata(
                old,
                &fact.key.file_name(),
                &leaf,
                &fact.key.file_name(),
                work,
            )?;
        }
        if includes_new {
            link_metadata(source, "metadata", &leaf, &key.file_name(), work)?;
        }
        // Restore both newly created and retained child durability once per partition.
        sync(&leaf, work)?;
    }
    Ok(())
}
fn step_move_old(
    root: &PrivateDirectory,
    source: &PrivateDirectory,
    transaction: Identity,
    plan: &mut Structure,
    work: &mut ReadWork,
) -> io::Result<()> {
    let parent = open_locator(root, &plan.parent, work)?;
    let (build, backup) = names(transaction);
    let prepared = parent.open_node(&build, work)?;
    let old = if plan.inode != 0 {
        match optional_directory(&parent, &suffix(&plan.parent, &plan.old)?, work)? {
            Some(old) => Some(old),
            None => optional_directory(&parent, &backup, work)?,
        }
    } else {
        None
    };
    let facts = if plan.kind == 1 {
        old_facts(old.as_ref().ok_or_else(invalid)?, plan, work)?
    } else {
        Vec::new()
    };
    drop(old);
    drop(parent);
    inspect_build(&prepared, plan, &facts, source, true, work)?;
    drop(prepared);
    let parent = open_locator(root, &plan.parent, work)?;
    if plan.inode != 0 {
        let name = suffix(&plan.parent, &plan.old)?;
        let old = optional_directory(&parent, &name, work)?;
        let saved = optional_directory(&parent, &backup, work)?;
        match (old, saved) {
            (Some(old), None) if same_old(&old, plan)? => {
                drop(old);
                work.renames += 1;
                parent.rename(&name, &backup)?;
            }
            (None, Some(saved)) if same_old(&saved, plan)? => {}
            _ => return Err(invalid()),
        }
    } else if optional_directory(&parent, &suffix(&plan.parent, &plan.replacement)?, work)?
        .is_some()
    {
        return Err(invalid());
    }
    sync(&parent, work)?;
    plan.stage = 3;
    Ok(())
}
fn step_install(
    root: &PrivateDirectory,
    source: &PrivateDirectory,
    transaction: Identity,
    plan: &mut Structure,
    work: &mut ReadWork,
) -> io::Result<()> {
    let parent = open_locator(root, &plan.parent, work)?;
    let (build_name, backup_name) = names(transaction);
    let destination = suffix(&plan.parent, &plan.replacement)?;
    let build = optional_directory(&parent, &build_name, work)?;
    let installed = optional_directory(&parent, &destination, work)?;
    if build.is_some() && installed.is_some() {
        return Err(invalid());
    }
    let directory = build.as_ref().or(installed.as_ref()).ok_or_else(invalid)?;
    if plan.kind == 2 && plan.inode != 0 {
        inspect_build(directory, plan, &[], source, true, work)?;
        let old_name = suffix(&plan.replacement, &plan.old)?;
        let nested = optional_directory(directory, &old_name, work)?;
        let backup = optional_directory(&parent, &backup_name, work)?;
        match (nested, backup) {
            (None, Some(old)) if same_old(&old, plan)? => {
                drop(old);
                work.renames += 1;
                parent.rename_to(&backup_name, directory, &old_name)?;
            }
            (Some(old), None) if same_old(&old, plan)? => {}
            _ => return Err(invalid()),
        }
        sync(directory, work)?;
        sync(&parent, work)?;
    }
    let facts = if plan.kind == 1 {
        let backup = parent.open_node(&backup_name, work)?;
        if !same_old(&backup, plan)? {
            return Err(invalid());
        }
        old_facts(&backup, plan, work)?
    } else {
        Vec::new()
    };
    inspect_build(directory, plan, &facts, source, true, work)?;
    // All inserted metadata must still match the immutable ready inode.
    let key = insertion_key(plan)?;
    if plan.inode == 0 {
        compare_link(directory, &key.file_name(), source, "metadata", work)?;
    }
    if build.is_some() {
        work.renames += 1;
        parent.rename(&build_name, &destination)?;
    }
    sync(directory, work)?;
    sync(&parent, work)?;
    plan.stage = 4;
    Ok(())
}
fn step_cleanup(
    root: &PrivateDirectory,
    transaction: Identity,
    plan: &mut Structure,
    work: &mut ReadWork,
) -> io::Result<bool> {
    let parent = open_locator(root, &plan.parent, work)?;
    let (_, backup_name) = names(transaction);
    if plan.kind == 1 {
        if let Some(backup) = optional_directory(&parent, &backup_name, work)? {
            if !same_old(&backup, plan)? {
                return Err(invalid());
            }
            let keys = leaf_keys(&backup, &plan.old, work)?;
            let installed = parent.open_node(&suffix(&plan.parent, &plan.replacement)?, work)?;
            cleanup_prove_links(&backup, &installed, plan, &keys, work)?;
            sync(&installed, work)?;
            sync(&parent, work)?;
            for key in &keys {
                work.removals += 1;
                backup.remove_file(&key.file_name())?;
            }
            sync(&backup, work)?;
            drop(backup);
            if plan.stage == 4 {
                plan.stage = 5;
                plan.cleanup = 128;
                return Ok(false);
            }
            work.removals += 1;
            parent.remove_directory(&backup_name)?;
        }
    } else if optional_directory(&parent, &backup_name, work)?.is_some() {
        return Err(invalid());
    }
    sync(&parent, work)?;
    Ok(true)
}

fn cleanup_prove_links(
    backup: &PrivateDirectory,
    installed: &PrivateDirectory,
    plan: &Structure,
    keys: &[CanonicalKey],
    work: &mut ReadWork,
) -> io::Result<()> {
    let depth = usize::from(plan.replacement.nibbles);
    for partition in 0..16 {
        let mut matching = keys
            .iter()
            .filter(|key| nibble(&key.bytes, depth) == partition);
        let Some(first) = matching.next() else {
            continue;
        };
        let child = split_child(plan, first)?;
        let leaf = installed.open_node(&suffix(&plan.replacement, &child)?, work)?;
        for key in std::iter::once(first).chain(matching) {
            compare_link(backup, &key.file_name(), &leaf, &key.file_name(), work)?;
        }
        sync(&leaf, work)?;
    }
    Ok(())
}

fn sync(directory: &PrivateDirectory, work: &mut ReadWork) -> io::Result<()> {
    work.full_syncs += 1;
    directory.full_sync()
}

trait NamespaceAccess {
    fn open_node(&self, name: &str, work: &mut ReadWork) -> io::Result<PrivateDirectory>;
}
impl NamespaceAccess for PrivateDirectory {
    fn open_node(&self, name: &str, work: &mut ReadWork) -> io::Result<PrivateDirectory> {
        work.namespace_attempts += 1;
        work.node_attempts += 1;
        self.open_directory(name)
    }
}
