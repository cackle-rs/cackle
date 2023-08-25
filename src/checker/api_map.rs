use crate::config::ApiName;
use fxhash::FxHashMap;
use fxhash::FxHashSet;

/// A map from a path prefix to a set of APIs. Stored as a tree where each level of the tree does
/// lookup for the next part of the name. e.g. `std::path::PathBuf` would be stored as a tree with 4
/// levels. The root is the empty path and should have an empty API set, then a tree node for each
/// of `std`, `path` and `PathBuf`.
///
/// This structure is kind of a trie. Each level however dispatches a whole word rather than a
/// character like you'd have with a typical trie.
///
/// Lookups are done using iterators, which allows us to efficiently find the permissions for a path
/// without heap allocation.
#[derive(Default)]
pub(super) struct ApiMap {
    apis: FxHashSet<ApiName>,
    map: FxHashMap<String, Box<ApiMap>>,
}

impl ApiMap {
    /// Returns the permissions for the path produced by `key_it`. The permissions are those on
    /// whatever node we reach when either `key_it` ends or we have no child node for the next value
    /// it produces. i.e. it's the deepest node that is a prefix of the name produced by `key_it`.
    pub(super) fn get<'a>(&self, mut key_it: impl Iterator<Item = &'a str>) -> &FxHashSet<ApiName> {
        key_it
            .next()
            .and_then(|key| self.map.get(key))
            .map(|sub| sub.get(key_it))
            .unwrap_or(&self.apis) as _
    }

    /// Creates nodes to represent the name produced by `key_it`. This should be called for all path
    /// prefixes that we care about before calling `mut_tree` on those names path prefixes.
    pub(super) fn create_entry<'a>(&mut self, mut key_it: impl Iterator<Item = &'a str>) {
        if let Some(key) = key_it.next() {
            self.map
                .entry(key.to_owned())
                .or_default()
                .create_entry(key_it)
        }
    }

    /// Returns the mutable tree rooted at the path indicated by `key_it`. Panics if such a tree
    /// doesn't exit. i.e. you must have previously called `create_entry` for `key_it`.
    pub(super) fn mut_tree<'a>(
        &mut self,
        mut key_it: impl Iterator<Item = &'a str>,
    ) -> &mut ApiMap {
        match key_it.next() {
            Some(key) => self
                .map
                .get_mut(key)
                .expect("mut_tree called without calling create_entry")
                .mut_tree(key_it),
            _ => self,
        }
    }

    /// Modifies the APIs for this node in the subtree and all child nodes.
    pub(super) fn update_subtree(&mut self, mutator: &impl Fn(&mut FxHashSet<ApiName>)) {
        (mutator)(&mut self.apis);
        for subtree in self.map.values_mut() {
            subtree.update_subtree(mutator);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.apis.clear();
        self.map.clear();
    }
}
