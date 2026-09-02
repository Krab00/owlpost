//! Subcommand implementations plus the shared exit-code error type.

pub mod ask;

/// An error that carries its §9 exit code (`2` offline/unavailable, `3` rate limited,
/// `4` nothing to do). Plain `anyhow` errors keep exiting `1`.
#[derive(Debug)]
pub struct ExitError {
    pub code: u8,
    pub msg: String,
}

impl ExitError {
    pub fn error(code: u8, msg: impl Into<String>) -> anyhow::Error {
        anyhow::Error::new(ExitError {
            code,
            msg: msg.into(),
        })
    }
}

impl std::fmt::Display for ExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for ExitError {}

/// Exit code for a failed command: the `ExitError` code when there is one, else `1`.
pub fn exit_code(e: &anyhow::Error) -> u8 {
    e.downcast_ref::<ExitError>().map_or(1, |x| x.code)
}
