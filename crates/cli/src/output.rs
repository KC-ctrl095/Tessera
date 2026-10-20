//! A closed output pipe is normal for commands used with `head`.

use std::fmt::Arguments;
use std::io::{self, Write};

pub fn print(args: Arguments<'_>) {
    if let Err(error) = io::stdout().lock().write_fmt(args) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        panic!("failed printing to stdout: {error}");
    }
}

pub fn exit_code(result: anyhow::Result<i32>) -> anyhow::Result<i32> {
    match result {
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
            }) =>
        {
            Ok(0)
        }
        other => other,
    }
}

// In scope for every CLI module, including streaming and setup commands.
macro_rules! print {
    ($($arg:tt)*) => { $crate::output::print(format_args!($($arg)*)) };
}
macro_rules! println {
    () => { $crate::output::print(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::output::print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_pipe_errors_exit_successfully_even_with_context() {
        let error = anyhow::Error::new(io::Error::from(io::ErrorKind::BrokenPipe))
            .context("writing the answer");
        assert_eq!(exit_code(Err(error)).unwrap(), 0);
        assert_eq!(exit_code(Ok(2)).unwrap(), 2);
        assert!(exit_code(Err(io::Error::from(io::ErrorKind::PermissionDenied).into())).is_err());
    }
}
