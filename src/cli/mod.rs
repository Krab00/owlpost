//! Subcommand implementations and shared CLI helpers, plus the shared exit-code error type.

pub mod ask;
pub mod doctor;
pub mod install;
pub mod watch;

/// An error carrying its §9 process exit code (`2` offline/unavailable, `3` rate limited,
/// `4` nothing to do, e.g. a `watch` or `ask --wait` timeout). `main` downcasts it; every
/// other error exits `1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitError {
    pub code: u8,
    pub message: String,
}

impl ExitError {
    pub fn new(code: u8, message: impl Into<String>) -> ExitError {
        ExitError {
            code,
            message: message.into(),
        }
    }

    /// `new`, already wrapped as an `anyhow::Error` for `?`-style returns.
    pub fn error(code: u8, message: impl Into<String>) -> anyhow::Error {
        ExitError::new(code, message).into()
    }
}

impl std::fmt::Display for ExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitError {}

/// Exit code for a failed command: the `ExitError` code when there is one, else `1`.
pub fn exit_code(e: &anyhow::Error) -> u8 {
    e.downcast_ref::<ExitError>().map_or(1, |x| x.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_error_displays_message_and_downcasts() {
        let e: anyhow::Error = ExitError::new(4, "timeout").into();
        assert_eq!(e.to_string(), "timeout");
        assert_eq!(e.downcast_ref::<ExitError>().map(|x| x.code), Some(4));
        assert_eq!(exit_code(&e), 4);
        let plain = anyhow::anyhow!("boom");
        assert!(plain.downcast_ref::<ExitError>().is_none());
        assert_eq!(exit_code(&plain), 1);
        let wrapped = ExitError::error(3, "rate limited");
        assert_eq!(wrapped.to_string(), "rate limited");
        assert_eq!(exit_code(&wrapped), 3);
    }
}
