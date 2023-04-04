mod built_in_perms;
mod checker;
mod config;
mod config_validation;
mod sandbox;

#[cfg(feature = "rust-analyzer")]
mod ra_based_analyser;

use anyhow::anyhow;
use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use std::path::PathBuf;

/// Analyses rust crates and their dependent crates to see what categories of
/// APIs and language features are used.
#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
struct Args {
    /// Directory containing crate to analyze. Defaults to current working
    /// directory.
    #[clap(short, long)]
    path: Option<PathBuf>,

    /// Path to Cackle.toml. If not specified, looks in the directory containing
    /// the crate to be analyzed.
    #[clap(short, long)]
    cackle: Option<PathBuf>,

    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Usage,
    ShowReferences,
    Check(CheckConfig),
}

#[derive(Parser, Debug)]
struct CheckConfig {
    /// Maximum number of source locations that use an API that should be
    /// reported.
    #[clap(long, default_value = "2")]
    usage_report_cap: i32,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let root_path = args
        .path
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| anyhow!("Failed to get current working directory"))?;

    let config_path = args
        .cackle
        .clone()
        .unwrap_or_else(|| root_path.join("Cackle.toml"));

    let config = config::parse_file(&config_path)?;

    #[cfg(feature = "rust-analyzer")]
    {
        ra_main(&args, &config, &root_path)?;
    }

    Ok(())
}

#[cfg(feature = "rust-analyzer")]
fn ra_main(args: &Args, config: &config::Config, root_path: &std::path::Path) -> Result<()> {
    let analysis_output = ra_based_analyser::analyse_crate(&config, root_path)?;

    match &args.command {
        Command::Usage => {
            //for crate_info in analysis_output.crate_infos {
            //    let mut perms = Vec::from_iter(crate_info.permission_usage.keys());
            //    perms.sort();
            //    println!(
            //        "{}: {}",
            //        crate_info.name,
            //        perms
            //            .iter()
            //            .map(|p| p.to_string())
            //            .collect::<Vec<_>>()
            //            .join(", ")
            //    );
            //}
        }
        Command::ShowReferences => {
            //for crate_info in analysis_output.crate_infos {
            //    println!("{}", crate_info.name);
            //    let mut referenced = Vec::from_iter(crate_info.referenced_paths.into_iter());
            //    referenced.sort();
            //    for path in referenced {
            //        println!("    {path}");
            //    }
            //}
        }
        Command::Check(check_config) => {
            if let Err(unused) = analysis_output.check_unused() {
                println!("{}", unused);
            }

            let mut failed = false;
            for crate_info in &analysis_output.crate_infos {
                if crate_info.disallowed_usage.is_empty() {
                    continue;
                }
                failed = true;
                println!("Crate '{}' uses disallowed APIs:", crate_info.name);
                for (perm_id, usages) in &crate_info.disallowed_usage {
                    let perm = analysis_output.permission_name(perm_id);
                    println!("  {perm}:");
                    let cap = if check_config.usage_report_cap < 0 {
                        usages.len()
                    } else {
                        check_config.usage_report_cap as usize
                    };
                    for usage in usages.iter().take(cap) {
                        println!("    {} {}:{}", perm, usage.filename, usage.line_number + 1);
                    }
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
