//! Where a pipeline reads its bytes from.
//!
//! A file on disk goes through `filesrc`. Anything else - a torrent that is
//! still downloading, in practice - goes through [`StreamSrc`], a pull-mode
//! source over an async reader. Pull mode matters: demuxers can then read the
//! index at the end of a file (an MP4 `moov`, Matroska cues) and seek straight
//! to any position, and a read of a missing torrent piece simply blocks until
//! it arrives instead of handing the demuxer zeroes.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use futures::future::BoxFuture;
use gst::prelude::*;
use gst::subclass::prelude::ObjectSubclassIsExt;
use tokio::io::{AsyncRead, AsyncSeek};

use crate::{Error, Result};

pub trait ReadSeek: AsyncRead + AsyncSeek + Send {}
impl<T: AsyncRead + AsyncSeek + Send> ReadSeek for T {}

pub type BoxReader = Pin<Box<dyn ReadSeek>>;

/// Opens a fresh reader over the same bytes. Called once per pipeline, from a
/// GStreamer streaming thread.
pub trait Opener: Send + Sync + 'static {
    /// Total length in bytes. Must be known up front: demuxers ask for it
    /// before the first read.
    fn size(&self) -> u64;
    fn open(&self) -> BoxFuture<'static, std::io::Result<BoxReader>>;
}

#[derive(Clone)]
pub enum Input {
    File(PathBuf),
    Stream {
        opener: Arc<dyn Opener>,
        /// The runtime the opener's readers need (librqbit streams wake on
        /// tokio timers and notifies).
        runtime: tokio::runtime::Handle,
        /// For logs and errors.
        name: String,
    },
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Input::File(p) => write!(f, "File({})", p.display()),
            Input::Stream { name, .. } => write!(f, "Stream({name})"),
        }
    }
}

impl Input {
    /// A stream input bound to the current tokio runtime. Panics outside one.
    pub fn stream(opener: Arc<dyn Opener>, name: impl Into<String>) -> Self {
        Input::Stream {
            opener,
            runtime: tokio::runtime::Handle::current(),
            name: name.into(),
        }
    }

    pub(crate) fn source_element(&self) -> Result<gst::Element> {
        match self {
            Input::File(path) => gst::ElementFactory::make("filesrc")
                .property("location", path.to_string_lossy().as_ref())
                .build()
                .map_err(|_| Error::MissingElement("filesrc".into())),
            Input::Stream {
                opener, runtime, ..
            } => {
                let src = gst::glib::Object::new::<StreamSrc>();
                src.imp().configure(opener.clone(), runtime.clone());
                Ok(src.upcast())
            }
        }
    }

    /// An async reader over the input, for code that parses the container
    /// itself (the keyframe index).
    pub(crate) async fn open_reader(&self) -> Result<(BoxReader, u64)> {
        match self {
            Input::File(path) => {
                let file = tokio::fs::File::open(path).await?;
                let len = file.metadata().await?.len();
                Ok((Box::pin(file), len))
            }
            Input::Stream { opener, .. } => Ok((opener.open().await?, opener.size())),
        }
    }
}

gst::glib::wrapper! {
    pub struct StreamSrc(ObjectSubclass<imp::StreamSrc>)
        @extends gst_base::BaseSrc, gst::Element, gst::Object;
}

mod imp {
    use std::sync::{Arc, LazyLock, Mutex};

    use gst::glib;
    use gst::subclass::prelude::*;
    use gst_base::prelude::BaseSrcExt;
    use gst_base::subclass::base_src::CreateSuccess;
    use gst_base::subclass::prelude::*;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    use tokio_util::sync::CancellationToken;

    use super::{BoxReader, Opener};

