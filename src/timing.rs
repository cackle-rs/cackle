use std::collections::hash_map::Entry;
use std::fmt::Display;
use std::time::Duration;
use std::time::Instant;

use fxhash::FxHashMap;

/// Records how long different parts of execution take.
#[derive(Default)]
pub(crate) struct TimingCollector {
    enabled: bool,

    /// The order in which each timing category was first reported. We print timings in this order.
    order: Vec<&'static str>,

    /// The total time for each category.
    timings: FxHashMap<&'static str, Duration>,
}

impl TimingCollector {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            order: Vec::new(),
            timings: FxHashMap::default(),
        }
    }

    /// Adds duration since `start` to the timing category `timing`. Returns the time now, which can
    /// optionally be used to record the time to the next event.
    pub(crate) fn add_timing(&mut self, start: Instant, timing: &'static str) -> Instant {
        let now = Instant::now();
        if !self.enabled {
            return now;
        }
        let elapsed = now - start;
        match self.timings.entry(timing) {
            Entry::Occupied(mut entry) => {
                *entry.get_mut() += elapsed;
            }
            Entry::Vacant(entry) => {
                entry.insert(elapsed);
                self.order.push(timing);
            }
        }
        now
    }
}

impl Display for TimingCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for key in &self.order {
            writeln!(f, "{key}: {:0.3}s", self.timings[key].as_secs_f32())?
        }
        Ok(())
    }
}
