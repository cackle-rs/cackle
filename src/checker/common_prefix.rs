use super::ApiUsages;
use crate::checker::ApiUsage;
use crate::demangle::DemangleToken;
use crate::names::NamesIterator;
use crate::names::SymbolOrDebugName;
use anyhow::Result;
use fxhash::FxHashSet;

/// Returns a list of name prefixes that are common to all of the from-names in the supplied API
/// usages. These are candidates for names that are missing from an API when an off-tree usage is
/// detected.
pub(crate) fn common_from_prefixes(usages: &ApiUsages) -> Result<Vec<String>> {
    common_prefixes(usages, true)
}

/// Returns a list of name prefixes that are common to all of the to-names in the supplied API
/// usages.
pub(crate) fn common_to_prefixes(usages: &ApiUsages) -> Result<Vec<String>> {
    common_prefixes(usages, false)
}

pub(crate) fn common_prefixes(usages: &ApiUsages, from: bool) -> Result<Vec<String>> {
    let mut checker = CommonPrefixChecker::default();

    for usage in &usages.usages {
        checker.check_usage(usage, from)?;
    }
    let mut prefixes: Vec<String> = checker.common.into_iter().map(|s| s.join("::")).collect();
    prefixes.sort();
    Ok(prefixes)
}

#[derive(Default)]
struct CommonPrefixChecker<'input> {
    num_names: u32,
    common: FxHashSet<Vec<&'input str>>,
}

impl<'input> CommonPrefixChecker<'input> {
    fn check_usage(&mut self, usage: &'input ApiUsage, from: bool) -> Result<()> {
        let name = if from { &usage.from } else { &usage.to };
        match name {
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
        let mut prefixes = FxHashSet::default();
        while let Some((name, _)) = names.next_name()? {
            let mut parts = Vec::new();
            for part in name {
                parts.push(part);
                prefixes.insert(parts.clone());
            }
        }
        if self.num_names == 0 {
            self.common = prefixes;
        } else {
            self.common = self.common.intersection(&prefixes).cloned().collect();
        }
        self.num_names += 1;
        Ok(())
    }
}
