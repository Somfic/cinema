#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("GStreamer failed to initialise: {0}")]
    Init(String),
    #[error("Missing GStreamer element `{0}`; is the plugin installed?")]
    MissingElement(String),
    #[error("Pipeline error: {0}")]
    Pipeline(String),
    #[error("Timed out {0}")]
    Timeout(&'static str),
    #[error("Unsupported media: {0}")]
    Unsupported(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<gst::glib::BoolError> for Error {
    fn from(e: gst::glib::BoolError) -> Self {
        Error::Pipeline(e.to_string())
    }
}

impl From<gst::glib::Error> for Error {
    fn from(e: gst::glib::Error) -> Self {
        Error::Pipeline(e.to_string())
    }
}

impl From<gst::StateChangeError> for Error {
    fn from(e: gst::StateChangeError) -> Self {
        Error::Pipeline(e.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
