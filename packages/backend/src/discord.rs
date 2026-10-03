use std::{
    sync::{
        LazyLock,
        Mutex,
        mpsc::{
            self,
            Receiver,
            Sender,
        },
    },
    thread,
    time::{
        Duration,
        SystemTime,
        UNIX_EPOCH,
    },
};

use discord_rich_presence::{
    DiscordIpc,
    DiscordIpcClient,
    activity::{
        Activity,
        ActivityType,
        Assets,
        Button,
        StatusDisplayType,
        Timestamps,
    },
};
use tracing::{
    debug,
    info,
    warn,
};

use crate::model::{
    AudioInfoPayload,
    DiscordAppNameMode,
    DiscordArtistSeparator,
    DiscordConfigPayload,
    DiscordDisplayMode,
    DiscordThirdLine,
    MetadataPayload,
    PlayStatePayload,
    PlaybackStatus,
    SharedMetadata,
    SongKind,
    TimelinePayload,
};

const APP_ID: &str = "1427186361827594375";
const NCM_ICON_ASSET_KEY: &str = "ncm_icon";
// 应用自带的 ncm_icon 素材图标占满了整个方形, 被 Discord 裁成圆形后会缺角,
// 所以小图标改用仓库里一张四周留有透明边距的图
const NCM_SMALL_ICON_URL: &str =
    "https://raw.githubusercontent.com/eepytofu/inflink-rs/main/assets/netease.png";
const NCM_HOME_URL: &str = "https://music.163.com/";
const APP_NAME_ZH: &str = "网易云音乐";
const APP_NAME_EN: &str = "NetEase CloudMusic";

// Discord 会拒绝整个载荷而不是截断超长字段, 太短的字段同样会被拒绝
const FIELD_MAX_CHARS: usize = 128;
const FIELD_MIN_CHARS: usize = 2;

// 主要用来应对跳转进度的更新
const TIMESTAMP_UPDATE_THRESHOLD_MS: i64 = 100;
const RECONNECT_COOLDOWN_SECONDS: u8 = 5;

enum RpcMessage {
    Metadata(SharedMetadata),
    PlayState(PlayStatePayload),
    Timeline(TimelinePayload),
    AudioInfo(AudioInfoPayload),
    Enable,
    Disable,
    Config(DiscordConfigPayload),
}

static SENDER: LazyLock<Mutex<Option<Sender<RpcMessage>>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, PartialEq, Eq)]
struct CardOptions {
    app_name_mode: DiscordAppNameMode,
    third_line: DiscordThirdLine,
    artist_separator: DiscordArtistSeparator,
    show_translation: bool,
    links: bool,
}

impl Default for CardOptions {
    fn default() -> Self {
        Self {
            app_name_mode: DiscordAppNameMode::Default,
            third_line: DiscordThirdLine::Album,
            artist_separator: DiscordArtistSeparator::Comma,
            show_translation: false,
            links: true,
        }
    }
}

/// 卡片上所有文字和链接, 在元数据、音频规格或配置变化时算一次
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    app_name: String,
    details: String,
    state: String,
    third_line: Option<String>,
    cover: String,
    song_url: Option<String>,
    artist_url: Option<String>,
    album_url: Option<String>,
}

fn clip(text: &str) -> String {
    if text.chars().count() <= FIELD_MAX_CHARS {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(FIELD_MAX_CHARS - 1).collect();
    clipped.push('\u{2026}');
    clipped
}

fn long_enough(text: &str) -> bool {
    text.trim().chars().count() >= FIELD_MIN_CHARS
}

fn with_translation(name: &str, translation: Option<&str>, show: bool) -> String {
    match translation.map(str::trim) {
        Some(t) if show && !t.is_empty() && t != name => format!("{name} ({t})"),
        _ => name.to_string(),
    }
}

fn format_artists(
    metadata: &MetadataPayload,
    separator: DiscordArtistSeparator,
    show_translation: bool,
) -> String {
    if metadata.artists.is_empty() {
        return metadata.author_name.clone();
    }
    let separator = match separator {
        DiscordArtistSeparator::Comma => ", ",
        DiscordArtistSeparator::Slash => " / ",
    };
    metadata
        .artists
        .iter()
        .map(|a| with_translation(&a.name, a.trans_name.as_deref(), show_translation))
        .collect::<Vec<_>>()
        .join(separator)
}

/// 44100 -> "44.1", 48000 -> "48"
fn format_khz(hz: u32) -> String {
    let (whole, rest) = (hz / 1000, hz % 1000);
    if rest == 0 {
        return whole.to_string();
    }
    format!("{whole}.{rest:03}")
        .trim_end_matches('0')
        .to_string()
}

/// 网易云下发的音质档位代码对应的名字, 代码取自客户端自己的档位表
///
/// 名字沿用网易云音质选择面板里的英文。`jyeffect` 和 `jymaster` 实测下发的就是
/// 无损档的文件, 由客户端在本地处理, 所以它们的名字必须显示出来, 不能只留下数字
fn tier_label(level: &str) -> Option<&'static str> {
    Some(match level {
        "standard" => "Standard",
        "higher" => "Higher",
        "exhigh" => "HQ",
        "lossless" => "Lossless",
        "hires" => "Hi-Res",
        "jyeffect" => "Spatial Audio",
        "jymaster" => "Master",
        "sky" => "Surround Audio",
        "vivid" => "Audio Vivid",
        "dolby" => "Dolby",
        _ => return None,
    })
}

