//! Subcommand implementations and shared CLI helpers.

pub mod doctor;
pub mod install;
pub mod watch;

/// An error carrying a specific process exit code (§9: `4` = nothing to do, e.g. a `watch`
/// timeout). `main` downcasts it; every other error exits `1`.
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
}

impl std::fmt::Display for ExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_error_displays_message_and_downcasts() {
        let e: anyhow::Error = ExitError::new(4, "timeout").into();
        assert_eq!(e.to_string(), "timeout");
        assert_eq!(e.downcast_ref::<ExitError>().map(|x| x.code), Some(4));
        let plain = anyhow::anyhow!("boom");
        assert!(plain.downcast_ref::<ExitError>().is_none());
    }
}