    static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
        gst::DebugCategory::new(
            "cinemastreamsrc",
            gst::DebugColorFlags::empty(),
            Some("Async reader source"),
        )
    });

    struct Reader {
        inner: BoxReader,
        position: u64,
    }

    #[derive(Default)]
    pub struct StreamSrc {
        source: Mutex<Option<(Arc<dyn Opener>, tokio::runtime::Handle)>>,
        reader: tokio::sync::Mutex<Option<Reader>>,
        /// Fired by `unlock` (flushing seek, state change to READY) so a read
        /// blocked on a missing piece gives up instead of wedging the
        /// pipeline.
        cancel: Mutex<CancellationToken>,
    }

    impl StreamSrc {
        pub(super) fn configure(&self, opener: Arc<dyn Opener>, runtime: tokio::runtime::Handle) {
            *self.source.lock().unwrap() = Some((opener, runtime));
        }

        fn runtime(&self) -> Option<tokio::runtime::Handle> {
            self.source
                .lock()
                .unwrap()
                .as_ref()
                .map(|(_, rt)| rt.clone())
        }

        fn opener(&self) -> Option<Arc<dyn Opener>> {
            self.source.lock().unwrap().as_ref().map(|(o, _)| o.clone())
        }

        /// Reads `length` bytes at `offset`, or fewer at end of stream.
        async fn read_at(&self, offset: u64, length: usize) -> std::io::Result<Vec<u8>> {
            let mut guard = self.reader.lock().await;
            if guard.is_none() {
                let opener = self
                    .opener()
                    .ok_or_else(|| std::io::Error::other("source not configured"))?;
                *guard = Some(Reader {
                    inner: opener.open().await?,
                    position: 0,
                });
            }
            let reader = guard.as_mut().unwrap();
            if reader.position != offset {
                // Poisoned until the seek lands, so a seek cancelled halfway
                // is redone on the next read.
                reader.position = u64::MAX;
                reader
                    .inner
                    .as_mut()
                    .seek(std::io::SeekFrom::Start(offset))
                    .await?;
                reader.position = offset;
            }
            let mut data = vec![0u8; length];
            let mut filled = 0;
            while filled < length {
                let n = reader.inner.as_mut().read(&mut data[filled..]).await?;
                if n == 0 {
                    break;
                }
                filled += n;
                reader.position += n as u64;
            }
            data.truncate(filled);
            Ok(data)
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StreamSrc {
        const NAME: &'static str = "CinemaStreamSrc";
        type Type = super::StreamSrc;
        type ParentType = gst_base::BaseSrc;
    }

    impl ObjectImpl for StreamSrc {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_format(gst::Format::Bytes);
        }
    }

    impl GstObjectImpl for StreamSrc {}

    impl ElementImpl for StreamSrc {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "Cinema stream source",
                    "Source/File",
                    "Reads from an async, seekable reader",
                    "cinema",
                )
            });
            Some(&*META)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                vec![
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &gst::Caps::new_any(),
                    )
                    .unwrap(),
                ]
            });
            TEMPLATES.as_ref()
        }
    }

    impl BaseSrcImpl for StreamSrc {
        fn is_seekable(&self) -> bool {
            true
        }

        fn size(&self) -> Option<u64> {
            self.opener().map(|o| o.size())
        }

        fn stop(&self) -> Result<(), gst::ErrorMessage> {
            // Dropped lazily: `try_lock` keeps a state change from blocking on
            // a read that `unlock` has already cancelled.
            if let Ok(mut reader) = self.reader.try_lock() {
                *reader = None;
            }
            Ok(())
        }

        fn unlock(&self) -> Result<(), gst::ErrorMessage> {
            self.cancel.lock().unwrap().cancel();
            Ok(())
        }

        fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
            *self.cancel.lock().unwrap() = CancellationToken::new();
            Ok(())
        }

        fn create(
            &self,
            offset: u64,
            _buffer: Option<&mut gst::BufferRef>,
            length: u32,
        ) -> Result<CreateSuccess, gst::FlowError> {
            let Some(runtime) = self.runtime() else {
                return Err(gst::FlowError::NotNegotiated);
            };
            let size = self.size().unwrap_or(u64::MAX);
            if offset >= size {
                return Err(gst::FlowError::Eos);
            }
            let length = (length as u64).min(size - offset) as usize;
            let cancel = self.cancel.lock().unwrap().clone();

            // Streaming threads are GStreamer's, never tokio's, so blocking on
            // the runtime here is safe.
            let result = runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    r = self.read_at(offset, length) => Some(r),
                }
            });

            match result {
                None => Err(gst::FlowError::Flushing),
                Some(Ok(data)) if data.is_empty() => Err(gst::FlowError::Eos),
                Some(Ok(data)) => {
                    let mut buffer = gst::Buffer::from_mut_slice(data);
                    buffer.get_mut().unwrap().set_offset(offset);
                    Ok(CreateSuccess::NewBuffer(buffer))
                }
                Some(Err(err)) => {
                    gst::error!(CAT, imp = self, "Read at {offset} failed: {err}");
                    gst::element_imp_error!(
                        self,
                        gst::ResourceError::Read,
                        ["Read at {} failed: {}", offset, err]
                    );
                    Err(gst::FlowError::Error)
                }
            }
        }
    }
}