fn codec_name(codec: &str) -> String {
    // m4a 只是容器, 里面装的是 AAC
    match codec.trim().to_lowercase().as_str() {
        "m4a" => "AAC".to_string(),
        other => other.to_uppercase(),
    }
}

fn audio_codec(audio: &AudioInfoPayload) -> Option<String> {
    audio
        .codec
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .map(codec_name)
}

/// 音质档位的名字, 认不出档位时用编码格式顶上
fn format_audio_tier(audio: &AudioInfoPayload) -> Option<String> {
    audio
        .level
        .as_deref()
        .and_then(tier_label)
        .map(str::to_string)
        .or_else(|| audio_codec(audio))
}

/// 形如 `FLAC 48 kHz, 1104 kbps` (紧凑时是 `FLAC 48k, 1104k`), 只拼接确实拿到的部分
fn format_audio_specs(audio: &AudioInfoPayload, compact: bool) -> Option<String> {
    let (khz, kbps, bits) = if compact {
        ("k", "k", "")
    } else {
        (" kHz", " kbps", "-bit")
    };

    let bit_depth = audio.bit_depth.filter(|b| *b > 0);
    let sample_rate = audio.sample_rate.filter(|s| *s > 0);
    // 写法跟 Apple Music 的 `ALAC 24-bit/48 kHz` 一致: 编码格式和采样率连在一起
    let sample = match (bit_depth, sample_rate) {
        (Some(depth), Some(hz)) => Some(format!("{depth}{bits}/{}{khz}", format_khz(hz))),
        (None, Some(hz)) => Some(format!("{}{khz}", format_khz(hz))),
        (Some(depth), None) => Some(format!("{depth}-bit")),
        (None, None) => None,
    };
    let format = [audio_codec(audio), sample]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let bitrate = audio
        .bitrate
        .filter(|b| *b > 0)
        .map(|b| format!("{}{kbps}", (b + 500) / 1000));

    let parts: Vec<String> = [Some(format).filter(|f| !f.is_empty()), bitrate]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// 形如 `Lossless · FLAC 48 kHz, 1104 kbps`
fn format_audio_line(audio: &AudioInfoPayload, compact: bool) -> Option<String> {
    let tier = audio
        .level
        .as_deref()
        .and_then(tier_label)
        .map(str::to_string);
    let parts: Vec<String> = [tier, format_audio_specs(audio, compact)]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn catalog_url(path: &str, id: Option<u64>) -> Option<String> {
    id.filter(|id| *id > 0)
        .map(|id| format!("https://music.163.com/{path}?id={id}"))
}

fn process_cover_url(original_url: Option<&str>) -> String {
    original_url.map_or_else(
        || NCM_ICON_ASSET_KEY.to_string(),
        |url| {
            let url = url.replace("http://", "https://");
            let base_url = url.split('?').next().unwrap_or(&url);
            format!("{base_url}?imageView&enlarge=1&type=jpeg&quality=90&thumbnail=150y150")
        },
    )
}

fn resolve_app_name(mode: &DiscordAppNameMode, metadata: &MetadataPayload) -> String {
    let name = match mode {
        DiscordAppNameMode::Default => APP_NAME_ZH,
        DiscordAppNameMode::DefaultEn => APP_NAME_EN,
        DiscordAppNameMode::Song => &metadata.song_name,
        DiscordAppNameMode::Artist => &metadata.author_name,
        DiscordAppNameMode::Album => &metadata.album_name,
        DiscordAppNameMode::Custom(text) => text,
    };
    if long_enough(name) {
        clip(name)
    } else {
        APP_NAME_ZH.to_string()
    }
}

fn build_card(
    metadata: &MetadataPayload,
    audio: Option<&AudioInfoPayload>,
    options: &CardOptions,
) -> Card {
    // 预加载或切歌途中, 规格可能还是上一首歌的
    let audio = audio.filter(|a| metadata.ncm_id == Some(a.ncm_id));
    let album = || metadata.album_name.clone();
    // 读不到规格时 (v2 客户端、本地歌曲、播客) 一律退回专辑名
    let third_line = match options.third_line {
        DiscordThirdLine::Album => album(),
        DiscordThirdLine::Tier => audio.and_then(format_audio_tier).unwrap_or_else(album),
        // 档位放前面: 一行放不下时被截掉的是专辑名
        DiscordThirdLine::TierAndAlbum => match audio.and_then(format_audio_tier) {
            Some(tier) if long_enough(&metadata.album_name) => {
                format!("{tier} · {}", metadata.album_name)
            }
            Some(tier) => tier,
            None => album(),
        },
        DiscordThirdLine::Full => audio
            .and_then(|a| format_audio_line(a, false))
            .unwrap_or_else(album),
        DiscordThirdLine::Compact => audio
            .and_then(|a| format_audio_line(a, true))
            .unwrap_or_else(album),
    };

    // 旧版前端不带 kind, 当时所有 ID 都被当成歌曲 ID
    let in_catalog = matches!(metadata.kind, None | Some(SongKind::Song));
    let link = |url: Option<String>| url.filter(|_| options.links && in_catalog);

    Card {
        app_name: resolve_app_name(&options.app_name_mode, metadata),
        details: clip(&with_translation(
            &metadata.song_name,
            metadata.trans_name.as_deref(),
            options.show_translation,
        )),
        state: clip(&format_artists(
            metadata,
            options.artist_separator,
            options.show_translation,
        )),
        third_line: Some(clip(&third_line)).filter(|t| long_enough(t)),
        cover: process_cover_url(metadata.cover.as_ref().and_then(|c| c.url.as_deref())),
        song_url: link(catalog_url("song", metadata.ncm_id)),
        artist_url: link(catalog_url(
            "artist",
            metadata.artists.first().and_then(|a| a.id),
        )),
        album_url: link(catalog_url("album", metadata.album_id)),
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ActivityData {
    metadata: SharedMetadata,
    status: PlaybackStatus,
    current_time: f64,
    card: Card,
}

impl ActivityData {
    fn from_metadata(
        metadata: SharedMetadata,
        audio: Option<&AudioInfoPayload>,
        options: &CardOptions,
    ) -> Self {
        let card = build_card(&metadata, audio, options);
        Self {
            metadata,
            status: PlaybackStatus::Paused,
            current_time: 0.0,
            card,
        }
    }

    fn update_metadata(
        &mut self,
        metadata: SharedMetadata,
        audio: Option<&AudioInfoPayload>,
        options: &CardOptions,
    ) {
        self.card = build_card(&metadata, audio, options);
        self.metadata = metadata;
        self.current_time = 0.0;
    }

    fn refresh_card(&mut self, audio: Option<&AudioInfoPayload>, options: &CardOptions) {
        self.card = build_card(&self.metadata, audio, options);
    }
}

#[derive(Debug)]
struct RpcWorker {
    client: Option<DiscordIpcClient>,
    data: Option<ActivityData>,
    is_enabled: bool,
    connect_retry_count: u8,
    // 上次发送的结束时间戳
    // 用于防抖，也用于判断是否要清除 Activity
    last_sent_end_timestamp: Option<i64>,
    show_when_paused: bool,
    display_mode: DiscordDisplayMode,
    options: CardOptions,
    // 规格可能早于元数据到达 (元数据要等封面), 所以独立于 data 保存
    audio: Option<AudioInfoPayload>,
}

impl Default for RpcWorker {
    fn default() -> Self {
        Self {
            client: None,
            data: None,
            is_enabled: false,
            connect_retry_count: 0,
            last_sent_end_timestamp: None,
            show_when_paused: false,
            display_mode: DiscordDisplayMode::Name,
            options: CardOptions::default(),
            audio: None,
        }
    }
}

impl RpcWorker {
    fn handle_message(&mut self, msg: RpcMessage) {
        match msg {
            RpcMessage::Enable => {
                info!("启用 Discord RPC");
                self.is_enabled = true;
                self.connect_retry_count = 0;
            }
            RpcMessage::Disable => {
                info!("禁用 Discord RPC");
                self.is_enabled = false;
                self.disconnect();
            }
            RpcMessage::Config(payload) => {
                info!(
                    show_when_paused = ?payload.show_when_paused,
                    display_mode = ?payload.display_mode,
                    app_name_mode = ?payload.app_name_mode,
                    third_line = ?payload.third_line,
                    artist_separator = ?payload.artist_separator,
                    show_translation = payload.show_translation,
                    links = payload.links,
                    "更新 Discord 配置",
                );
                self.show_when_paused = payload.show_when_paused;
                self.options = CardOptions {
                    app_name_mode: payload.app_name_mode,
                    third_line: payload.third_line,
                    artist_separator: payload.artist_separator,
                    show_translation: payload.show_translation,
                    links: payload.links,
                };

                if let Some(mode) = payload.display_mode {
                    self.display_mode = mode;
                }

                if let Some(data) = &mut self.data {
                    data.refresh_card(self.audio.as_ref(), &self.options);
                }

                self.last_sent_end_timestamp = None;
            }
            RpcMessage::Metadata(payload) => {
                let audio = self.audio.as_ref();
                let new_data = match self.data.take() {
                    Some(mut d) => {
                        d.update_metadata(payload, audio, &self.options);
                        d
                    }
                    None => ActivityData::from_metadata(payload, audio, &self.options),
                };
                self.data = Some(new_data);
                self.last_sent_end_timestamp = None;
            }
            RpcMessage::AudioInfo(payload) => {
                debug!(?payload, "更新音频规格");
                self.audio = Some(payload);
                // 只换第三行, 进度和封面都不动; 清掉防抖才能在同一首歌内刷新卡片
                if let Some(data) = &mut self.data {
                    let old_card = data.card.clone();
                    data.refresh_card(self.audio.as_ref(), &self.options);
                    if data.card != old_card {
                        self.last_sent_end_timestamp = None;
                    }
                }
            }
            RpcMessage::PlayState(payload) => {
                if let Some(data) = &mut self.data {
                    if payload.status == PlaybackStatus::Playing
                        && data.status != PlaybackStatus::Playing
                    {
                        self.last_sent_end_timestamp = None;
                    }
                    data.status = payload.status;
                }
            }
            RpcMessage::Timeline(payload) => {
                if let Some(data) = &mut self.data {
                    data.current_time = payload.current_time;
                }
            }
        }
    }

    fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = client.clear_activity();
            let _ = client.close();
        }
        self.last_sent_end_timestamp = None;
    }

    fn connect(&mut self) {
        if self.connect_retry_count > 0 {
            self.connect_retry_count -= 1;
            return;
        }

        let mut client = DiscordIpcClient::new(APP_ID);
        match client.connect() {
            Ok(()) => {
                info!("Discord IPC 已连接");
                self.client = Some(client);
                self.last_sent_end_timestamp = None;
            }
            Err(e) => {
                info!("连接 Discord IPC 失败: {e:?}. Discord 可能未运行");
                self.connect_retry_count = RECONNECT_COOLDOWN_SECONDS;
            }
        }
    }

    fn sync_discord(&mut self) {
        if !self.is_enabled {
            if self.client.is_some() {
                self.disconnect();
            }
            return;
        }

        if self.data.is_none() {
            if let Some(client) = &mut self.client {
                let _ = client.clear_activity();
                self.last_sent_end_timestamp = None;
            }
            return;
        }

        if self.client.is_none() {
            self.connect();
        }

        if let (Some(client), Some(data)) = (&mut self.client, &self.data) {
            let success = Self::perform_update(
                client,
                data,
                &mut self.last_sent_end_timestamp,
                self.show_when_paused,
                &self.display_mode,
            );
            if !success {
                self.disconnect();
            }
        }
    }

    fn build_base_activity<'a>(
        data: &'a ActivityData,
        display_mode: &DiscordDisplayMode,
    ) -> Activity<'a> {
        let card = &data.card;

        let mut assets = Assets::new()
            .large_image(card.cover.as_str())
            .small_image(NCM_SMALL_ICON_URL)
            .small_text(card.app_name.as_str())
            .small_url(NCM_HOME_URL);
        // Listening 类型的卡片把 large_text 画成第三行, 它和封面共用 large_url
        if let Some(third_line) = &card.third_line {
            assets = assets.large_text(third_line.as_str());
        }
        if let Some(url) = &card.album_url {
            assets = assets.large_url(url.as_str());
        }

        let buttons = vec![Button::new(
            "🎧 Listen",
            card.song_url.as_deref().unwrap_or(NCM_HOME_URL),
        )];

        let status_type = match display_mode {
            DiscordDisplayMode::Name => StatusDisplayType::Name,
            DiscordDisplayMode::State => StatusDisplayType::State,
            DiscordDisplayMode::Details => StatusDisplayType::Details,
        };

        let mut activity = Activity::new()
            .name(card.app_name.as_str())
            .details(card.details.as_str())
            .state(card.state.as_str())
            .activity_type(ActivityType::Listening)
            .assets(assets)
            .buttons(buttons)
            .status_display_type(status_type);

        if let Some(url) = &card.song_url {
            activity = activity.details_url(url.as_str());
        }
        if let Some(url) = &card.artist_url {
            activity = activity.state_url(url.as_str());
        }

        activity
    }

    fn calc_paused_timestamps(current_time: f64, duration: f64) -> (i64, i64) {
        // 来自 https://musicpresence.app/ 的 hack，通过将
        // 开始和结束时间戳向后平移一年以实现在暂停时进度静止的效果
        const ONE_YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let current_progress_ms = current_time as i64;
        let future_start = (now_ms - current_progress_ms) + ONE_YEAR_MS;
        let future_end = future_start + (duration as i64);

        (future_start, future_end)
    }

    fn calc_playing_timestamps(current_time: f64, duration: f64) -> (i64, i64) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let duration_ms = duration as i64;
        let current_time_ms = current_time as i64;
        let remaining_ms = (duration_ms - current_time_ms).max(0);

        let end = now_ms + remaining_ms;
        let start = end - duration_ms;

        (start, end)
    }

    fn perform_update(
        client: &mut DiscordIpcClient,
        data: &ActivityData,
        last_sent_end_timestamp: &mut Option<i64>,
        show_when_paused: bool,
        display_mode: &DiscordDisplayMode,
    ) -> bool {
        let mut activity = Self::build_base_activity(data, display_mode);
        let mut new_end_timestamp = None;
        let should_send;

        match data.status {
            PlaybackStatus::Paused => {
                if !show_when_paused {
                    debug!("播放暂停且配置为隐藏，清除 Activity");
                    if let Err(e) = client.clear_activity() {
                        warn!("清除 Discord Activity 失败: {e:?}");
                        return false;
                    }
                    *last_sent_end_timestamp = None;
                    return true;
                }

                if let Some(duration) = data.metadata.duration
                    && duration > 0.0
                {
                    let (start, end) = Self::calc_paused_timestamps(data.current_time, duration);

                    debug!(future_start = start, future_end = end, "应用 hack 时间戳");

                    activity = activity.timestamps(Timestamps::new().start(start).end(end));
                }

                should_send = true;
                *last_sent_end_timestamp = None;
            }
            PlaybackStatus::Playing => {
                if let Some(duration) = data.metadata.duration
                    && duration > 0.0
                {
                    let (start, end) = Self::calc_playing_timestamps(data.current_time, duration);

                    // 频繁调用 Discord RPC 接口会导致限流，所以在跳转发生时再更新时间戳
                    if let Some(last_end) = last_sent_end_timestamp {
                        let diff = (*last_end - end).abs();
                        if diff < TIMESTAMP_UPDATE_THRESHOLD_MS {
                            return true;
                        }
                        debug!(
                            diff_ms = diff,
                            threshold_ms = TIMESTAMP_UPDATE_THRESHOLD_MS,
                            "进度变更超过阈值，触发更新"
                        );
                    }

                    activity = activity.timestamps(Timestamps::new().start(start).end(end));
                    new_end_timestamp = Some(end);
                    should_send = true;
                } else {
                    should_send = last_sent_end_timestamp.is_some();
                    if should_send {
                        warn!("没有时长，清除时间戳");
                    }
                }
            }
        }

        if should_send {
            debug!(
                song = %data.metadata.song_name,
                state = ?data.status,
                "更新 Discord Activity"
            );

            if let Err(e) = client.set_activity(activity) {
                warn!("设置 Discord Activity 失败: {e:?}, 尝试重连");
                return false;
            }
        }

        if new_end_timestamp.is_some() {
            *last_sent_end_timestamp = new_end_timestamp;
        } else if matches!(data.status, PlaybackStatus::Playing) && data.metadata.duration.is_none()
        {
            *last_sent_end_timestamp = None;
        }

        true
    }
}

