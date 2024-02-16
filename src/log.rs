use std::{cell::UnsafeCell, collections::HashMap, fmt::Arguments, mem::MaybeUninit, str::FromStr};

macro_rules! log {
    ($level:expr, $($arg:tt)+) => {
        if $level >= $crate::log::level(module_path!()) {
            $crate::log::log(
                $level,
                file!(),
                line!(),
                format_args!($($arg)+))
        }
    }
}

macro_rules! debug {
    ($($arg:tt)+) => {
        log!($crate::log::Level::Debug, $($arg)+)
    }
}

macro_rules! info {
    ($($arg:tt)+) => {
        log!($crate::log::Level::Info, $($arg)+)
    }
}

macro_rules! warn {
    ($($arg:tt)+) => {
        log!($crate::log::Level::Warn, $($arg)+)
    }
}

macro_rules! error {
    ($($arg:tt)+) => {
        log!($crate::log::Level::Error, $($arg)+)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl FromStr for Level {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_ref() {
            "debug" => Ok(Level::Debug),
            "info" => Ok(Level::Info),
            "warn" => Ok(Level::Warn),
            "error" => Ok(Level::Error),
            _ => Err(()),
        }
    }
}

impl Level {
    fn log_str(&self) -> &'static str {
        match self {
            Level::Debug => "\x1b[32;1mDEBUG\x1b[m",
            Level::Info => "\x1b[34;1m INFO\x1b[m",
            Level::Warn => "\x1b[33;1m WARN\x1b[m",
            Level::Error => "\x1b[91;1mERROR\x1b[m",
        }
    }
}

struct StaticLogLevels(UnsafeCell<MaybeUninit<HashMap<String, Level>>>);
unsafe impl Sync for StaticLogLevels {}

static LOG_LEVELS: StaticLogLevels = StaticLogLevels(UnsafeCell::new(MaybeUninit::uninit()));

pub fn init() {
    let log_str = std::env::var("TERMIE_LOG");
    let Ok(log_str) = log_str else {
        unsafe {
            (*LOG_LEVELS.0.get()).write(HashMap::new());
        }
        return;
    };

    let mut levels = HashMap::new();

    for kv in log_str.split(';') {
        let last_equals = kv
            .chars()
            .enumerate()
            .filter(|(_i, c)| *c == '=')
            .map(|(i, _c)| i)
            .last();
        let Some(last_equals) = last_equals else {
            continue;
        };

        let (module, level) = kv.split_at(last_equals);
        let Ok(level) = level[1..].parse() else {
            continue;
        };
        levels.insert(module.to_string(), level);
    }

    unsafe {
        (*LOG_LEVELS.0.get()).write(levels);
    }
}

pub fn log(level: Level, file: &str, line: u32, args: Arguments) {
    print!("[{}] {file}:{line} ", level.log_str());
    println!("{}", args);
}

pub fn level(module_path: &str) -> Level {
    unsafe {
        let levels = (*LOG_LEVELS.0.get()).assume_init_ref();
        *levels.get(module_path).unwrap_or(&Level::Info)
    }
}
