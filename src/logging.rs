use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use log::Level;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

pub(crate) fn init(output_path: &Path) -> Result<()> {
    let file = std::fs::File::create(output_path)
        .with_context(|| format!("Failed to write log file `{}`", output_path.display()))?;
    log::set_boxed_logger(Box::new(FileLogger {
        file: Mutex::new(file),
        start: Instant::now(),
    }))
    .map_err(|_| anyhow!("Failed to set logger"))?;
    log::set_max_level(log::LevelFilter::Info);
    Ok(())
}

struct FileLogger {
    file: Mutex<std::fs::File>,
    start: Instant,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            // If a write to our log file fails, there's not a lot we can do, so we just ignore it.
            let mut file = self.file.lock().unwrap();
            let _ = writeln!(
                file,
                "{:0.3}: {} - {}",
                self.start.elapsed().as_secs_f32(),
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        let _ = self.file.lock().unwrap().flush();
    }
}
