use crate::events::AppEvent;
use crate::outcome::Outcome;
use crate::problem::Problem;
use crate::problem::ProblemList;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use log::info;
use std::collections::hash_map::Entry;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

pub(crate) fn create(event_sender: Sender<AppEvent>) -> ProblemStoreRef {
    ProblemStoreRef {
        inner: Arc::new(Mutex::new(ProblemStore::new(event_sender))),
    }
}

/// A store of multiple `ProblemList` instances that allows signalling when a problem list is
/// resolved.
pub(crate) struct ProblemStore {
    /// Our problems. Entries are none once each problem is resolved. Indexed by ProblemId. To keep
    /// ProblemIds stable, we avoid actually removing entries.
    problems: Vec<Option<Problem>>,
    notification_entries: Vec<NotificationEntry>,
    id_by_deduplication_key: FxHashMap<Problem, ProblemId>,
    event_sender: Sender<AppEvent>,
    pub(crate) has_aborted: bool,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct ProblemId(usize);

#[derive(Clone)]
pub(crate) struct ProblemStoreRef {
    inner: Arc<Mutex<ProblemStore>>,
}

impl ProblemStoreRef {
    /// Reports some problems and waits until either they're resolved, or we abort.
    pub(crate) fn fix_problems(&mut self, problems: ProblemList) -> Outcome {
        if problems.is_empty() {
            return Outcome::Continue;
        }
        let outcome = self.lock().add(problems);
        outcome.recv().unwrap_or(Outcome::GiveUp)
    }

    pub(crate) fn lock(&self) -> MutexGuard<ProblemStore> {
        self.inner.lock().unwrap()
    }
}

impl ProblemStore {
    fn new(event_sender: Sender<AppEvent>) -> Self {
        Self {
            problems: Default::default(),
            notification_entries: Default::default(),
            id_by_deduplication_key: Default::default(),
            event_sender,
            has_aborted: false,
        }
    }

    /// Adds `problems` to this store. The returned receiver will receive a single value once all
    /// problems in the supplied list have been resolved, or abort has been called. The supplied
    /// problem list must not be empty.
    fn add(&mut self, problems: ProblemList) -> Receiver<Outcome> {
        for problem in &problems {
            info!("Reported problem: {problem}");
        }
        assert!(!problems.is_empty());
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut problem_ids = FxHashSet::default();
        for problem in problems.take() {
            problem_ids.insert(self.add_problem(problem));
        }
        self.notification_entries.push(NotificationEntry {
            problem_ids,
            sender: Some(sender),
        });
        let _ = self.event_sender.send(AppEvent::ProblemsAdded);
        receiver
    }

    /// Resolve all problems for which at least one edit, when applied to `editor` gives an empty
    /// diff, provided that edit is not expected to produce an empty diff.
    #[cfg(feature = "ui")]
    pub(crate) fn resolve_problems_with_empty_diff(
        &mut self,
        editor: &crate::config_editor::ConfigEditor,
        config: &crate::config::Config,
    ) {
        let current_toml = editor.to_toml();
        let mut empty_indexes = Vec::new();
        for (index, problem) in self.deduplicated_into_iter() {
            for edit in crate::config_editor::fixes_for_problem(problem, config) {
                if !edit.resolve_problem_if_edit_is_empty() {
                    continue;
                }
                let mut editor_copy = editor.clone();
                if edit.apply(&mut editor_copy, &Default::default()).is_ok()
                    && editor_copy.to_toml() == current_toml
                {
                    empty_indexes.push(index);
                    info!(
                        "Resolved problem ({problem}) because diff for edit ({edit}) became empty"
                    );
                    break;
                }
            }
        }
        // When we resolve a problem, the indexes of all problems after it are invalided, however
        // those before it remain valid. So we reverse our list of indexes so that we process from
        // the end and thus only invalidate those indexes that we've already processed.
        empty_indexes.reverse();
        for index in empty_indexes {
            self.resolve(index);
        }
    }

