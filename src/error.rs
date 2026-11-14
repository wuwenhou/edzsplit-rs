#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),

    #[error("{0}")]
    Message(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Xml(#[from] quick_xml::Error),

    #[error(transparent)]
    Regex(#[from] regex::Error),

    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),

    #[error(transparent)]
    SevenZ(#[from] sevenz_rust2::Error),

    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}

impl AppError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub fn is_usage(&self) -> bool {
        matches!(self, Self::Usage(_))
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
