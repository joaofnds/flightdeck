//! Stable canonical node grammar, shared by traversal and structural insertion.
use super::tree::{CanonicalKey, NodeLocator, invalid};
use crate::transaction::normal::{nibble, prefix_of};
use std::io;

pub(super) enum Entry {
    Child(NodeLocator),
    Seal(CanonicalKey),
}
pub(super) struct Grammar<'node> {
    parent: &'node NodeLocator,
    children: u16,
    seals: usize,
}
impl<'node> Grammar<'node> {
    pub fn new(parent: &'node NodeLocator) -> Self {
        Self {
            parent,
            children: 0,
            seals: 0,
        }
    }
    pub fn admit(&mut self, name: &str) -> io::Result<Entry> {
        if let Some(digits) = name.strip_suffix(".seal") {
            if self.parent.nibbles == 0
                || self.children != 0
                || self.seals == 128
                || digits.len() != self.parent.namespace.width() * 2
            {
                return Err(invalid());
            }

            let root = NodeLocator {
                namespace: self.parent.namespace,
                prefix: [0; 60],
                nibbles: 0,
            };
            let key = child_locator(&root, digits)?;
            if !prefix_of(self.parent, &key) {
                return Err(invalid());
            }

            self.seals += 1;
            Ok(Entry::Seal(CanonicalKey::from_bytes(
                key.namespace,
                &key.prefix[..key.namespace.width()],
            )?))
        } else {
            if self.seals != 0 {
                return Err(invalid());
            }

            let child = child_locator(self.parent, name)?;
            let route = 1_u16 << nibble(&child.prefix, usize::from(self.parent.nibbles));
            if self.children & route != 0 {
                return Err(invalid());
            }

            self.children |= route;
            Ok(Entry::Child(child))
        }
    }
}

pub(super) fn child_locator(parent: &NodeLocator, name: &str) -> io::Result<NodeLocator> {
    if name.is_empty() || usize::from(parent.nibbles) + name.len() > parent.namespace.width() * 2 {
        return Err(invalid());
    }
    let mut bytes = parent.prefix;
    for (offset, byte) in name.bytes().enumerate() {
        let value = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => return Err(invalid()),
        };
        let index = usize::from(parent.nibbles) + offset;
        if index.is_multiple_of(2) {
            bytes[index / 2] = value << 4;
        } else {
            bytes[index / 2] |= value;
        }
    }
    NodeLocator::from_key(
        parent.namespace,
        CanonicalKey::from_bytes(parent.namespace, &bytes[..parent.namespace.width()])?,
        u8::try_from(usize::from(parent.nibbles) + name.len()).map_err(|_| invalid())?,
    )
}
