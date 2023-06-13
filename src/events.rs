#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppEvent {
    /// Shutdown in progress. The UI should close.
    Shutdown,
    /// New problems have been added to the problem store.
    ProblemsAdded,
}
