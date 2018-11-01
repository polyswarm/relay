use log::Level;
use settings::Logging;

mod raw_logger {
    use log::{set_boxed_logger, set_max_level, Level, Log, Metadata, Record, SetLoggerError};

    struct SimpleLogger {
        name: String,
        level: Level,
    }

    impl Log for SimpleLogger {
        fn enabled(&self, metadata: &Metadata) -> bool {
            metadata.level() <= self.level
        }

        fn log(&self, record: &Record) {
            if self.enabled(record.metadata()) {
                println!(
                    "{} {:<5} [{}] {}",
                    self.name,
                    record.level().to_string(),
                    record.module_path().unwrap_or_default(),
                    record.args()
                );
            }
        }

        fn flush(&self) {}
    }

    pub fn init(name: &str, level: Level) -> Result<(), SetLoggerError> {
        let logger = SimpleLogger {
            name: name.to_owned(),
            level: level,
        };

        set_boxed_logger(Box::new(logger))?;
        set_max_level(level.to_level_filter());

        Ok(())
    }
}

mod json_logger {
    use log::{set_boxed_logger, set_max_level, Level, Log, Metadata, Record, SetLoggerError};

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
                println!("{}", json_record.to_string());
            }
        }

        fn flush(&self) {}
    }

    pub fn init(name: &str, level: Level) -> Result<(), SetLoggerError> {
        let logger = JsonLogger {
            level: level,
            name: name.to_owned(),
        };

        set_boxed_logger(Box::new(logger))?;
        set_max_level(level.to_level_filter());

        Ok(())
    }
}

pub fn init_logger(log_type: Logging, name: &str, level: Level) {
    match log_type {
        Logging::Raw => raw_logger::init(name, level),
        Logging::Json => json_logger::init(name, level),
    }.unwrap()
}