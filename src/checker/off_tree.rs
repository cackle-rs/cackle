use super::ApiUsages;
use crate::checker::ApiUsage;
use crate::demangle::DemangleToken;
use crate::names::NamesIterator;
use crate::names::SymbolOrDebugName;
use anyhow::Result;
use fxhash::FxHashSet;

/// Returns a list of name prefixes that are common to all of the supplied API usages. These are
/// candidates for names that are missing from an API when an off-tree usage is detected.
pub(crate) fn common_from_prefixes(usages: &ApiUsages) -> Result<Vec<String>> {
    let mut checker = OffTreeUsageChecker::default();

    for usage in &usages.usages {
        checker.check_usage(usage)?;
    }
    Ok(checker
        .common1
        .into_iter()
        .map(|s| s.to_owned())
        .chain(
            checker
                .common2
                .into_iter()
                .map(|(s1, s2)| format!("{s1}::{s2}")),
        )
        .collect())
}

#[derive(Default)]
struct OffTreeUsageChecker<'input> {
    num_names: u32,
    common1: FxHashSet<&'input str>,
    common2: FxHashSet<(&'input str, &'input str)>,
}

impl<'input> OffTreeUsageChecker<'input> {
    fn check_usage(&mut self, usage: &'input ApiUsage) -> Result<()> {
        match &usage.from {
            SymbolOrDebugName::Symbol(symbol) => {
                self.check_names(symbol.names()?)?;
            }
            SymbolOrDebugName::DebugName(debug_name) => {
                self.check_names(debug_name.names_iterator())?;
            }
        }
        Ok(())
    }

    fn check_names<I: Clone + Iterator<Item = DemangleToken<'input>>>(
        &mut self,
        mut names: NamesIterator<'input, I>,
    ) -> Result<()> {
        let mut prefix1 = FxHashSet::default();
        let mut prefix2 = FxHashSet::default();
        while let Some((mut name, _)) = names.next_name()? {
            let (Some(n1), Some(n2)) = (name.next(), name.next()) else {
                continue;
            };
            prefix1.insert(n1);
            prefix2.insert((n1, n2));
        }
        if self.num_names == 0 {
            self.common1 = prefix1;
            self.common2 = prefix2;
        } else {
            self.common1 = self.common1.intersection(&prefix1).cloned().collect();
            self.common2 = self.common2.intersection(&prefix2).cloned().collect();
        }
        self.num_names += 1;
        Ok(())
    }
}
