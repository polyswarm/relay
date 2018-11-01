extern crate log;

use std::io::prelude::*;
use std::io::{self, Stdout};
use std::borrow::ToOwned;
use std::str;
use log::{Log,Level,Metadata,Record,SetLoggerError};

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
                record.args());
        }
    }

    fn flush(&self) {
    }
}

pub fn init_raw(name: &str, level: Level) -> Result<(), SetLoggerError> {
    let logger = SimpleLogger {
        name: name.to_owned(),
        level: level
    };
    log::set_boxed_logger(Box::new(logger))?;
    log::set_max_level(level.to_level_filter());
    Ok(())
}

pub struct JsonLogger {
    out: Stdout,
    level: Level,
    name: String,
}

impl Log for JsonLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        let output = json!({
            "level": record.level().to_string(),
            "name": self.name,
            "src": {
                "module_path": record.module_path().unwrap_or_default(),
                "file": record.file(),
                "line": record.line()
            },
            "msg": record.args().to_string()
        });

        writeln!(&mut self.out.lock(), "{}", output.to_string());
    }

    fn flush(&self) {}
}

pub fn init_json(name: &str, level: Level) -> Result<(), SetLoggerError> {
    let logger = JsonLogger {
        out: io::stdout(),
        level: level,
        name: name.to_owned(),
    };

    log::set_boxed_logger(Box::new(logger))?;
    log::set_max_level(level.to_level_filter());

    Ok(())
}