impl Drop for RpcWorker {
    fn drop(&mut self) {
        if let Some(mut client) = self.client.take() {
            let _ = client.clear_activity();
            let _ = client.close();
        }
    }
}

fn background_loop(rx: &Receiver<RpcMessage>) {
    let mut worker = RpcWorker::default();

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(msg) => {
                worker.handle_message(msg);
                worker.sync_discord();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if worker.client.is_none() {
                    worker.sync_discord();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

pub fn init() {
    let (tx, rx) = mpsc::channel();
    if let Ok(mut guard) = SENDER.lock() {
        *guard = Some(tx);
    }
    thread::spawn(move || {
        background_loop(&rx);
    });
}

fn send(msg: RpcMessage) {
    if let Ok(guard) = SENDER.lock()
        && let Some(tx) = guard.as_ref()
        && let Err(e) = tx.send(msg)
    {
        warn!("向 Discord RPC 线程发送消息失败: {e}");
    }
}

pub fn enable() {
    send(RpcMessage::Enable);
}
pub fn disable() {
    send(RpcMessage::Disable);
}
pub fn update_config(payload: DiscordConfigPayload) {
    send(RpcMessage::Config(payload));
}
pub fn update_metadata(payload: SharedMetadata) {
    send(RpcMessage::Metadata(payload));
}
pub fn update_play_state(payload: PlayStatePayload) {
    send(RpcMessage::PlayState(payload));
}
pub fn update_timeline(payload: TimelinePayload) {
    send(RpcMessage::Timeline(payload));
}
pub fn update_audio_info(payload: AudioInfoPayload) {
    send(RpcMessage::AudioInfo(payload));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::ArtistPayload;

    fn artist(name: &str, id: Option<u64>, trans_name: Option<&str>) -> ArtistPayload {
        ArtistPayload {
            name: name.to_string(),
            id,
            trans_name: trans_name.map(str::to_string),
        }
    }

    fn metadata() -> MetadataPayload {
        MetadataPayload {
            song_name: "刹那芳华".to_string(),
            author_name: "哔哩哔哩拜年纪 / 洛天依Official / 乐正绫 / 裘丹莉".to_string(),
            album_name: "2026哔哩哔哩拜年纪".to_string(),
            cover: None,
            ncm_id: Some(3_348_915_450),
            duration: Some(258_586.0),
            artists: vec![
                artist("哔哩哔哩拜年纪", Some(47_090_969), None),
                artist("洛天依Official", Some(906_118), None),
                artist("乐正绫", Some(1_102_240), None),
                artist("裘丹莉", Some(52_437_191), None),
            ],
            album_id: Some(361_770_580),
            trans_name: None,
            kind: Some(SongKind::Song),
        }
    }

    fn audio(
        codec: Option<&str>,
        bitrate: Option<u32>,
        sample_rate: Option<u32>,
        bit_depth: Option<u8>,
    ) -> AudioInfoPayload {
        AudioInfoPayload {
            ncm_id: 3_348_915_450,
            codec: codec.map(str::to_string),
            bitrate,
            sample_rate,
            bit_depth,
            level: None,
        }
    }

    fn tiered(level: &str, mut spec: AudioInfoPayload) -> AudioInfoPayload {
        spec.level = Some(level.to_string());
        spec
    }

    fn third_line(choice: DiscordThirdLine) -> CardOptions {
        CardOptions {
            third_line: choice,
            ..CardOptions::default()
        }
    }

    fn audio_options() -> CardOptions {
        third_line(DiscordThirdLine::Full)
    }

    /// 用户在设置里把第三行换成音质之后的 worker
    fn audio_worker() -> RpcWorker {
        let mut worker = RpcWorker::default();
        worker.handle_message(RpcMessage::Config(DiscordConfigPayload {
            show_when_paused: false,
            display_mode: None,
            app_name_mode: DiscordAppNameMode::Default,
            third_line: DiscordThirdLine::Full,
            artist_separator: DiscordArtistSeparator::Comma,
            show_translation: false,
            links: true,
        }));
        worker
    }

    fn links(card: Card) -> (Option<String>, Option<String>, Option<String>) {
        (card.song_url, card.artist_url, card.album_url)
    }

    #[test]
    fn sample_rates_read_like_a_player_shows_them() {
        assert_eq!(format_khz(44_100), "44.1");
        assert_eq!(format_khz(48_000), "48");
        assert_eq!(format_khz(88_200), "88.2");
        assert_eq!(format_khz(96_000), "96");
        assert_eq!(format_khz(192_000), "192");
        assert_eq!(format_khz(22_050), "22.05");
    }

    #[test]
    fn audio_line_shows_only_what_is_known() {
        let line = |a| format_audio_line(&a, false);
        assert_eq!(
            line(audio(Some("flac"), Some(1_596_360), Some(44_100), None)).as_deref(),
            Some("FLAC 44.1 kHz, 1596 kbps")
        );
        assert_eq!(
            line(audio(Some("flac"), Some(1_869_617), None, None)).as_deref(),
            Some("FLAC, 1870 kbps")
        );
        assert_eq!(
            line(audio(Some("flac"), Some(985_000), Some(44_100), Some(16))).as_deref(),
            Some("FLAC 16-bit/44.1 kHz, 985 kbps")
        );
        assert_eq!(
            line(audio(None, Some(320_000), None, None)).as_deref(),
            Some("320 kbps")
        );
        assert_eq!(
            line(audio(Some("flac"), None, None, Some(24))).as_deref(),
            Some("FLAC 24-bit")
        );
        assert_eq!(line(audio(None, None, None, None)), None);
        assert_eq!(line(audio(Some(" "), Some(0), Some(0), Some(0))), None);
    }

    #[test]
    fn compact_audio_line_shortens_every_unit_to_k() {
        let line = |a| format_audio_line(&a, true);
        assert_eq!(
            line(audio(Some("flac"), Some(1_596_360), Some(44_100), None)).as_deref(),
            Some("FLAC 44.1k, 1596k")
        );
        assert_eq!(
            line(audio(Some("flac"), Some(985_000), Some(44_100), Some(16))).as_deref(),
            Some("FLAC 16/44.1k, 985k")
        );
        assert_eq!(
            line(audio(None, Some(320_000), None, None)).as_deref(),
            Some("320k")
        );
        assert_eq!(line(audio(None, None, None, None)), None);
    }

    /// 数值全部来自实机: 同一首歌在各个档位下网易云实际下发的音频流
    #[test]
    fn tiers_read_the_way_netease_names_them() {
        let lossless_file = || audio(Some("flac"), Some(1_103_664), Some(48_000), None);
        let cases = [
            (
                tiered(
                    "standard",
                    audio(Some("m4a"), Some(96_007), Some(48_000), None),
                ),
                "Standard · AAC 48 kHz, 96 kbps",
                "Standard · AAC 48k, 96k",
                "Standard",
            ),
            (
                tiered(
                    "exhigh",
                    audio(Some("m4a"), Some(256_016), Some(48_000), None),
                ),
                "HQ · AAC 48 kHz, 256 kbps",
                "HQ · AAC 48k, 256k",
                "HQ",
            ),
            (
                tiered(
                    "exhigh",
                    audio(Some("mp3"), Some(320_000), Some(44_100), None),
                ),
                "HQ · MP3 44.1 kHz, 320 kbps",
                "HQ · MP3 44.1k, 320k",
                "HQ",
            ),
            (
                tiered("lossless", lossless_file()),
                "Lossless · FLAC 48 kHz, 1104 kbps",
                "Lossless · FLAC 48k, 1104k",
                "Lossless",
            ),
            (
                tiered(
                    "hires",
                    audio(Some("flac"), Some(1_869_617), Some(48_000), None),
                ),
                "Hi-Res · FLAC 48 kHz, 1870 kbps",
                "Hi-Res · FLAC 48k, 1870k",
                "Hi-Res",
            ),
            // 这两个档位下发的就是无损档的文件
            (
                tiered("jyeffect", lossless_file()),
                "Spatial Audio · FLAC 48 kHz, 1104 kbps",
                "Spatial Audio · FLAC 48k, 1104k",
                "Spatial Audio",
            ),
            (
                tiered("jymaster", lossless_file()),
                "Master · FLAC 48 kHz, 1104 kbps",
                "Master · FLAC 48k, 1104k",
                "Master",
            ),
        ];

        for (spec, full, compact, tier) in cases {
            assert_eq!(format_audio_line(&spec, false).as_deref(), Some(full));
            assert_eq!(format_audio_line(&spec, true).as_deref(), Some(compact));
            assert_eq!(format_audio_tier(&spec).as_deref(), Some(tier));
            // 紧凑写法必须能放进卡片的一行 (大约 37 个字符)
            assert!(compact.chars().count() <= 37, "{compact}");
        }
    }

    #[test]
    fn missing_numbers_and_unknown_tiers_degrade_gracefully() {
        // 从磁盘缓存恢复的条目没有采样率
        let cached = tiered("hires", audio(Some("flac"), Some(1_869_617), None, None));
        assert_eq!(
            format_audio_line(&cached, true).as_deref(),
            Some("Hi-Res · FLAC, 1870k")
        );

        // 没见过的档位代码不猜名字: 完整写法里只剩规格, 只显示档位时用编码格式顶上
        let unknown = tiered(
            "future",
            audio(Some("flac"), Some(1_103_664), Some(48_000), None),
        );
        assert_eq!(
            format_audio_line(&unknown, true).as_deref(),
            Some("FLAC 48k, 1104k")
        );
        assert_eq!(format_audio_tier(&unknown).as_deref(), Some("FLAC"));
        assert_eq!(format_audio_tier(&audio(None, None, None, None)), None);
    }

    #[test]
    fn third_line_choices() {
        let spec = tiered(
            "lossless",
            audio(Some("flac"), Some(1_103_664), Some(48_000), None),
        );
        let line = |choice, audio: Option<&AudioInfoPayload>| {
            build_card(&metadata(), audio, &third_line(choice)).third_line
        };
        let album = "2026哔哩哔哩拜年纪";

        let cases = [
            (DiscordThirdLine::Album, album.to_string()),
            (DiscordThirdLine::Tier, "Lossless".to_string()),
            (
                DiscordThirdLine::TierAndAlbum,
                format!("Lossless · {album}"),
            ),
            (
                DiscordThirdLine::Full,
                "Lossless · FLAC 48 kHz, 1104 kbps".to_string(),
            ),
            (
                DiscordThirdLine::Compact,
                "Lossless · FLAC 48k, 1104k".to_string(),
            ),
        ];
        for (choice, expected) in cases {
            assert_eq!(line(choice, Some(&spec)), Some(expected), "{choice:?}");
            // 读不到规格时, 每一种选择都退回专辑名
            assert_eq!(line(choice, None).as_deref(), Some(album), "{choice:?}");
        }
    }

    #[test]
    fn every_artist_is_kept_and_the_first_one_is_linked() {
        let card = build_card(&metadata(), None, &CardOptions::default());
        assert_eq!(card.state, "哔哩哔哩拜年纪, 洛天依Official, 乐正绫, 裘丹莉");
        assert_eq!(
            card.artist_url.as_deref(),
            Some("https://music.163.com/artist?id=47090969")
        );
        assert_eq!(
            card.song_url.as_deref(),
            Some("https://music.163.com/song?id=3348915450")
        );
        assert_eq!(
            card.album_url.as_deref(),
            Some("https://music.163.com/album?id=361770580")
        );

        let slash = CardOptions {
            artist_separator: DiscordArtistSeparator::Slash,
            ..CardOptions::default()
        };
        assert_eq!(
            build_card(&metadata(), None, &slash).state,
            "哔哩哔哩拜年纪 / 洛天依Official / 乐正绫 / 裘丹莉"
        );
    }

    #[test]
    fn flat_author_name_is_used_when_no_structured_artists_exist() {
        let mut meta = metadata();
        meta.artists.clear();
        let card = build_card(&meta, None, &CardOptions::default());
        assert_eq!(card.state, meta.author_name);
        assert_eq!(card.artist_url, None);
    }

    #[test]
    fn third_line_is_the_album_unless_audio_info_is_chosen() {
        let spec = audio(Some("flac"), Some(1_869_617), None, None);

        let card = build_card(&metadata(), Some(&spec), &CardOptions::default());
        assert_eq!(card.third_line.as_deref(), Some("2026哔哩哔哩拜年纪"));

        let card = build_card(&metadata(), Some(&spec), &audio_options());
        assert_eq!(card.third_line.as_deref(), Some("FLAC, 1870 kbps"));

        // 选了音频信息但读不到规格时, 退回专辑名
        let card = build_card(&metadata(), None, &audio_options());
        assert_eq!(card.third_line.as_deref(), Some("2026哔哩哔哩拜年纪"));
    }

    #[test]
    fn another_songs_audio_info_is_never_shown() {
        let mut stale = audio(Some("flac"), Some(806_663), Some(44_100), None);
        stale.ncm_id = 1_234_853;
        let card = build_card(&metadata(), Some(&stale), &audio_options());
        assert_eq!(card.third_line.as_deref(), Some("2026哔哩哔哩拜年纪"));
    }

    #[test]
    fn translations_are_opt_in_and_skip_duplicates() {
        let mut meta = metadata();
        meta.song_name = "石火".to_string();
        meta.trans_name = Some("Stonefire".to_string());
        meta.artists = vec![
            artist("澤野弘之", Some(1), Some("Hiroyuki Sawano")),
            artist("Aimer", Some(2), Some("Aimer")),
            artist("mizuki", Some(3), Some("  ")),
        ];

        let card = build_card(&meta, None, &CardOptions::default());
        assert_eq!(card.details, "石火");
        assert_eq!(card.state, "澤野弘之, Aimer, mizuki");

        let shown = CardOptions {
            show_translation: true,
            ..CardOptions::default()
        };
        let card = build_card(&meta, None, &shown);
        assert_eq!(card.details, "石火 (Stonefire)");
        assert_eq!(card.state, "澤野弘之 (Hiroyuki Sawano), Aimer, mizuki");
    }

    #[test]
    fn links_need_catalog_ids() {
        let options = CardOptions::default();

        let mut local = metadata();
        local.kind = Some(SongKind::Local);
        local.ncm_id = Some(0);
        assert_eq!(
            links(build_card(&local, None, &options)),
            (None, None, None)
        );

        // 播客的 ID 不是歌曲 ID
        let mut podcast = metadata();
        podcast.kind = Some(SongKind::Podcast);
        assert_eq!(
            links(build_card(&podcast, None, &options)),
            (None, None, None)
        );

        let mut partial = metadata();
        partial.album_id = None;
        partial.artists[0].id = None;
        let card = build_card(&partial, None, &options);
        assert!(card.song_url.is_some());
        assert_eq!((card.artist_url, card.album_url), (None, None));

        let off = CardOptions {
            links: false,
            ..CardOptions::default()
        };
        assert_eq!(
            links(build_card(&metadata(), None, &off)),
            (None, None, None)
        );
    }

    #[test]
    fn app_name_follows_the_chosen_mode() {
        let meta = metadata();
        let name = |mode| resolve_app_name(&mode, &meta);
        assert_eq!(name(DiscordAppNameMode::Default), "网易云音乐");
        assert_eq!(name(DiscordAppNameMode::DefaultEn), "NetEase CloudMusic");
        assert_eq!(name(DiscordAppNameMode::Song), "刹那芳华");
        assert_eq!(
            name(DiscordAppNameMode::Custom("网易云音乐".into())),
            "网易云音乐"
        );
        assert_eq!(
            name(DiscordAppNameMode::Custom(String::new())),
            "网易云音乐"
        );
    }

    #[test]
    fn long_and_short_fields_stay_inside_discords_limits() {
        let mut meta = metadata();
        meta.song_name = "长".repeat(200);
        meta.album_name = "A".to_string();
        let card = build_card(&meta, None, &CardOptions::default());
        assert_eq!(card.details.chars().count(), FIELD_MAX_CHARS);
        assert!(card.details.ends_with('\u{2026}'));
        assert_eq!(card.third_line, None);
    }

    #[test]
    fn audio_info_refreshes_the_card_without_touching_progress() {
        let mut worker = audio_worker();
        worker.handle_message(RpcMessage::Metadata(SharedMetadata(Arc::new(metadata()))));
        worker.handle_message(RpcMessage::PlayState(PlayStatePayload {
            status: PlaybackStatus::Playing,
        }));
        worker.handle_message(RpcMessage::Timeline(TimelinePayload {
            current_time: 42_000.0,
            total_time: 258_586.0,
        }));
        worker.last_sent_end_timestamp = Some(1);

        let spec = audio(Some("flac"), Some(1_869_617), None, None);
        worker.handle_message(RpcMessage::AudioInfo(spec.clone()));

        let data = worker.data.as_ref().unwrap();
        assert_eq!(data.card.third_line.as_deref(), Some("FLAC, 1870 kbps"));
        assert!((data.current_time - 42_000.0).abs() < f64::EPSILON);
        assert_eq!(data.status, PlaybackStatus::Playing);
        assert_eq!(worker.last_sent_end_timestamp, None);

        // 同样的规格再来一次不应该打断防抖
        worker.last_sent_end_timestamp = Some(1);
        worker.handle_message(RpcMessage::AudioInfo(spec));
        assert_eq!(worker.last_sent_end_timestamp, Some(1));
    }

    #[test]
    fn audio_info_that_arrives_before_metadata_is_kept() {
        let mut worker = audio_worker();
        worker.handle_message(RpcMessage::AudioInfo(audio(
            Some("flac"),
            Some(1_596_360),
            Some(44_100),
            None,
        )));
        worker.handle_message(RpcMessage::Metadata(SharedMetadata(Arc::new(metadata()))));
        assert_eq!(
            worker.data.as_ref().unwrap().card.third_line.as_deref(),
            Some("FLAC 44.1 kHz, 1596 kbps")
        );
    }
}
