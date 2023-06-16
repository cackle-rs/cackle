use log::info;

use crate::events::AppEvent;
use crate::outcome::Outcome;
use crate::problem::Problem;
use crate::problem::ProblemList;
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
    entries: Vec<Entry>,
    event_sender: Sender<AppEvent>,
    pub(crate) has_aborted: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProblemStoreIndex {
    a: usize,
    b: usize,
}

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

    /// Reports an error and waits until it's acknowledged or the UI shuts down.
    pub(crate) fn report_error(&mut self, error: anyhow::Error) {
        let outcome = self.lock().report_error(error);
        let _ = outcome.recv();
    }

    pub(crate) fn lock(&self) -> MutexGuard<ProblemStore> {
        self.inner.lock().unwrap()
    }
}

impl ProblemStore {
    fn new(event_sender: Sender<AppEvent>) -> Self {
        Self {
            entries: Vec::new(),
            event_sender,
            has_aborted: false,
        }
    }

    /// Adds `problems` to this store. The returned receiver will receive a single value once all
    /// problems in the supplied list have been resolved, or abort has been called. The supplied
    /// problem list must not be empty.
    fn add(&mut self, problems: ProblemList) -> Receiver<Outcome> {
        for problem in &problems {
            info!("Reported problem: {}", problem.short_description());
        }
        assert!(!problems.is_empty());
        let (sender, receiver) = std::sync::mpsc::channel();
        self.entries.push(Entry {
            problems,
            sender: Some(sender),
        });
        let _ = self.event_sender.send(AppEvent::ProblemsAdded);
        receiver
    }

    /// Within each problem list, group problems by type and crate.
    pub(crate) fn group_by_crate(&mut self) {
        for plist in &mut self.entries {
            let mut problems = ProblemList::default();
            std::mem::swap(&mut problems, &mut plist.problems);
            problems = problems.grouped_by_type_and_crate();
            std::mem::swap(&mut problems, &mut plist.problems);
        }
    }

    pub(crate) fn report_error(&mut self, error: anyhow::Error) -> Receiver<Outcome> {
        self.add(Problem::new(error.to_string()).into())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.iter().all(|entry| entry.problems.is_empty())
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.iter().map(|entry| entry.problems.len()).sum()
    }

    pub(crate) fn resolve(&mut self, index: ProblemStoreIndex) {
        let entry = &mut self.entries[index.a];
        let problem = entry.problems.remove(index.b);
        info!("Resolved problem: {}", problem.short_description());
        if entry.problems.is_empty() {
            if let Some(sender) = entry.sender.take() {
                let _ = sender.send(Outcome::Continue);
            }
            self.entries.remove(index.a);
        }
    }

    pub(crate) fn abort(&mut self) {
        for mut entry in &mut self.entries.drain(..) {
            if let Some(sender) = entry.sender.take() {
                let _ = sender.send(Outcome::GiveUp);
            }
        }
        self.has_aborted = true;
    }
}

struct Entry {
    problems: ProblemList,
    sender: Option<Sender<Outcome>>,
}

pub(crate) struct ProblemStoreIterator<'a> {
    store: &'a ProblemStore,
    index: ProblemStoreIndex,
}

impl<'a> Iterator for ProblemStoreIterator<'a> {
    type Item = (ProblemStoreIndex, &'a Problem);

    fn next(&mut self) -> Option<Self::Item> {
        let item = self
            .store
            .entries
            .get(self.index.a)?
            .problems
            .get(self.index.b)?;
        let item_index = self.index;
        self.index.b += 1;
        while let Some(entry) = self.store.entries.get(self.index.a) {
            if self.index.b < entry.problems.len() {
                break;
            }
            self.index.b = 0;
            self.index.a += 1;
        }
        Some((item_index, item))
    }
}

impl<'a> IntoIterator for &'a ProblemStore {
    type Item = (ProblemStoreIndex, &'a Problem);

    type IntoIter = ProblemStoreIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        ProblemStoreIterator {
            store: self,
            index: ProblemStoreIndex::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProblemStore;
    use crate::problem::Problem;
    use crate::problem::ProblemList;
    use std::sync::mpsc::channel;
    use std::sync::mpsc::TryRecvError;

    fn create_problems() -> ProblemList {
        let mut problems = ProblemList::default();
        problems.push(Problem::UsesBuildScript("crab1".to_owned()));
        problems.push(Problem::UsesBuildScript("crab2".to_owned()));
        problems
    }
    #[test]
    fn basic_queries() {
        let mut store = ProblemStore::new(channel().0);
        store.add(create_problems());
        store.add(create_problems());

        assert_eq!(store.len(), 4);

        let mut iter = store.into_iter();
        assert_eq!(
            iter.next().map(|(_, v)| v),
            Some(&Problem::UsesBuildScript("crab1".to_owned()))
        );
        assert_eq!(
            iter.next().map(|(_, v)| v),
            Some(&Problem::UsesBuildScript("crab2".to_owned()))
        );
        assert_eq!(
            iter.next().map(|(_, v)| v),
            Some(&Problem::UsesBuildScript("crab1".to_owned()))
        );
        assert_eq!(
            iter.next().map(|(_, v)| v),
            Some(&Problem::UsesBuildScript("crab2".to_owned()))
        );
        assert_eq!(iter.next().map(|(_, v)| v), None);

        assert_eq!(store.into_iter().count(), 4);
    }

    #[test]
    fn all_resolved() {
        let mut store = ProblemStore::new(channel().0);
        let done1 = store.add(create_problems());
        let done2 = store.add(create_problems());

        assert_eq!(done1.try_recv(), Err(TryRecvError::Empty));
        store.resolve(store.into_iter().next().unwrap().0);
        assert_eq!(done1.try_recv(), Err(TryRecvError::Empty));
        store.resolve(store.into_iter().next().unwrap().0);
        assert_eq!(done1.try_recv(), Ok(crate::outcome::Outcome::Continue));

        assert_eq!(done2.try_recv(), Err(TryRecvError::Empty));
        store.resolve(store.into_iter().next().unwrap().0);
        assert_eq!(done2.try_recv(), Err(TryRecvError::Empty));
        store.resolve(store.into_iter().next().unwrap().0);
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
}
