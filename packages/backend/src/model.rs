use std::{
    fmt,
    ops::Deref,
    sync::Arc,
};

use serde::{
    Deserialize,
    Serialize,
};

#[derive(Debug, Clone, PartialEq)]
pub struct SharedMetadata(pub Arc<MetadataPayload>);

impl Deref for SharedMetadata {
    type Target = MetadataPayload;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<MetadataPayload> for SharedMetadata {
    fn as_ref(&self) -> &MetadataPayload {
        &self.0
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "payload")]
pub enum AppMessage {
    UpdateMetadata(MetadataUpdate),
    UpdateAudioInfo(AudioInfoPayload),

    UpdatePlayState(PlayStatePayload),
    UpdateTimeline(TimelinePayload),
    UpdatePlayMode(PlayModePayload),

    EnableSmtc,
    DisableSmtc,

    EnableDiscord,
    DisableDiscord,
    DiscordConfig(DiscordConfigPayload),

    Shutdown,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct CoverPayload {
    /// 封面地址。只有在二进制通道没送成时才会用到 (后端直接按这个地址取图)
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtistPayload {
    pub name: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub trans_name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SongKind {
    Song,
    Podcast,
    Local,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataPayload {
    pub song_name: String,
    pub author_name: String,
    pub album_name: String,
    pub cover: Option<CoverPayload>,
    pub ncm_id: Option<u64>,
    pub duration: Option<f64>,

    /// 结构化的艺术家列表, 为空时退回 `author_name`
    #[serde(default)]
    pub artists: Vec<ArtistPayload>,
    #[serde(default)]
    pub album_id: Option<u64>,
    #[serde(default)]
    pub trans_name: Option<String>,
    /// 只有 `Song` 的 ID 才是曲库 ID, 播客和本地歌曲的 ID 不能用来拼链接
    #[serde(default)]
    pub kind: Option<SongKind>,
}

/// 当前音频流的真实规格, 缺失的字段一律不展示
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfoPayload {
    /// 这份规格属于哪首歌, 与当前元数据的 `ncm_id` 不一致时不展示
    pub ncm_id: u64,
    #[serde(default)]
    pub codec: Option<String>,
    /// 单位 bit/s
    #[serde(default)]
    pub bitrate: Option<u32>,
    /// 单位 Hz
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub bit_depth: Option<u8>,
    /// 网易云实际下发的音质档位代码, 例如 "lossless"
    #[serde(default)]
    pub level: Option<String>,
}

/// 一次元数据更新命令
///
/// 除了前端送来的元数据, 它还带着封面二进制数据: 前端用
/// `inflink.dispatchWithArrayBuffer` 在同一次调用里把字节和命令一起交过来,
/// `dispatcher::send_command` 会把字节挂到这个字段上 (详见 `array_buffer` 模块),
/// 让它跟着命令一起跨线程送到 dispatcher。
#[derive(Deserialize, Serialize, Clone, PartialEq)]
pub struct MetadataUpdate {
    #[serde(flatten)]
    pub payload: MetadataPayload,

    /// 封面原始字节, 只由后端内部填充, 前端不会传
    #[serde(default, skip_serializing)]
    pub cover_bytes: Option<Vec<u8>>,
}

impl fmt::Debug for MetadataUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MetadataUpdate")
            .field("payload", &self.payload)
            .field("cover_bytes", &self.cover_bytes.as_ref().map(Vec::len))
            .finish()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum RepeatMode {
    None,
    Track,
    List,
    AI,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayStatePayload {
    pub status: PlaybackStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePayload {
    pub current_time: f64,
    pub total_time: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlayModePayload {
    pub is_shuffling: bool,
    pub repeat_mode: RepeatMode,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiscordConfigPayload {
    pub show_when_paused: bool,
    pub display_mode: Option<DiscordDisplayMode>,
    #[serde(default)]
    pub app_name_mode: DiscordAppNameMode,
    #[serde(default)]
    pub third_line: DiscordThirdLine,
    #[serde(default)]
    pub artist_separator: DiscordArtistSeparator,
    #[serde(default)]
    pub show_translation: bool,
    #[serde(default = "default_true")]
    pub links: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordThirdLine {
    #[default]
    Album,
    /// `Lossless`
    Tier,
    /// `Lossless · 专辑名`
    TierAndAlbum,
    /// `Lossless · FLAC 48 kHz, 1104 kbps`
    Full,
    /// `Lossless · FLAC 48k, 1104k`
    Compact,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordArtistSeparator {
    #[default]
    Comma,
    Slash,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", content = "value")]
pub enum DiscordAppNameMode {
    #[default]
    Default,
    DefaultEn,
    Song,
    Artist,
    Album,
    Custom(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum DiscordDisplayMode {
    Name,    // Listening to Spotify
    State,   // Listening to Rick Astley
    Details, // Listening to Never Gonna Give You Up
}

#[derive(Serialize, Debug)]
pub enum CommandStatus {
    Success,
    Error,
}

#[derive(Serialize, Debug)]
pub struct CommandResult {
    pub status: CommandStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
