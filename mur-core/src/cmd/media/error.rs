//! Typed media errors with warm, actionable, locale-aware user messages.

use std::fmt;

/// All recoverable media failures. `user_message()` is what the agent relays.
#[derive(Debug, Clone, PartialEq, Eq)]
// VlcNotFound / VlcHttpDown / SnapshotFailed are constructed only by Plan B (proactive
// co-watching); keep them in the Plan A foundation without a dead_code warning.
#[allow(dead_code)]
pub enum MediaError {
    VlcNotFound,
    VlcHttpDown,
    DrmProtected,
    NoTranscript,
    ModelOffline,
    SnapshotFailed,
    SourceUnresolvable,
    YtdlpMissing,
}

impl MediaError {
    /// Warm zh-TW message + actionable hint. (zh-TW is the product default brand voice.)
    // Reached only via `Display`/Plan-B co-watching, both of which are dead in
    // the `mur` bin target (variants are constructed only by proactive
    // co-watching) — same rationale as the enum's `#[allow(dead_code)]` above.
    #[allow(dead_code)]
    pub fn user_message(&self) -> &'static str {
        match self {
            MediaError::VlcNotFound => "我找不到 VLC，請先安裝 VLC.app 再試一次。",
            MediaError::VlcHttpDown => "VLC 沒有回應，請確認它正在執行。",
            MediaError::DrmProtected => "這是有 DRM 保護的串流，我沒辦法擷取畫面或字幕喔。",
            MediaError::NoTranscript => "這支影片找不到字幕，所以我沒辦法做文字分析。",
            MediaError::ModelOffline => "本地模型還沒就緒（MuR Hub 有啟動嗎？）。",
            MediaError::SnapshotFailed => "我擷取畫面失敗了，稍後再試一次。",
            MediaError::SourceUnresolvable => "我解析不了這個來源，請確認連結或檔案路徑。",
            MediaError::YtdlpMissing => "要分析 YouTube 影片需要 yt-dlp，安裝後再試一次。",
        }
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.user_message())
    }
}

impl std::error::Error for MediaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_nonempty_message() {
        let all = [
            MediaError::VlcNotFound,
            MediaError::VlcHttpDown,
            MediaError::DrmProtected,
            MediaError::NoTranscript,
            MediaError::ModelOffline,
            MediaError::SnapshotFailed,
            MediaError::SourceUnresolvable,
            MediaError::YtdlpMissing,
        ];
        for e in all {
            assert!(!e.user_message().is_empty(), "empty message for {e:?}");
            assert_eq!(format!("{e}"), e.user_message());
        }
    }
}
