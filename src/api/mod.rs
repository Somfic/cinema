pub mod collections;
pub mod downloads;
pub mod hls;
pub mod media;
pub mod remote;
pub mod search;
pub mod settings;
pub mod streams;
pub mod subtitles;
pub mod trailer;
pub mod urls;
pub mod watch;

// draad 0.2 emits `use crate::api::<module>::*` for every module that hosts
// `#[ty]` wire types, not just the `#[api]`/`#[events]` ones. The `tmdb` and
// `torrent` wire types live at the crate root, so re-export those modules here
// to make `crate::api::tmdb` / `crate::api::torrent` resolve.
pub(crate) use crate::{tmdb, torrent};
