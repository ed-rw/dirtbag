use thiserror::Error;

/// Errors from the `tart` CLI wrapper.
#[derive(Debug, Error)]
pub enum TartError {
    #[error("`tart` binary not found on PATH; install it with `brew install cirruslabs/cli/tart`")]
    NotFound,

    #[error("`tart {cmd}` failed (exit {code}): {stderr}")]
    Command {
        cmd: String,
        code: i32,
        stderr: String,
    },

    #[error("failed to parse `tart {cmd}` output: {source}")]
    Parse {
        cmd: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to invoke tart: {0}")]
    Io(#[from] std::io::Error),
}
