//! Human output that survives a reader hanging up.
//!
//! `tally query run <id> | head -1` closes the pipe as soon as it has its
//! line. The Rust runtime ignores SIGPIPE process-wide, so the next write
//! returns `BrokenPipe` and stock `println!` turns that into a panic: an
//! operator who asked for one line of a run board got a panic message on
//! stderr and a failing exit code instead of the line they asked for.
//!
//! Every human-facing print on the query surface goes through [`write_line`],
//! which reports a hung-up reader as an ordinary quiet exit — no message, exit
//! status 0, the same shape a pipeline stage that finished early has. The
//! process-wide SIGPIPE disposition is deliberately left alone: `daemon run`,
//! `__remote-executor`, and the unit-exit recorder must keep seeing a closed
//! socket as an error to report rather than a reason to die mid-write, and
//! nothing here changes what any of them are handed.

use std::fmt::Arguments;
use std::io::{self, Write};

use anyhow::Result;

use super::exit::exit_failure;

/// Write one line to stdout, mapping a hung-up reader to a quiet exit.
pub(super) fn write_line(args: Arguments<'_>) -> Result<()> {
    write_to(&mut io::stdout().lock(), "stdout", args)
}

/// [`write_line`] for the operator hints that belong on stderr.
pub(super) fn write_error_line(args: Arguments<'_>) -> Result<()> {
    write_to(&mut io::stderr().lock(), "stderr", args)
}

fn write_to(sink: &mut impl Write, name: &str, args: Arguments<'_>) -> Result<()> {
    match writeln!(sink, "{args}") {
        Ok(()) => Ok(()),
        // The empty message is load-bearing: `cli::main` prints nothing for an
        // error that carries none, which is exactly the silence a reader that
        // stopped reading deserves. Nothing may add context to it on the way
        // out, or the hang-up starts speaking again.
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(exit_failure(0, "")),
        Err(error) => Err(anyhow::Error::new(error).context(format!("writing to {name}"))),
    }
}

/// `println!` that reports a broken pipe instead of panicking on it. Usable
/// only inside a function returning `anyhow::Result`.
macro_rules! outln {
    () => {
        $crate::cli::out::write_line(format_args!(""))?
    };
    ($($arg:tt)*) => {
        $crate::cli::out::write_line(format_args!($($arg)*))?
    };
}

/// `eprintln!` counterpart of [`outln`].
macro_rules! errln {
    ($($arg:tt)*) => {
        $crate::cli::out::write_error_line(format_args!($($arg)*))?
    };
}

pub(super) use {errln, outln};

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink whose every write fails the way a closed pipe does.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "Broken pipe"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct UnwritableSink;

    impl Write for UnwritableSink {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("device is on fire"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_hung_up_reader_is_a_silent_exit_zero() {
        let error = write_to(&mut ClosedPipe, "stdout", format_args!("a line")).unwrap_err();
        assert_eq!(error.to_string(), "");
        assert_eq!(super::super::error_exit_code(&error), 0);
    }

    #[test]
    fn any_other_write_failure_is_still_an_error_with_a_message() {
        let error = write_to(&mut UnwritableSink, "stdout", format_args!("a line")).unwrap_err();
        assert!(error.to_string().contains("writing to stdout"), "{error:#}");
        assert_eq!(super::super::error_exit_code(&error), 1);
    }

    #[test]
    fn a_writable_sink_takes_the_whole_line() {
        let mut sink = Vec::new();
        write_to(&mut sink, "stdout", format_args!("{}-{}", "a", 2)).unwrap();
        assert_eq!(sink, b"a-2\n");
    }
}
