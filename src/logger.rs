use log::Level;
use settings::LogFmt;
use std::io::stderr;
use std::io::Write;

pub fn flush() {
    // flushing `println!` shouldn't be nessasary unless we are writing
    // to a hard fd or line discipline is fully buffered. i've
    // implemented `flush` anyway to this to ensure no termios emit bugs
    let _ = stderr().flush();
}

mod raw_logger {
    use log::{Level, Log, Metadata, Record};

    struct RawLogger {
        level: Level,
        name: String,
    }

    impl Log for RawLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            metadata.level() <= self.level
        }

        fn log(&self, record: &Record) {
            if self.enabled(record.metadata()) {
                eprintln!(
                    "{} {:<5} [{}] {}",
                    self.name,
                    record.level().to_string(),
                    record.module_path().unwrap_or_default(),
                    record.args()
                );
            }
        }

        fn flush(&self) {
            super::flush()
        }
    }

    pub fn build(name: &str, level: Level) -> Box<Log> {
        Box::new(RawLogger {
            level,
            name: name.to_owned(),
        })
    }
}

mod json_logger {
    use log::{Level, Log, Metadata, Record};

    pub struct JsonLogger {
        level: Level,
        name: String,
    }

    impl Log for JsonLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            metadata.level() <= self.level
        }

        fn log(&self, record: &Record) {
            let json_record = json!({
                "level": record.level().to_string(),
                "name": self.name,
                "src": {
                    "module_path": record.module_path().unwrap_or_default(),
                    "file": record.file(),
                    "line": record.line()
                },
                "msg": record.args().to_string()
            });

            if self.enabled(record.metadata()) {
                eprintln!("{}", json_record.to_string());
            }
        }

        fn flush(&self) {
            super::flush()
        }
    }

    pub fn build(name: &str, level: Level) -> Box<Log> {
        Box::new(JsonLogger {
            level,
            name: name.to_owned(),
        })
    }
}

use log::{set_boxed_logger, set_max_level, SetLoggerError};

pub fn init_logger(log_type: &LogFmt, name: &str, level: Level) -> Result<(), SetLoggerError> {
    set_boxed_logger(match log_type {
        LogFmt::Raw => raw_logger::build(name, level),
        LogFmt::Json => json_logger::build(name, level),
    })?;

    set_max_level(level.to_level_filter());

    Ok(())
}
