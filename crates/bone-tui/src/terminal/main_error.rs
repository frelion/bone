use std::io::Write;

pub(crate) fn write(message: &std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}
