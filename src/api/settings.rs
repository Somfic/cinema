use crate::app::{AppContext, CinemaError};

#[draad::ty]
pub struct YoutubeCookiesStatus {
    /// `"none"`, `"file"`, or `"env"` — which cookies source yt-dlp is using.
    pub source: String,
    /// Size of the stored cookies file (only set when `source == "file"`).
    pub size: Option<i64>,
    /// Path the env override points at (only set when `source == "env"`).
    pub env_path: Option<String>,
}

#[draad::api(namespace = "settings")]
pub trait SettingsApi {
    /// Returns the active yt-dlp cookies source.
    #[get]
    async fn youtube_cookies_status(&self) -> Result<YoutubeCookiesStatus, CinemaError>;

    /// Stores a Netscape-format `cookies.txt` body in the data dir so yt-dlp
    /// can use it on subsequent trailer requests.
    #[put]
    async fn set_youtube_cookies(
        &self,
        content: String,
    ) -> Result<YoutubeCookiesStatus, CinemaError>;

    /// Removes the stored cookies file (the env override, if any, still wins).
    #[delete]
    async fn clear_youtube_cookies(&self) -> Result<YoutubeCookiesStatus, CinemaError>;
}

const MAX_COOKIES_BYTES: usize = 1024 * 1024;

async fn current_status(ctx: &AppContext) -> YoutubeCookiesStatus {
    if let Some(path) = crate::trailer::cookies_env_override() {
        return YoutubeCookiesStatus {
            source: "env".into(),
            size: None,
            env_path: Some(path),
        };
    }
    match tokio::fs::metadata(crate::trailer::cookies_storage_path(&ctx.storage)).await {
        Ok(m) if m.is_file() => YoutubeCookiesStatus {
            source: "file".into(),
            size: Some(m.len() as i64),
            env_path: None,
        },
        _ => YoutubeCookiesStatus {
            source: "none".into(),
            size: None,
            env_path: None,
        },
    }
}

#[draad::api]
impl SettingsApi for AppContext {
    async fn youtube_cookies_status(&self) -> Result<YoutubeCookiesStatus, CinemaError> {
        Ok(current_status(self).await)
    }

    async fn set_youtube_cookies(
        &self,
        content: String,
    ) -> Result<YoutubeCookiesStatus, CinemaError> {
        if content.len() > MAX_COOKIES_BYTES {
            return Err(CinemaError::Generic(
                "cookies file too large (max 1 MiB)".into(),
            ));
        }
        let head = &content[..content.len().min(256)];
        if !head.contains("# Netscape HTTP Cookie File") && !head.contains("# HTTP Cookie File") {
            return Err(CinemaError::Generic(
                "expected a Netscape-format cookies.txt (header line missing)".into(),
            ));
        }
        let path = crate::trailer::cookies_storage_path(&self.storage);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content.as_bytes()).await?;
        Ok(current_status(self).await)
    }

    async fn clear_youtube_cookies(&self) -> Result<YoutubeCookiesStatus, CinemaError> {
        let path = crate::trailer::cookies_storage_path(&self.storage);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(current_status(self).await)
    }
}
