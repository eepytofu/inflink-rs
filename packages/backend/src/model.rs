use std::{
    fmt,
    time::{
        Instant,
        SystemTime,
        UNIX_EPOCH,
    },
};

use serde::{
    Deserialize,
    Serialize,
};

/// 一条消息被观察到的时刻
///
/// 在渲染线程收到命令时就取, 而不是等后台线程处理到它时再取: 进度锚点要的是
/// "这个位置是什么时候的位置", 排队等待的时间不能算进去。
#[derive(Debug, Clone, Copy)]
pub struct Stamp {
    pub mono: Instant,
    /// Unix 毫秒
    pub wall_ms: i64,
}

impl Stamp {
    pub fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "payload")]
pub enum AppMessage {
    UpdateTrack(Box<TrackUpdate>),
    UpdateCover(CoverUpdate),
    UpdateAudioInfo(AudioInfoPayload),
    ProbeAudioHeader(AudioHeaderRequest),

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
    /// 这份规格属于哪一次播放, 见 [`TrackUpdate::seq`]
    #[serde(default)]
    pub seq: u64,
    /// 这份规格描述的是哪一个音频流, 见 [`AudioHeaderRequest::stream`]
    #[serde(default)]
    pub stream: u64,
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

/// 切歌瞬间的完整快照
///
/// 文字、规格、播放状态和进度在同一条命令里到达, 不等封面, 这样卡片不会出现
/// "新歌的进度配旧歌的标题" 这类中间状态。
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackUpdate {
    /// 前端每换一首歌就加一。A→B→A 里的两次 A 序号不同, 歌曲 ID 做不到这一点
    pub seq: u64,
    pub metadata: MetadataPayload,
    #[serde(default)]
    pub audio: Option<AudioInfoPayload>,
    /// 规格还在路上。此时音质行留空, 而不是先拿专辑名顶替
    #[serde(default)]
    pub audio_pending: bool,
    pub status: PlaybackStatus,
    #[serde(default)]
    pub position_ms: f64,
}

/// 封面下载完成后单独送来, 只给 SMTC 用 (Discord 自己按 URL 取图)
///
/// 字节由前端用 `inflink.dispatchWithArrayBuffer` 在同一次调用里交过来,
/// `dispatcher::send_command` 会把它挂到 `cover_bytes` 上 (详见 `array_buffer` 模块)。
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct CoverUpdate {
    pub seq: u64,
    /// 没有字节 (下载失败或超时) 时, 后端直接按这个地址取图
    #[serde(default)]
    pub url: Option<String>,

    /// 封面原始字节, 只由后端内部填充, 前端不会传
    #[serde(default, skip_serializing)]
    pub cover_bytes: Option<Vec<u8>>,
}

impl fmt::Debug for CoverUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoverUpdate")
            .field("seq", &self.seq)
            .field("url", &self.url)
            .field("cover_bytes", &self.cover_bytes.as_ref().map(Vec::len))
            .finish()
    }
}

/// 从缓存文件头读到的规格
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeaderSpecs {
    pub stream: u64,
    pub sample_rate: u32,
    pub bit_depth: Option<u8>,
}

/// 请求从网易云的缓存文件头读取采样率和位深
#[derive(Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioHeaderRequest {
    pub seq: u64,
    /// 前端给每个音频流 (歌曲加 MD5) 编的号, 同一首歌换音质时会变。
    /// 读到的结果凭它认领对应的那份规格, 0 表示没有编号
    #[serde(default)]
    pub stream: u64,
    pub ncm_id: u64,
    /// 音频流的 MD5, 缓存文件名里带着它, 用来认准是哪一个文件
    pub md5: String,
    #[serde(default)]
    pub duration_ms: Option<f64>,
}

impl fmt::Debug for AudioHeaderRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // MD5 不进日志
        f.debug_struct("AudioHeaderRequest")
            .field("seq", &self.seq)
            .field("stream", &self.stream)
            .field("ncm_id", &self.ncm_id)
            .field("duration_ms", &self.duration_ms)
            .finish_non_exhaustive()
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
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub reason: TimelineReason,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TimelineReason {
    /// 播放器例行上报的进度
    #[default]
    Progress,
    /// 用户主动跳转, 哪怕只跳了一点点也要反映出来
    Seek,
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
