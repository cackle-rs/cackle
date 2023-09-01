use crate::checker::BinLocation;
use crate::location::SourceLocation;
use anyhow::Context;
use anyhow::Result;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use gimli::Dwarf;
use std::fmt::Display;

#[derive(Default)]
pub(crate) struct Backtracer {
    /// A map from symbol addresses in the binary to a list of relocations pointing to that address.
    back_references: FxHashMap<u64, Vec<BinLocation>>,

    bin_bytes: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) name: String,
    pub(crate) source_location: Option<SourceLocation>,
    inlined: bool,
}

impl Backtracer {
    /// Declare a reference from `bin_location` to `target_address`.
    pub(crate) fn add_reference(&mut self, bin_location: BinLocation, target_address: u64) {
        self.back_references
            .entry(target_address)
            .or_default()
            .push(bin_location);
    }

    pub(crate) fn provide_bin_bytes(&mut self, bin_bytes: Vec<u8>) {
        self.bin_bytes = bin_bytes;
    }

    pub(crate) fn backtrace(&self, bin_location: BinLocation) -> Result<Vec<Frame>> {
        let mut addresses = Vec::new();
        self.find_frames(
            &mut vec![],
            bin_location,
            &mut addresses,
            &mut FxHashSet::default(),
        );

        let obj = object::File::parse(self.bin_bytes.as_slice()).with_context(|| {
            format!(
                "Backtrace failed to parse bin file of size {}",
                self.bin_bytes.len()
            )
        })?;
        let owned_dwarf = Dwarf::load(|id| super::load_section(&obj, id))?;
        let dwarf =
            owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
        let ctx = addr2line::Context::from_dwarf(dwarf)
            .context("Failed in addr2line during backtrace")?;

        let mut backtrace: Vec<Frame> = Vec::new();
        for address in addresses {
            let mut frame_iter = ctx.find_frames(address).skip_all_loads()?;
            let mut first = true;
            while let Some(frame) = frame_iter.next()? {
                let name = frame
                    .function
                    .and_then(|n| n.name.to_string().ok())
                    .map(|n| format!("{:#}", rustc_demangle::demangle(n)))
                    .unwrap_or_else(|| "??".to_owned());
                let source_location = frame
                    .location
                    .and_then(|location| SourceLocation::try_from(&location).ok());
                if first {
                    first = false;
                } else {
                    // Mark all frames except the last one as inlined.
                    backtrace.last_mut().unwrap().inlined = true;
                }
                backtrace.push(Frame {
                    name,
                    source_location,
                    inlined: false,
                })
            }
        }
        Ok(backtrace)
    }

    /// Find the longest sequence of addresses leading to `bin_location`. Why longest? Just a guess
    /// that it's likely to be the most interesting.
    fn find_frames(
        &self,
        candiate: &mut Vec<u64>,
        bin_location: BinLocation,
        out: &mut Vec<u64>,
        visited: &mut FxHashSet<u64>,
    ) {
        if !visited.insert(bin_location.address) {
            return;
        }
        candiate.push(bin_location.address);
        if let Some(references) = self.back_references.get(&bin_location.symbol_start) {
            for reference in references {
                self.find_frames(candiate, *reference, out, visited);
            }
        } else if candiate.len() > out.len() {
            out.resize(candiate.len(), 0);
            out.copy_from_slice(candiate);
        }
        candiate.pop();
    }
}

impl Display for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.name.fmt(f)?;
        if self.inlined {
            " (inlined)".fmt(f)?;
        }
        Ok(())
    }
}
