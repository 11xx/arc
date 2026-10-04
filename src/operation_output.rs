//! Nestable command output capture for coordinators that render one document.
use std::cell::RefCell;
use std::fmt::Arguments;
use std::io::Write;

thread_local! {
    static CAPTURES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn line(arguments: Arguments<'_>) {
    CAPTURES.with(|captures| {
        if let Some(output) = captures.borrow_mut().last_mut() {
            use std::fmt::Write;
            writeln!(output, "{arguments}").expect("writing to a string");
        } else {
            writeln!(std::io::stdout().lock(), "{arguments}").expect("writing command output");
        }
    });
}

pub(crate) fn capture<T>(operation: impl FnOnce() -> T) -> (T, String) {
    struct Capture;
    impl Drop for Capture {
        fn drop(&mut self) {
            CAPTURES.with(|captures| {
                captures.borrow_mut().pop();
            });
        }
    }
    CAPTURES.with(|captures| captures.borrow_mut().push(String::new()));
    let guard = Capture;
    let result = operation();
    let output = CAPTURES.with(|captures| captures.borrow_mut().last_mut().unwrap().clone());
    drop(guard);
    (result, output)
}

#[macro_export]
macro_rules! operation_println {
    () => { $crate::operation_output::line(format_args!("")) };
    ($($argument:tt)*) => { $crate::operation_output::line(format_args!($($argument)*)) };
}