    pub(crate) fn deduplicated_into_iter(&self) -> impl Iterator<Item = (ProblemId, &Problem)> {
        ProblemStoreIterator {
            store: self,
            id: ProblemId(0),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.problems.iter().all(|p| p.is_none())
    }

    pub(crate) fn len(&self) -> usize {
        self.problems.iter().filter(|p| p.is_some()).count()
    }

    pub(crate) fn resolve(&mut self, id: ProblemId) {
        self.replace(id, ProblemList::default());
    }

    pub(crate) fn replace(&mut self, id: ProblemId, replacement: ProblemList) {
        let problem = self
            .problems
            .get_mut(id.0)
            .expect("Called ProblemStore::replace with invalid ID")
            .take()
            .expect("Called ProblemStore::replace with ID that was already resolved");
        let replacement_ids: Vec<ProblemId> = replacement
            .take()
            .into_iter()
            .map(|problem| self.add_problem(problem))
            .collect();
        for entry in &mut self.notification_entries {
            entry.replace_problem(id, &replacement_ids);
        }
        info!("Resolved problem: {problem}");
        // If we try to add an equivalent problem later, it should get a new ID, not reuse this ID -
        // otherwise we'd be adding entries into middle of the list and we should only ever have new
        // entries show up at the end.
        self.id_by_deduplication_key
            .remove(&problem.deduplication_key());
    }

    pub(crate) fn abort(&mut self) {
        for mut entry in &mut self.notification_entries.drain(..) {
            if let Some(sender) = entry.sender.take() {
                let _ = sender.send(Outcome::GiveUp);
            }
        }
        self.has_aborted = true;
    }

    /// Adds a problem, possibly merging it into an existing problem. Returns the ID of that
    /// problem.
    fn add_problem(&mut self, problem: Problem) -> ProblemId {
        match self
            .id_by_deduplication_key
            .entry(problem.deduplication_key())
        {
            Entry::Occupied(entry) => {
                let id = *entry.get();
                let existing_problem = self.problems[id.0]
                    .as_mut()
                    .expect("Internal error: Trying to deduplicate against resolved problem");
                if problem != *existing_problem {
                    existing_problem.merge(problem);
                }
                id
            }
            Entry::Vacant(entry) => {
                let next_id = ProblemId(self.problems.len());
                entry.insert(next_id);
                self.problems.push(Some(problem));
                next_id
            }
        }
    }
}

struct NotificationEntry {
    problem_ids: FxHashSet<ProblemId>,
    sender: Option<Sender<Outcome>>,
}
impl NotificationEntry {
    /// Tries to remove `id`. If `id` was present, then adds all `replacements`.
    fn replace_problem(&mut self, id: ProblemId, replacements: &[ProblemId]) {
        if !self.problem_ids.remove(&id) {
            // ID wasn't present.
            return;
        }
        self.problem_ids.extend(replacements.iter());
        if self.problem_ids.is_empty() {
            if let Some(sender) = self.sender.take() {
                let _ = sender.send(Outcome::Continue);
            }
        }
    }
}

struct ProblemStoreIterator<'a> {
    store: &'a ProblemStore,
    id: ProblemId,
}

impl<'a> Iterator for ProblemStoreIterator<'a> {
    type Item = (ProblemId, &'a Problem);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.id.0 >= self.store.problems.len() {
                return None;
            }
            let id = self.id;
            self.id = ProblemId(id.0 + 1);
            if let Some(problem) = self.store.problems[id.0].as_ref() {
                return Some((id, problem));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProblemStore;
    use crate::crate_index::testing::pkg_id;
    use crate::problem::Problem;
    use crate::problem::ProblemList;
    use crate::problem_store::ProblemId;
    use std::sync::mpsc::channel;
    use std::sync::mpsc::TryRecvError;

    fn create_problems() -> ProblemList {
        let mut problems = ProblemList::default();
        problems.push(Problem::UsesBuildScript(pkg_id("crab1")));
        problems.push(Problem::UsesBuildScript(pkg_id("crab2")));
        problems
    }

    #[test]
    fn basic_queries() {
        let mut store = ProblemStore::new(channel().0);
        store.add(create_problems());
        store.add(create_problems());

        assert_eq!(store.len(), 2);

        let mut iter = store.deduplicated_into_iter();
        assert_eq!(
            iter.next().map(|(_, v)| v),
            Some(&Problem::UsesBuildScript(pkg_id("crab1")))
        );
        assert_eq!(
            iter.next().map(|(_, v)| v),
            Some(&Problem::UsesBuildScript(pkg_id("crab2")))
        );
        assert_eq!(iter.next().map(|(_, v)| v), None);

        assert_eq!(store.deduplicated_into_iter().count(), 2);
    }

    #[test]
    fn all_resolved() {
        fn first_problem_index(store: &ProblemStore) -> Option<ProblemId> {
            Some(store.deduplicated_into_iter().next()?.0)
        }

        let mut store = ProblemStore::new(channel().0);
        let done1 = store.add(create_problems());
        let done2 = store.add(create_problems());

        assert_eq!(done1.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(done2.try_recv(), Err(TryRecvError::Empty));
        store.resolve(first_problem_index(&store).unwrap());
        assert_eq!(done1.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(done2.try_recv(), Err(TryRecvError::Empty));
        store.resolve(first_problem_index(&store).unwrap());
        assert_eq!(done1.try_recv(), Ok(crate::outcome::Outcome::Continue));
        assert_eq!(done2.try_recv(), Ok(crate::outcome::Outcome::Continue));
    }

    #[test]
    fn add_notifications() {
        let (send, recv) = channel();
        let mut store = ProblemStore::new(send);
        assert_eq!(recv.try_recv(), Err(TryRecvError::Empty));
        store.add(create_problems());
        assert_eq!(recv.try_recv(), Ok(crate::events::AppEvent::ProblemsAdded));
        assert_eq!(recv.try_recv(), Err(TryRecvError::Empty));
        store.add(create_problems());
        assert_eq!(recv.try_recv(), Ok(crate::events::AppEvent::ProblemsAdded));
        assert_eq!(recv.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn abort() {
        let mut store = ProblemStore::new(channel().0);
        let done1 = store.add(create_problems());
        let done2 = store.add(create_problems());
        store.abort();
        assert_eq!(done1.try_recv(), Ok(crate::outcome::Outcome::GiveUp));
        assert_eq!(done2.try_recv(), Ok(crate::outcome::Outcome::GiveUp));
    }

    #[test]
    fn deduplicated_iteration() {
        let mut store = ProblemStore::new(channel().0);
        store.add(create_problems());
        store.add(create_problems());
        assert_eq!(store.deduplicated_into_iter().count(), 2);
    }
}
