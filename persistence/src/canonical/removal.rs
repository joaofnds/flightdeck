//! Removes a name from a canonical namespace, and the directories that leaves empty. Namespace
//! roots are permanent. Publications insert into whatever routing is left, so an emptied node
//! that stays reads as an empty leaf and harms nothing.
use super::insert::{NamespaceAccess, Node, full, read_node, root_locator, suffix, sync};
use super::tree::{CanonicalKey, Namespace, NodeLocator, ReadWork};
use crate::transaction::normal::prefix_of;
use collector::recording::private::PrivateDirectory;
use std::io;

/// The most levels a key can route through, one per nibble of the widest key.
const LEVELS_MAX: usize = 124;

/// Removes `key` and any node that leaves with no entries, making both durable. A name already
/// gone is done, since a removal repeats after a crash. Only a writer holding the namespace Busy
/// with no structural plan in flight calls this.
pub(crate) fn remove(
    root: &PrivateDirectory,
    namespace: Namespace,
    key: &CanonicalKey,
    work: &mut ReadWork,
) -> io::Result<()> {
    let mut path = Vec::with_capacity(8);
    let target = full(namespace, key)?;
    let mut current = root_locator(namespace)?;
    let mut directory = root.open_node(namespace.name(), work)?;
    for _level in 0..LEVELS_MAX {
        let Node::Branch(children) = read_node(&directory, &current, work)? else {
            match directory.remove_file(&key.file_name()) {
                Ok(()) => work.removals += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            sync(&directory, work)?;
            return remove_prune(path, current, work);
        };
        let Some(child) = children.iter().find(|child| prefix_of(child, &target)) else {
            return Ok(());
        };
        let next = directory.open_node(&suffix(&current, child)?, work)?;
        path.push((directory, current));
        directory = next;
        current = *child;
    }

    Err(io::Error::from(io::ErrorKind::InvalidData))
}

/// Removes emptied nodes from the leaf upward, stopping at the first that still holds an entry.
fn remove_prune(
    path: Vec<(PrivateDirectory, NodeLocator)>,
    leaf: NodeLocator,
    work: &mut ReadWork,
) -> io::Result<()> {
    let mut child = leaf;
    for (parent, locator) in path.into_iter().rev() {
        match parent.remove_directory(&suffix(&locator, &child)?) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        sync(&parent, work)?;
        child = locator;
    }

    Ok(())
}
