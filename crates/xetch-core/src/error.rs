use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Dependency(String),
    #[error("the result was published, but final durability/cleanup failed: {0}; inspect the workspace or output before retrying")]
    CommitUncertain(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: match self {
                Self::Invalid(_) => "invalid_argument",
                Self::Unsupported(_) => "unsupported",
                Self::Conflict(_) => "conflict",
                Self::Dependency(_) => "dependency_failure",
                Self::CommitUncertain(_) => "commit_uncertain",
                Self::Io(_) => "io_error",
                Self::Json(_) => "invalid_json",
            },
            message: self.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
