use std::{
    collections::VecDeque,
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
        Instant,
    },
};

use discord_rich_presence::activity::{
    Activity,
    ActivityType,
    Assets,
    Button,
    StatusDisplayType,
    Timestamps,
};
use serde_json::{
    Value,
    json,
};
use tracing::{
    debug,
    info,
    warn,
};

use crate::{
    discord_ipc::{
        Connector,
        OP_CLOSE,
        OP_FRAME,
        OP_PING,
        OP_PONG,
        PipeConnector,
        Transport,
    },
    discord_policy::SendPolicy,
    model::{
        AudioHeaderSpecs,
        AudioInfoPayload,
        DiscordAppNameMode,
        DiscordArtistSeparator,
        DiscordConfigPayload,
        DiscordDisplayMode,
        DiscordThirdLine,
        MetadataPayload,
        PlayStatePayload,
        PlaybackStatus,
        SongKind,
        Stamp,
        TimelinePayload,
        TimelineReason,
        TrackUpdate,
    },
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

// 例行上报的进度和按锚点推算的位置相差超过这个值, 才认为播放位置真的变了。
// 用户主动跳转不受它限制
const DRIFT_TOLERANCE_MS: f64 = 1500.0;
const TRACK_SETTLE: Duration = Duration::from_millis(100);
const PAUSE_SETTLE: Duration = Duration::from_millis(800);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const REPLY_DEADLINE: Duration = Duration::from_secs(10);
// 没有事情要做时也要定期醒来, 读走 Discord 的回复和心跳
const IDLE_TICK: Duration = Duration::from_secs(1);
const RETRY_TICK: Duration = Duration::from_millis(50);

enum RpcMessage {
    Track(Box<TrackUpdate>),
    PlayState(PlayStatePayload),
    Timeline(TimelinePayload),
    AudioInfo(AudioInfoPayload),
    AudioHeader(AudioHeaderSpecs),
    Enable,
    Disable,
    Config(DiscordConfigPayload),
}

type Envelope = (RpcMessage, Stamp);

static SENDER: LazyLock<Mutex<Option<Sender<Envelope>>>> = LazyLock::new(|| Mutex::new(None));

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

fn format_artists(metadata: &MetadataPayload, separator: DiscordArtistSeparator) -> String {
    if metadata.artists.is_empty() {
        return metadata.author_name.clone();
    }
    let separator = match separator {
        DiscordArtistSeparator::Comma => ", ",
        DiscordArtistSeparator::Slash => " / ",
    };
    // Artist names stay as NetEase lists them: the player never carries a translation for them
    metadata
        .artists
        .iter()
        .map(|a| a.name.as_str())
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

/// `24-bit/48 kHz`, 写法跟 Apple Music 的 `ALAC 24-bit/48 kHz` 一致
fn format_sample(audio: &AudioInfoPayload) -> Option<String> {
    let bit_depth = audio.bit_depth.filter(|b| *b > 0);
    let sample_rate = audio.sample_rate.filter(|s| *s > 0);
    match (bit_depth, sample_rate) {
        (Some(depth), Some(hz)) => Some(format!("{depth}-bit/{} kHz", format_khz(hz))),
        (None, Some(hz)) => Some(format!("{} kHz", format_khz(hz))),
        (Some(depth), None) => Some(format!("{depth}-bit")),
        (None, None) => None,
    }
}

fn format_bitrate(audio: &AudioInfoPayload) -> Option<String> {
    audio
        .bitrate
        .filter(|b| *b > 0)
        .map(|b| format!("{} kbps", (b + 500) / 1000))
}

/// 无损格式的质量看位深和采样率, 有损格式的质量看码率
fn is_lossless(audio: &AudioInfoPayload) -> bool {
    audio.bit_depth.is_some_and(|b| b > 0)
        || audio.codec.as_deref().is_some_and(|codec| {
            matches!(
                codec.trim().to_lowercase().as_str(),
                "flac" | "alac" | "wav" | "ape"
            )
        })
}

/// 拿到的每一项都写出来: `FLAC 24-bit/48 kHz, 1695 kbps`、`AAC 48 kHz, 256 kbps`
///
/// 不带网易云的音质档位名字 (Lossless、Hi-Res、Standard 之类): 有了位深和采样率之后
/// 它没有再多说明什么, `Standard` 这样的名字离开网易云也没人知道指的是什么。
fn format_audio_full(audio: &AudioInfoPayload) -> Option<String> {
    let format = [audio_codec(audio), format_sample(audio)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");

    let parts: Vec<String> = [
        Some(format).filter(|f| !f.is_empty()),
        format_bitrate(audio),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// 只写最能说明质量的那一项: `FLAC 24-bit/48 kHz`、`AAC 256 kbps`
///
/// 那一项缺失时用另一项顶上, 例如读不到文件头的无损文件写成 `FLAC 1695 kbps`。
fn format_audio_compact(audio: &AudioInfoPayload) -> Option<String> {
    let (sample, bitrate) = (format_sample(audio), format_bitrate(audio));
    let quality = if is_lossless(audio) {
        sample.or(bitrate)
    } else {
        bitrate.or(sample)
    };

    let parts: Vec<String> = [audio_codec(audio), quality]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
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

#[cfg(test)]
fn build_card(
    metadata: &MetadataPayload,
    audio: Option<&AudioInfoPayload>,
    options: &CardOptions,
) -> Card {
    build_card_with(metadata, audio, false, options)
}

/// `audio_pending`: 规格还在路上。这时音质行留空, 等规格到了再补上;
/// 先显示专辑名再换成音质, 看起来就是卡片在闪
fn build_card_with(
    metadata: &MetadataPayload,
    audio: Option<&AudioInfoPayload>,
    audio_pending: bool,
    options: &CardOptions,
) -> Card {
    // 预加载或切歌途中, 规格可能还是上一首歌的
    let audio = audio.filter(|a| metadata.ncm_id == Some(a.ncm_id));
    // 旧版前端不带 kind, 当时所有 ID 都被当成歌曲 ID
    let in_catalog = matches!(metadata.kind, None | Some(SongKind::Song));
    let waiting = audio.is_none() && audio_pending && in_catalog;
    let album_name = with_translation(
        &metadata.album_name,
        metadata.album_trans_name.as_deref(),
        options.show_translation,
    );
    let album = || {
        if waiting {
            String::new()
        } else {
            album_name.clone()
        }
    };
    // 确定读不到规格时 (v2 客户端、本地歌曲、播客) 一律退回专辑名
    let third_line = match options.third_line {
        DiscordThirdLine::Album => album_name.clone(),
        // 音质放前面: 一行放不下时被截掉的是专辑名
        DiscordThirdLine::QualityAndAlbum => match audio.and_then(format_audio_compact) {
            Some(quality) if long_enough(&metadata.album_name) => {
                format!("{quality} · {album_name}")
            }
            Some(quality) => quality,
            None => album_name.clone(),
        },
        DiscordThirdLine::Full => audio.and_then(format_audio_full).unwrap_or_else(album),
        DiscordThirdLine::Compact => audio.and_then(format_audio_compact).unwrap_or_else(album),
    };

    let link = |url: Option<String>| url.filter(|_| options.links && in_catalog);

    Card {
        app_name: resolve_app_name(&options.app_name_mode, metadata),
        details: clip(&with_translation(
            &metadata.song_name,
            metadata.trans_name.as_deref(),
            options.show_translation,
        )),
        state: clip(&format_artists(metadata, options.artist_separator)),
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

/// 进度锚点: 某个时刻的播放位置
///
/// 卡片上的时间戳只由锚点算出, 不看 "现在"。这样无关的消息 (规格到了、配置变了)
/// 算出来的时间戳和上次完全一样, 不会因为调度的抖动而重发。
#[derive(Debug, Clone, Copy)]
struct Anchor {
    position_ms: f64,
    at: Stamp,
}

#[derive(Debug, Clone)]
struct Playback {
    seq: u64,
    metadata: MetadataPayload,
    audio: Option<AudioInfoPayload>,
    audio_pending: bool,
    status: PlaybackStatus,
    anchor: Anchor,
    // v2 客户端的时长在元数据之后才随进度到达
    timeline_duration: Option<f64>,
}

impl Playback {
    fn position_at(&self, now: Instant) -> f64 {
        match self.status {
            PlaybackStatus::Playing => now
                .saturating_duration_since(self.anchor.at.mono)
                .as_secs_f64()
                .mul_add(1000.0, self.anchor.position_ms),
            PlaybackStatus::Paused => self.anchor.position_ms,
        }
    }

    fn duration(&self) -> Option<f64> {
        self.metadata
            .duration
            .filter(|d| *d > 0.0)
            .or_else(|| self.timeline_duration.filter(|d| *d > 0.0))
    }

    fn timestamps(&self) -> Option<(i64, i64)> {
        // 来自 https://musicpresence.app/ 的 hack，通过将
        // 开始和结束时间戳向后平移一年以实现在暂停时进度静止的效果
        const ONE_YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

        let duration = self.duration()?;
        let shift = match self.status {
            PlaybackStatus::Playing => 0,
            PlaybackStatus::Paused => ONE_YEAR_MS,
        };
        let start = self.anchor.at.wall_ms - self.anchor.position_ms as i64 + shift;
        Some((start, start + duration as i64))
    }
}

/// 一张完整的卡片。两张卡片相等就不需要再写一次
#[derive(Debug, Clone, PartialEq, Eq)]
struct CardState {
    card: Card,
    display_mode: DiscordDisplayMode,
    timestamps: Option<(i64, i64)>,
}

/// 希望 Discord 显示的东西
#[derive(Debug, Clone, PartialEq, Eq)]
enum Desired {
    Clear,
    Card(Box<CardState>),
}

fn build_activity(state: &CardState) -> Activity<'_> {
    let card = &state.card;

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

    let status_type = match state.display_mode {
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
    if let Some((start, end)) = state.timestamps {
        activity = activity.timestamps(Timestamps::new().start(start).end(end));
    }

    activity
}

/// 一次已经写出、还没等到回复的更新
#[derive(Debug)]
struct Awaiting {
    nonce: String,
    at: Instant,
    seq: Option<u64>,
    desired: Desired,
}

struct RpcWorker<C: Connector> {
    connector: C,
    transport: Option<C::Transport>,
    is_enabled: bool,
    show_when_paused: bool,
    display_mode: DiscordDisplayMode,
    options: CardOptions,
    playback: Option<Playback>,
    // 规格可能早于它所属的那首歌的快照到达, 这时先存着, 不去动正在显示的卡片
    pending_audio: Option<AudioInfoPayload>,
    // 后端自己从缓存文件头读到的规格, 比前端转一圈送回来的早几百毫秒
    header: Option<AudioHeaderSpecs>,
    // 这条连接上 Discord 现在拿着什么。`None` 表示不确定 (更新被拒绝之后)
    last_written: Option<Desired>,
    // 被 Discord 拒绝的那张卡片, 内容不变就不再重发
    rejected: Option<Desired>,
    hold_until: Option<Instant>,
    policy: SendPolicy,
    awaiting: VecDeque<Awaiting>,
    next_connect_at: Option<Instant>,
    nonce_counter: u64,
}

impl<C: Connector> RpcWorker<C> {
    fn new(connector: C) -> Self {
        Self {
            connector,
            transport: None,
            is_enabled: false,
            show_when_paused: false,
            display_mode: DiscordDisplayMode::Name,
            options: CardOptions::default(),
            playback: None,
            pending_audio: None,
            header: None,
            last_written: None,
            rejected: None,
            hold_until: None,
            policy: SendPolicy::default(),
            awaiting: VecDeque::new(),
            next_connect_at: None,
            nonce_counter: 0,
        }
    }

    fn handle_message(&mut self, msg: RpcMessage, at: Stamp) {
        match msg {
            RpcMessage::Enable => {
                info!("启用 Discord RPC");
                self.is_enabled = true;
                self.next_connect_at = None;
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
            }
            RpcMessage::Track(update) => self.handle_track(*update, at),
            RpcMessage::AudioHeader(specs) => {
                if specs.stream != 0 {
                    self.header = Some(specs);
                }
            }
            RpcMessage::AudioInfo(payload) => {
                debug!(?payload, "更新音频规格");
                match &mut self.playback {
                    Some(playback) if playback.seq == payload.seq => {
                        playback.audio = Some(payload);
                        playback.audio_pending = false;
                    }
                    Some(playback) if playback.seq > payload.seq => {}
                    _ => self.pending_audio = Some(payload),
                }
            }
            RpcMessage::PlayState(payload) => {
                if let Some(playback) = &mut self.playback
                    && playback.status != payload.status
                {
                    // 换状态时把位置定下来: 暂停后不再前进, 恢复后从这里继续
                    playback.anchor = Anchor {
                        position_ms: playback.position_at(at.mono),
                        at,
                    };
                    playback.status = payload.status;
                    self.hold_until = match payload.status {
                        // 一首歌自然结束时会先报一次暂停, 紧接着就是下一首歌。
                        // 等一小会儿再写, 免得为这个瞬间花掉一次写入, 让新歌多等一个间隔
                        PlaybackStatus::Paused => Some(at.mono + PAUSE_SETTLE),
                        PlaybackStatus::Playing => None,
                    };
                }
            }
            RpcMessage::Timeline(payload) => {
                let Some(playback) = &mut self.playback else {
                    return;
                };
                if payload.seq != 0 && payload.seq != playback.seq {
                    return;
                }
                if payload.total_time > 0.0 {
                    playback.timeline_duration = Some(payload.total_time);
                }

                let moved = match payload.reason {
                    TimelineReason::Seek => true,
                    // 例行进度只在和预期对不上时才算数 (卡顿、没有上报的跳转)
                    TimelineReason::Progress => {
                        (payload.current_time - playback.position_at(at.mono)).abs()
                            > DRIFT_TOLERANCE_MS
                    }
                };
                if moved {
                    debug!(
                        seq = playback.seq,
                        reason = ?payload.reason,
                        position_ms = payload.current_time,
                        "重新确定进度锚点"
                    );
                    playback.anchor = Anchor {
                        position_ms: payload.current_time,
                        at,
                    };
                }
            }
        }
    }

    fn handle_track(&mut self, update: TrackUpdate, at: Stamp) {
        if let Some(playback) = &mut self.playback
            && playback.seq == update.seq
        {
            // 同一次播放的补充 (例如封面地址变了): 进度和状态都不动
            playback.metadata = update.metadata;
            if update.audio.is_some() {
                playback.audio = update.audio;
            }
            playback.audio_pending = update.audio_pending && playback.audio.is_none();
            return;
        }

        let audio = update.audio.or_else(|| {
            self.pending_audio
                .take()
                .filter(|audio| audio.seq == update.seq)
        });
        self.pending_audio = None;

        // 从缓存文件头读到的规格通常紧跟着就到, 等它一下, 第一张卡片就是完整的。
        // 快照到达时还处在暂停状态的话 (上一首歌刚自然结束), 按暂停的等待时间来
        let settle = match update.status {
            PlaybackStatus::Playing => TRACK_SETTLE,
            PlaybackStatus::Paused => PAUSE_SETTLE,
        };
        self.hold_until = self.hold_until.max(Some(at.mono + settle));

        debug!(
            seq = update.seq,
            stage = "track_received",
            "收到新的曲目快照"
        );
        self.playback = Some(Playback {
            seq: update.seq,
            audio_pending: update.audio_pending && audio.is_none(),
            audio,
            metadata: update.metadata,
            status: update.status,
            anchor: Anchor {
                position_ms: update.position_ms,
                at,
            },
            timeline_duration: None,
        });
    }

    fn desired(&self) -> Desired {
        let Some(playback) = &self.playback else {
            return Desired::Clear;
        };
        if playback.status == PlaybackStatus::Paused && !self.show_when_paused {
            return Desired::Clear;
        }

        Desired::Card(Box::new(CardState {
            card: build_card_with(
                &playback.metadata,
                self.audio_with_header(playback).as_ref(),
                playback.audio_pending,
                &self.options,
            ),
            display_mode: self.display_mode.clone(),
            timestamps: playback.timestamps(),
        }))
    }

    /// 规格里缺的采样率和位深, 用同一个音频流的文件头补上
    fn audio_with_header(&self, playback: &Playback) -> Option<AudioInfoPayload> {
        let mut audio = playback.audio.clone()?;
        if let Some(header) = self.header
            && header.stream == audio.stream
        {
            audio.sample_rate = audio.sample_rate.or(Some(header.sample_rate));
            audio.bit_depth = audio.bit_depth.or(header.bit_depth);
        }
        Some(audio)
    }

    fn drop_connection(&mut self) {
        self.transport = None;
        self.last_written = None;
        self.rejected = None;
        self.awaiting.clear();
    }

    fn disconnect(&mut self) {
        if let Some(mut transport) = self.transport.take() {
            let nonce = self.next_nonce();
            let _ = transport.write_frame(OP_FRAME, &activity_frame(&Desired::Clear, &nonce));
            let _ = transport.write_frame(OP_CLOSE, &json!({}));
        }
        self.drop_connection();
    }

    fn next_nonce(&mut self) -> String {
        self.nonce_counter += 1;
        format!("inflink-{}-{}", std::process::id(), self.nonce_counter)
    }

    /// 读走 Discord 的回复: 确认、拒绝、心跳和关闭
    fn drain_replies(&mut self, now: Instant) {
        let Some(transport) = &mut self.transport else {
            return;
        };

        let frames = match transport.read_frames() {
            Ok(frames) => frames,
            Err(e) => {
                warn!("读取 Discord IPC 失败: {e}, 尝试重连");
                self.drop_connection();
                self.next_connect_at = Some(now);
                return;
            }
        };

        for (op, payload) in frames {
            match op {
                OP_PING => {
                    if let Some(transport) = &mut self.transport {
                        let _ = transport.write_frame(OP_PONG, &payload);
                    }
                }
                OP_CLOSE => {
                    warn!(%payload, "Discord 关闭了 IPC 连接");
                    self.drop_connection();
                    self.next_connect_at = Some(now);
                    return;
                }
                OP_FRAME => self.handle_reply(&payload, now),
                _ => {}
            }
        }

        while let Some(oldest) = self.awaiting.front() {
            if now.saturating_duration_since(oldest.at) < REPLY_DEADLINE {
                break;
            }
            warn!(seq = ?oldest.seq, "Discord 没有回复这次 Activity 更新");
            self.awaiting.pop_front();
        }
    }

    fn handle_reply(&mut self, payload: &Value, now: Instant) {
        let Some(nonce) = payload.get("nonce").and_then(Value::as_str) else {
            return;
        };
        let Some(index) = self.awaiting.iter().position(|a| a.nonce == nonce) else {
            return;
        };
        let Some(sent) = self.awaiting.remove(index) else {
            return;
        };
        let waited_ms = now.saturating_duration_since(sent.at).as_millis();

        if payload.get("evt").and_then(Value::as_str) == Some("ERROR") {
            let code = payload.pointer("/data/code").and_then(Value::as_i64);
            let message = payload
                .pointer("/data/message")
                .and_then(Value::as_str)
                .unwrap_or("");
            warn!(
                seq = ?sent.seq,
                stage = "rpc_rejected",
                ?code,
                message,
                waited_ms,
                "Discord 拒绝了 Activity 更新"
            );
            // Discord 还拿着之前的卡片, 但那是哪一张已经说不准了
            if self.last_written.as_ref() == Some(&sent.desired) {
                self.last_written = None;
            }
            self.rejected = Some(sent.desired);
        } else {
            debug!(seq = ?sent.seq, stage = "rpc_ack", waited_ms, "Discord 已接受 Activity 更新");
        }
    }

    /// 把现状同步给 Discord, 返回最迟多久之后需要再被调用一次
    fn tick(&mut self, now: Instant) -> Duration {
        if !self.is_enabled {
            if self.transport.is_some() {
                self.disconnect();
            }
            return IDLE_TICK;
        }

        self.drain_replies(now);
        let desired = self.desired();

        if self.transport.is_none() {
            // 没有要显示的东西时不必连着 Discord
            if desired == Desired::Clear {
                return IDLE_TICK;
            }
            if let Some(retry_at) = self.next_connect_at
                && now < retry_at
            {
                return retry_at.duration_since(now).min(IDLE_TICK);
            }
            match self.connector.connect(APP_ID) {
                Ok(transport) => {
                    info!("Discord IPC 已连接");
                    self.transport = Some(transport);
                    // 新连接上还没有设置过任何 Activity
                    self.last_written = Some(Desired::Clear);
                    self.next_connect_at = None;
                    // 握手可能花了几百毫秒, 期间到达的消息还在队列里。
                    // 先回去把它们处理完, 再按最新的状态写第一张卡片
                    return Duration::ZERO;
                }
                Err(e) => {
                    info!("连接 Discord IPC 失败: {e}. Discord 可能未运行");
                    self.next_connect_at = Some(now + RECONNECT_DELAY);
                    return IDLE_TICK;
                }
            }
        }

        if self.last_written.as_ref() == Some(&desired) || self.rejected.as_ref() == Some(&desired)
        {
            return IDLE_TICK;
        }

        if let Some(hold) = self.hold_until
            && now < hold
        {
            return hold.duration_since(now);
        }

        let wait = self.policy.wait_for(now);
        if !wait.is_zero() {
            return wait.min(IDLE_TICK);
        }

        self.write(desired, now)
    }

    fn write(&mut self, desired: Desired, now: Instant) -> Duration {
        let nonce = self.next_nonce();
        let frame = activity_frame(&desired, &nonce);
        let seq = self.playback.as_ref().map(|p| p.seq);
        let Some(transport) = &mut self.transport else {
            return IDLE_TICK;
        };

        if let Err(e) = transport.write_frame(OP_FRAME, &frame) {
            warn!("设置 Discord Activity 失败: {e}, 尝试重连");
            self.drop_connection();
            // 刚才还好好的连接断了, 多半是 Discord 重启, 马上重连一次
            self.next_connect_at = Some(now);
            return RETRY_TICK;
        }

        match &desired {
            Desired::Clear => debug!(?seq, stage = "rpc_write", "清除 Discord Activity"),
            Desired::Card(state) => debug!(
                ?seq,
                stage = "rpc_write",
                song = %state.card.details,
                third_line = ?state.card.third_line,
                "更新 Discord Activity"
            ),
        }

        self.policy.record(now);
        self.rejected = None;
        self.awaiting.push_back(Awaiting {
            nonce,
            at: now,
            seq,
            desired: desired.clone(),
        });
        self.last_written = Some(desired);
        IDLE_TICK
    }
}

fn activity_frame(desired: &Desired, nonce: &str) -> Value {
    let activity = match desired {
        Desired::Clear => Value::Null,
        Desired::Card(state) => serde_json::to_value(build_activity(state)).unwrap_or(Value::Null),
    };
    json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce,
    })
}

impl<C: Connector> Drop for RpcWorker<C> {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn background_loop(rx: &Receiver<Envelope>) {
    let mut worker = RpcWorker::new(PipeConnector);
    let mut wait = IDLE_TICK;

    loop {
        match rx.recv_timeout(wait) {
            Ok((msg, at)) => {
                worker.handle_message(msg, at);
                // 一起到达的消息先全部吃完再同步, 中间状态不值得写出去
                while let Ok((msg, at)) = rx.try_recv() {
                    worker.handle_message(msg, at);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        wait = worker.tick(Instant::now());
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

fn send(msg: RpcMessage, at: Stamp) {
    if let Ok(guard) = SENDER.lock()
        && let Some(tx) = guard.as_ref()
        && let Err(e) = tx.send((msg, at))
    {
        warn!("向 Discord RPC 线程发送消息失败: {e}");
    }
}

pub fn enable() {
    send(RpcMessage::Enable, Stamp::now());
}
pub fn disable() {
    send(RpcMessage::Disable, Stamp::now());
}
pub fn update_config(payload: DiscordConfigPayload) {
    send(RpcMessage::Config(payload), Stamp::now());
}
pub fn update_track(payload: TrackUpdate, at: Stamp) {
    send(RpcMessage::Track(Box::new(payload)), at);
}
pub fn update_play_state(payload: PlayStatePayload, at: Stamp) {
    send(RpcMessage::PlayState(payload), at);
}
pub fn update_timeline(payload: TimelinePayload, at: Stamp) {
    send(RpcMessage::Timeline(payload), at);
}
pub fn update_audio_info(payload: AudioInfoPayload, at: Stamp) {
    send(RpcMessage::AudioInfo(payload), at);
}
pub fn update_audio_header(specs: AudioHeaderSpecs) {
    send(RpcMessage::AudioHeader(specs), Stamp::now());
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        io,
        rc::Rc,
    };

    use super::*;
    use crate::model::{
        ArtistPayload,
        CoverPayload,
    };

    fn artist(name: &str, id: Option<u64>) -> ArtistPayload {
        ArtistPayload {
            name: name.to_string(),
            id,
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
                artist("哔哩哔哩拜年纪", Some(47_090_969)),
                artist("洛天依Official", Some(906_118)),
                artist("乐正绫", Some(1_102_240)),
                artist("裘丹莉", Some(52_437_191)),
            ],
            album_id: Some(361_770_580),
            trans_name: None,
            album_trans_name: None,
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
            seq: 0,
            stream: 0,
            ncm_id: 3_348_915_450,
            codec: codec.map(str::to_string),
            bitrate,
            sample_rate,
            bit_depth,
            level: None,
        }
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
    fn full_line_shows_everything_that_is_known() {
        let line = |a| format_audio_full(&a);
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

        // 最长的常见写法也要能放进卡片的一行 (大约 37 个字符)
        let longest = line(audio(Some("flac"), Some(1_717_000), Some(44_100), Some(24))).unwrap();
        assert_eq!(longest, "FLAC 24-bit/44.1 kHz, 1717 kbps");
        assert!(longest.chars().count() <= 37);
    }

    #[test]
    fn compact_line_shows_the_one_number_that_says_the_quality() {
        let line = |a| format_audio_compact(&a);

        // 无损看位深和采样率
        assert_eq!(
            line(audio(Some("flac"), Some(985_000), Some(44_100), Some(16))).as_deref(),
            Some("FLAC 16-bit/44.1 kHz")
        );
        assert_eq!(
            line(audio(Some("flac"), Some(1_596_360), Some(44_100), None)).as_deref(),
            Some("FLAC 44.1 kHz")
        );
        // 有损看码率
        assert_eq!(
            line(audio(Some("m4a"), Some(256_016), Some(48_000), None)).as_deref(),
            Some("AAC 256 kbps")
        );
        assert_eq!(
            line(audio(Some("mp3"), Some(320_000), Some(44_100), None)).as_deref(),
            Some("MP3 320 kbps")
        );

        // 该看的那一项缺失时, 用另一项顶上
        assert_eq!(
            line(audio(Some("flac"), Some(1_869_617), None, None)).as_deref(),
            Some("FLAC 1870 kbps")
        );
        assert_eq!(
            line(audio(Some("mp3"), None, Some(44_100), None)).as_deref(),
            Some("MP3 44.1 kHz")
        );
        assert_eq!(
            line(audio(None, Some(320_000), None, None)).as_deref(),
            Some("320 kbps")
        );
        assert_eq!(
            line(audio(Some("flac"), None, None, None)).as_deref(),
            Some("FLAC")
        );
        assert_eq!(line(audio(None, None, None, None)), None);
    }

    /// 数值全部来自实机: 网易云在各个音质下实际下发的音频流
    #[test]
    fn real_streams_read_as_plain_specs() {
        let cases = [
            (
                audio(Some("m4a"), Some(96_007), Some(48_000), None),
                "AAC 48 kHz, 96 kbps",
                "AAC 96 kbps",
            ),
            (
                audio(Some("m4a"), Some(256_016), Some(48_000), None),
                "AAC 48 kHz, 256 kbps",
                "AAC 256 kbps",
            ),
            (
                audio(Some("mp3"), Some(320_000), Some(44_100), None),
                "MP3 44.1 kHz, 320 kbps",
                "MP3 320 kbps",
            ),
            (
                audio(Some("flac"), Some(1_103_664), Some(48_000), None),
                "FLAC 48 kHz, 1104 kbps",
                "FLAC 48 kHz",
            ),
            // 位深读自缓存文件头
            (
                audio(Some("flac"), Some(1_694_785), Some(48_000), Some(24)),
                "FLAC 24-bit/48 kHz, 1695 kbps",
                "FLAC 24-bit/48 kHz",
            ),
        ];

        for (spec, full, compact) in cases {
            assert_eq!(format_audio_full(&spec).as_deref(), Some(full));
            assert_eq!(format_audio_compact(&spec).as_deref(), Some(compact));
        }
    }

    #[test]
    fn a_lossless_file_without_its_header_still_reads() {
        // 读不到文件头: 既没有采样率也没有位深
        let cached = audio(Some("flac"), Some(1_869_617), None, None);
        assert_eq!(
            format_audio_full(&cached).as_deref(),
            Some("FLAC, 1870 kbps")
        );
        assert_eq!(
            format_audio_compact(&cached).as_deref(),
            Some("FLAC 1870 kbps")
        );
    }

    #[test]
    fn third_line_choices() {
        let spec = audio(Some("flac"), Some(1_103_664), Some(48_000), Some(16));
        let line = |choice, audio: Option<&AudioInfoPayload>| {
            build_card(&metadata(), audio, &third_line(choice)).third_line
        };
        let album = "2026哔哩哔哩拜年纪";

        let cases = [
            (DiscordThirdLine::Album, album.to_string()),
            (DiscordThirdLine::Compact, "FLAC 16-bit/48 kHz".to_string()),
            (
                DiscordThirdLine::QualityAndAlbum,
                format!("FLAC 16-bit/48 kHz · {album}"),
            ),
            (
                DiscordThirdLine::Full,
                "FLAC 16-bit/48 kHz, 1104 kbps".to_string(),
            ),
        ];
        for (choice, expected) in cases {
            assert_eq!(line(choice, Some(&spec)), Some(expected), "{choice:?}");
            // 读不到规格时, 每一种选择都退回专辑名
            assert_eq!(line(choice, None).as_deref(), Some(album), "{choice:?}");
        }

        let lossy = audio(Some("m4a"), Some(256_016), Some(48_000), None);
        assert_eq!(
            line(DiscordThirdLine::QualityAndAlbum, Some(&lossy)),
            Some(format!("AAC 256 kbps · {album}"))
        );
    }

    /// 音质放在专辑名前面: 一行放不下时被截掉的是专辑名, 不是音质
    #[test]
    fn a_long_album_is_what_gets_cut_not_the_quality() {
        let mut meta = metadata();
        meta.album_name = "专".repeat(200);
        let spec = audio(Some("flac"), Some(1_103_664), Some(48_000), Some(24));
        let card = build_card(
            &meta,
            Some(&spec),
            &third_line(DiscordThirdLine::QualityAndAlbum),
        );
        let line = card.third_line.unwrap();
        assert!(line.starts_with("FLAC 24-bit/48 kHz · 专"));
        assert_eq!(line.chars().count(), FIELD_MAX_CHARS);
    }

    /// 带档位名字的两个选项都已经换成 "音质 · 专辑名", 存储里残留的旧取值不能让整条配置被拒绝
    #[test]
    fn the_retired_tier_choices_read_as_quality_and_album() {
        let parse = |json| serde_json::from_str::<DiscordThirdLine>(json).unwrap();
        assert_eq!(parse(r#""Tier""#), DiscordThirdLine::QualityAndAlbum);
        assert_eq!(
            parse(r#""TierAndAlbum""#),
            DiscordThirdLine::QualityAndAlbum
        );
        assert_eq!(
            parse(r#""QualityAndAlbum""#),
            DiscordThirdLine::QualityAndAlbum
        );
        assert_eq!(parse(r#""Compact""#), DiscordThirdLine::Compact);
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
        meta.album_name = "聖槍爆裂ボーイ".to_string();
        meta.album_trans_name = Some("圣枪爆裂男孩".to_string());
        meta.artists = vec![artist("澤野弘之", Some(1)), artist("Aimer", Some(2))];

        let card = build_card(&meta, None, &CardOptions::default());
        assert_eq!(card.details, "石火");
        assert_eq!(card.state, "澤野弘之, Aimer");
        assert_eq!(card.third_line.as_deref(), Some("聖槍爆裂ボーイ"));

        let shown = CardOptions {
            show_translation: true,
            ..CardOptions::default()
        };
        let card = build_card(&meta, None, &shown);
        assert_eq!(card.details, "石火 (Stonefire)");
        assert_eq!(card.state, "澤野弘之, Aimer");
        assert_eq!(
            card.third_line.as_deref(),
            Some("聖槍爆裂ボーイ (圣枪爆裂男孩)")
        );

        // A translation equal to the name, or blank, adds nothing
        meta.trans_name = Some("石火".to_string());
        meta.album_trans_name = Some("  ".to_string());
        let card = build_card(&meta, None, &shown);
        assert_eq!(card.details, "石火");
        assert_eq!(card.third_line.as_deref(), Some("聖槍爆裂ボーイ"));
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
    fn a_pending_quality_line_stays_empty_instead_of_showing_the_album() {
        let line = |choice, pending| {
            build_card_with(&metadata(), None, pending, &third_line(choice)).third_line
        };
        let album = "2026哔哩哔哩拜年纪";

        for choice in [DiscordThirdLine::Full, DiscordThirdLine::Compact] {
            assert_eq!(line(choice, true), None, "{choice:?}");
            // 确定读不到规格时才退回专辑名
            assert_eq!(line(choice, false).as_deref(), Some(album), "{choice:?}");
        }
        // 这两种写法本来就带着专辑名
        assert_eq!(line(DiscordThirdLine::Album, true).as_deref(), Some(album));
        assert_eq!(
            line(DiscordThirdLine::QualityAndAlbum, true).as_deref(),
            Some(album)
        );

        // 本地歌曲和播客永远没有规格, 不存在 "还在路上"
        let mut local = metadata();
        local.kind = Some(SongKind::Local);
        let card = build_card_with(&local, None, true, &audio_options());
        assert_eq!(card.third_line.as_deref(), Some(album));
    }

    #[derive(Default)]
    struct Pipe {
        written: Vec<(u32, Value)>,
        incoming: Vec<(u32, Value)>,
        connects: usize,
        refuse_connect: bool,
        fail_writes: bool,
    }

    #[derive(Clone, Default)]
    struct FakeDiscord(Rc<RefCell<Pipe>>);

    struct FakeTransport(Rc<RefCell<Pipe>>);

    impl Transport for FakeTransport {
        fn write_frame(&mut self, op: u32, payload: &Value) -> io::Result<()> {
            let mut pipe = self.0.borrow_mut();
            if pipe.fail_writes {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            pipe.written.push((op, payload.clone()));
            Ok(())
        }

        fn read_frames(&mut self) -> io::Result<Vec<(u32, Value)>> {
            Ok(std::mem::take(&mut self.0.borrow_mut().incoming))
        }
    }

    impl Connector for FakeDiscord {
        type Transport = FakeTransport;

        fn connect(&mut self, _client_id: &str) -> io::Result<FakeTransport> {
            let mut pipe = self.0.borrow_mut();
            pipe.connects += 1;
            if pipe.refuse_connect {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            Ok(FakeTransport(self.0.clone()))
        }
    }

    const WALL_T0: i64 = 1_800_000_000_000;
    const DURATION_MS: i64 = 258_586;
    const ONE_YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

    /// 一个接着假 Discord 的 worker, 时间由测试自己推进 (单位毫秒)
    struct Rig {
        worker: RpcWorker<FakeDiscord>,
        discord: FakeDiscord,
        t0: Instant,
    }

    impl Rig {
        fn new(third_line: DiscordThirdLine, show_when_paused: bool) -> Self {
            let discord = FakeDiscord::default();
            let mut rig = Self {
                worker: RpcWorker::new(discord.clone()),
                discord,
                t0: Instant::now(),
            };
            rig.msg(0, RpcMessage::Enable);
            rig.config(0, third_line, show_when_paused);
            rig
        }

        fn config(&mut self, ms: u64, third_line: DiscordThirdLine, show_when_paused: bool) {
            self.msg(
                ms,
                RpcMessage::Config(DiscordConfigPayload {
                    show_when_paused,
                    display_mode: None,
                    app_name_mode: DiscordAppNameMode::Default,
                    third_line,
                    artist_separator: DiscordArtistSeparator::Comma,
                    show_translation: false,
                    links: true,
                }),
            );
        }

        fn stamp(&self, ms: u64) -> Stamp {
            Stamp {
                mono: self.t0 + Duration::from_millis(ms),
                wall_ms: WALL_T0 + ms as i64,
            }
        }

        fn msg(&mut self, ms: u64, msg: RpcMessage) {
            let at = self.stamp(ms);
            self.worker.handle_message(msg, at);
        }

        fn track(&mut self, ms: u64, update: TrackUpdate) {
            self.msg(ms, RpcMessage::Track(Box::new(update)));
        }

        fn progress(&mut self, ms: u64, position_ms: f64) {
            self.timeline(ms, position_ms, TimelineReason::Progress);
        }

        fn timeline(&mut self, ms: u64, position_ms: f64, reason: TimelineReason) {
            self.msg(
                ms,
                RpcMessage::Timeline(TimelinePayload {
                    current_time: position_ms,
                    total_time: DURATION_MS as f64,
                    seq: 0,
                    reason,
                }),
            );
        }

        fn status(&mut self, ms: u64, status: PlaybackStatus) {
            self.msg(ms, RpcMessage::PlayState(PlayStatePayload { status }));
        }

        fn tick(&mut self, ms: u64) -> Duration {
            let now = self.stamp(ms).mono;
            self.worker.tick(now)
        }

        /// 每 50 毫秒同步一次, 返回期间每次写入发生的时刻
        fn run(&mut self, from_ms: u64, to_ms: u64) -> Vec<u64> {
            let mut writes = Vec::new();
            for ms in (from_ms..=to_ms).step_by(50) {
                let before = self.activities().len();
                self.tick(ms);
                if self.activities().len() > before {
                    writes.push(ms);
                }
            }
            writes
        }

        /// 同上, 但每秒钟上报一次进度, 上报的位置带着真实客户端那样的抖动
        fn play(&mut self, from_ms: u64, to_ms: u64, offset_ms: f64) -> Vec<u64> {
            let mut writes = Vec::new();
            for ms in (from_ms..=to_ms).step_by(50) {
                if ms % 1000 == 0 {
                    let jitter = if (ms / 1000) % 2 == 0 { 140.0 } else { -90.0 };
                    self.progress(ms, ms as f64 + offset_ms + jitter);
                }
                writes.extend(self.run(ms, ms));
            }
            writes
        }

        /// 写给 Discord 的每一个 Activity, 清除时是 `null`
        fn activities(&self) -> Vec<Value> {
            self.discord
                .0
                .borrow()
                .written
                .iter()
                .filter(|(op, frame)| *op == OP_FRAME && frame["cmd"] == "SET_ACTIVITY")
                .map(|(_, frame)| frame["args"]["activity"].clone())
                .collect()
        }

        fn last_nonce(&self) -> String {
            let pipe = self.discord.0.borrow();
            pipe.written.last().unwrap().1["nonce"]
                .as_str()
                .unwrap()
                .to_string()
        }

        fn reply(&self, frame: Value) {
            self.discord.0.borrow_mut().incoming.push((OP_FRAME, frame));
        }
    }

    fn song(seq: u64, name: &str) -> TrackUpdate {
        let mut meta = metadata();
        meta.song_name = name.to_string();
        TrackUpdate {
            seq,
            metadata: meta,
            audio: Some(AudioInfoPayload {
                seq,
                ..audio(Some("flac"), Some(1_596_360), Some(44_100), Some(24))
            }),
            audio_pending: false,
            status: PlaybackStatus::Playing,
            position_ms: 0.0,
        }
    }

    fn start(activity: &Value) -> i64 {
        activity["timestamps"]["start"].as_i64().unwrap()
    }

    fn third(activity: &Value) -> Option<&str> {
        activity["assets"]["large_text"].as_str()
    }

    #[test]
    fn stable_playback_is_written_exactly_once() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));

        let writes = rig.play(0, 60_000, 0.0);

        assert_eq!(writes, [100]);
        let activities = rig.activities();
        assert_eq!(activities[0]["details"], "A");
        assert_eq!(start(&activities[0]), WALL_T0);
        assert_eq!(
            activities[0]["timestamps"]["end"].as_i64(),
            Some(WALL_T0 + DURATION_MS)
        );
        assert_eq!(
            third(&activities[0]),
            Some("FLAC 24-bit/44.1 kHz, 1596 kbps")
        );
    }

    #[test]
    fn the_next_songs_audio_never_touches_the_card_on_screen() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.run(0, 200);

        // B 的规格先到, B 的快照还没到
        rig.msg(
            5000,
            RpcMessage::AudioInfo(AudioInfoPayload {
                seq: 2,
                ..audio(Some("mp3"), Some(320_000), Some(48_000), None)
            }),
        );
        assert_eq!(rig.run(5000, 9000), [] as [u64; 0]);

        let mut next = song(2, "B");
        next.audio = None;
        next.audio_pending = true;
        rig.track(9000, next);
        assert_eq!(rig.run(9000, 9500), [9100]);

        let activities = rig.activities();
        assert_eq!(activities.len(), 2);
        assert_eq!(activities[1]["details"], "B");
        assert_eq!(third(&activities[1]), Some("MP3 48 kHz, 320 kbps"));
    }

    #[test]
    fn late_quality_fills_in_once_without_moving_progress() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        let mut track = song(1, "A");
        track.audio = None;
        track.audio_pending = true;
        rig.track(0, track);
        rig.run(0, 200);

        rig.msg(
            300,
            RpcMessage::AudioInfo(AudioInfoPayload {
                seq: 1,
                ..audio(Some("flac"), Some(1_596_360), Some(48_000), Some(24))
            }),
        );
        let writes = rig.play(300, 30_000, 0.0);

        assert_eq!(writes, [4100]);
        let activities = rig.activities();
        assert_eq!(third(&activities[0]), None);
        assert_eq!(third(&activities[1]), Some("FLAC 24-bit/48 kHz, 1596 kbps"));
        assert_eq!(activities[0]["timestamps"], activities[1]["timestamps"]);
    }

    #[test]
    fn quality_that_arrives_within_the_settle_window_is_in_the_first_card() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        let mut track = song(1, "A");
        track.audio = Some(AudioInfoPayload {
            seq: 1,
            ..audio(Some("flac"), Some(1_596_360), None, None)
        });
        rig.track(0, track);
        // 文件头读到的采样率和位深
        rig.msg(
            30,
            RpcMessage::AudioInfo(AudioInfoPayload {
                seq: 1,
                ..audio(Some("flac"), Some(1_596_360), Some(48_000), Some(24))
            }),
        );

        assert_eq!(rig.run(0, 10_000), [100]);
        assert_eq!(
            third(&rig.activities()[0]),
            Some("FLAC 24-bit/48 kHz, 1596 kbps")
        );
    }

    #[test]
    fn a_header_read_by_the_backend_completes_the_first_card_on_its_own() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        let mut track = song(1, "A");
        // 从磁盘缓存恢复的条目: 没有采样率, 也没有位深
        track.audio = Some(AudioInfoPayload {
            seq: 1,
            stream: 7,
            ..audio(Some("flac"), Some(1_596_360), None, None)
        });
        rig.track(0, track);
        rig.msg(
            5,
            RpcMessage::AudioHeader(AudioHeaderSpecs {
                stream: 7,
                sample_rate: 48_000,
                bit_depth: Some(24),
            }),
        );

        // 前端转一圈送回来的同一份规格晚了 600 毫秒, 内容相同, 不再写一次
        rig.msg(
            600,
            RpcMessage::AudioInfo(AudioInfoPayload {
                seq: 1,
                stream: 7,
                ..audio(Some("flac"), Some(1_596_360), Some(48_000), Some(24))
            }),
        );

        assert_eq!(rig.play(0, 20_000, 0.0), [100]);
        assert_eq!(
            third(&rig.activities()[0]),
            Some("FLAC 24-bit/48 kHz, 1596 kbps")
        );
    }

    #[test]
    fn a_header_for_another_stream_is_not_applied() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        let mut track = song(1, "A");
        track.audio = Some(AudioInfoPayload {
            seq: 1,
            stream: 8,
            ..audio(Some("flac"), Some(1_596_360), Some(44_100), None)
        });
        rig.track(0, track);
        // 同一首歌上一个音质的文件头
        rig.msg(
            5,
            RpcMessage::AudioHeader(AudioHeaderSpecs {
                stream: 7,
                sample_rate: 96_000,
                bit_depth: Some(24),
            }),
        );

        rig.run(0, 5000);
        assert_eq!(
            third(&rig.activities()[0]),
            Some("FLAC 44.1 kHz, 1596 kbps")
        );
    }

    #[test]
    fn paused_and_hidden_clears_once() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.play(0, 10_000, 0.0);

        rig.status(10_000, PlaybackStatus::Paused);
        let mut writes = Vec::new();
        for ms in (10_000..30_000).step_by(500) {
            rig.status(ms, PlaybackStatus::Paused);
            rig.progress(ms, 10_000.0);
            writes.extend(rig.run(ms, ms + 450));
        }

        assert_eq!(writes, [10_800]);
        assert_eq!(rig.activities()[1], Value::Null);
    }

    #[test]
    fn paused_and_visible_writes_one_frozen_card() {
        let mut rig = Rig::new(DiscordThirdLine::Full, true);
        rig.track(0, song(1, "A"));
        rig.play(0, 10_000, 0.0);

        rig.status(10_000, PlaybackStatus::Paused);
        let mut writes = Vec::new();
        for ms in (10_000..30_000).step_by(500) {
            rig.status(ms, PlaybackStatus::Paused);
            rig.progress(ms, 10_000.0);
            writes.extend(rig.run(ms, ms + 450));
        }

        assert_eq!(writes, [10_800]);
        let paused = &rig.activities()[1];
        assert_eq!(paused["details"], "A");
        // 暂停在 10 秒处: 进度条冻结在那一刻
        assert_eq!(start(paused), WALL_T0 + ONE_YEAR_MS);

        // 恢复播放后从暂停的位置继续
        rig.status(40_000, PlaybackStatus::Playing);
        assert_eq!(rig.play(40_000, 50_000, -30_000.0), [40_000]);
        assert_eq!(start(&rig.activities()[2]), WALL_T0 + 30_000);
    }

    #[test]
    fn an_explicit_seek_is_written_however_short() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.play(0, 10_000, 0.0);

        // 往前跳 1 秒
        rig.timeline(10_020, 11_020.0, TimelineReason::Seek);
        let writes = rig.play(10_050, 30_000, 1000.0);

        assert_eq!(writes, [10_050]);
        let activities = rig.activities();
        assert_eq!(start(&activities[1]), start(&activities[0]) - 1000);
    }

    #[test]
    fn progress_noise_is_not_a_seek_but_a_real_jump_is() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.play(0, 10_000, 0.0);

        // 上报的位置比预期晚了 0.9 秒: 在容差之内
        rig.progress(10_500, 9600.0);
        assert_eq!(rig.run(10_500, 15_000), [] as [u64; 0]);

        // 没有跳转事件, 但位置一下子差了 30 秒
        rig.progress(16_000, 46_000.0);
        assert_eq!(rig.play(16_050, 30_000, 30_000.0), [16_050]);
        assert_eq!(start(&rig.activities()[1]), WALL_T0 - 30_000);
    }

    #[test]
    fn a_song_without_a_duration_still_gets_its_first_card() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        let mut track = song(1, "A");
        track.metadata.duration = None;
        rig.track(0, track);

        assert_eq!(rig.run(0, 3000), [100]);
        let first = &rig.activities()[0];
        assert_eq!(first["details"], "A");
        assert!(first.get("timestamps").is_none());

        // 时长随后跟着进度到达 (v2 客户端)
        let writes = rig.play(3000, 20_000, 0.0);
        assert_eq!(writes, [4100]);
        assert_eq!(start(&rig.activities()[1]), WALL_T0);
    }

    #[test]
    fn rapid_skipping_delivers_only_the_latest_song_one_spacing_later() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        let mut writes = rig.run(0, 950);
        rig.track(1000, song(2, "B"));
        writes.extend(rig.run(1000, 1950));
        rig.track(2000, song(3, "C"));
        writes.extend(rig.run(2000, 2950));
        rig.track(3000, song(4, "D"));
        writes.extend(rig.play(3000, 20_000, -3000.0));

        assert_eq!(writes, [100, 4100]);
        let activities = rig.activities();
        assert_eq!(activities[0]["details"], "A");
        assert_eq!(activities[1]["details"], "D");
        assert_eq!(start(&activities[1]), WALL_T0 + 3000);
    }

    #[test]
    fn a_song_ending_into_the_next_one_never_clears_the_card() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.play(0, 200_000, 0.0);

        rig.status(200_000, PlaybackStatus::Paused);
        let mut writes = rig.run(200_000, 200_250);
        let mut next = song(2, "B");
        next.status = PlaybackStatus::Paused;
        rig.track(200_300, next);
        writes.extend(rig.run(200_300, 200_300));
        rig.status(200_350, PlaybackStatus::Playing);
        writes.extend(rig.run(200_350, 205_000));

        assert_eq!(writes, [200_350]);
        let activities = rig.activities();
        assert_eq!(activities.len(), 2);
        assert_eq!(activities[1]["details"], "B");
        assert_eq!(start(&activities[1]), WALL_T0 + 200_350);
    }

    #[test]
    fn an_update_for_the_same_play_keeps_its_progress() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.play(0, 30_000, 0.0);

        let mut refreshed = song(1, "A");
        refreshed.position_ms = 0.0;
        refreshed.metadata.cover = Some(CoverPayload {
            url: Some("http://p1.music.126.net/new.jpg".to_string()),
        });
        rig.track(30_000, refreshed);
        assert_eq!(rig.play(30_050, 40_000, 0.0), [30_050]);

        let activities = rig.activities();
        assert_eq!(activities[0]["timestamps"], activities[1]["timestamps"]);
        assert_ne!(
            activities[0]["assets"]["large_image"],
            activities[1]["assets"]["large_image"]
        );
    }

    #[test]
    fn a_rejected_card_is_not_resent_until_it_changes() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.run(0, 200);

        rig.reply(json!({
            "cmd": "SET_ACTIVITY",
            "evt": "ERROR",
            "nonce": rig.last_nonce(),
            "data": { "code": 4000, "message": "child \"activity\" fails" },
        }));
        assert_eq!(rig.play(250, 20_000, 0.0), [] as [u64; 0]);
        assert_eq!(rig.worker.last_written, None);

        rig.config(20_000, DiscordThirdLine::Album, false);
        assert_eq!(rig.run(20_000, 21_000), [20_000]);
        assert_eq!(third(&rig.activities()[1]), Some("2026哔哩哔哩拜年纪"));
    }

    #[test]
    fn an_accepted_card_is_matched_to_its_write() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.run(0, 200);
        assert_eq!(rig.worker.awaiting.len(), 1);

        // 别的回复不算数
        rig.reply(json!({ "cmd": "SET_ACTIVITY", "evt": null, "nonce": "someone-else" }));
        rig.tick(250);
        assert_eq!(rig.worker.awaiting.len(), 1);

        rig.reply(json!({ "cmd": "SET_ACTIVITY", "evt": null, "nonce": rig.last_nonce() }));
        rig.tick(300);
        assert!(rig.worker.awaiting.is_empty());
        assert!(rig.worker.rejected.is_none());
    }

    #[test]
    fn a_ping_is_answered() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.run(0, 200);

        rig.discord
            .0
            .borrow_mut()
            .incoming
            .push((OP_PING, json!({ "beat": 7 })));
        rig.tick(250);

        let pipe = rig.discord.0.borrow();
        assert_eq!(pipe.written.last(), Some(&(OP_PONG, json!({ "beat": 7 }))));
    }

    #[test]
    fn a_broken_pipe_reconnects_at_once_and_restores_the_card() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.play(0, 10_000, 0.0);

        rig.discord.0.borrow_mut().fail_writes = true;
        rig.timeline(10_000, 60_000.0, TimelineReason::Seek);
        rig.tick(10_000);
        assert!(rig.worker.transport.is_none());

        rig.discord.0.borrow_mut().fail_writes = false;
        // 10.05 秒时重连, 下一次同步写出卡片
        assert_eq!(rig.play(10_050, 20_000, 50_000.0), [10_100]);

        assert_eq!(rig.discord.0.borrow().connects, 2);
        let restored = &rig.activities()[1];
        assert_eq!(restored["details"], "A");
        assert_eq!(start(restored), WALL_T0 - 50_000);
    }

    #[test]
    fn a_missing_discord_is_retried_by_the_clock_not_by_message_count() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.discord.0.borrow_mut().refuse_connect = true;
        rig.track(0, song(1, "A"));

        // 一阵密集的消息不会把重连的冷却提前耗尽
        for ms in 0..200 {
            rig.progress(ms * 10, (ms * 10) as f64);
            rig.tick(ms * 10);
        }
        assert_eq!(rig.discord.0.borrow().connects, 1);

        rig.tick(4900);
        assert_eq!(rig.discord.0.borrow().connects, 1);
        rig.tick(5000);
        assert_eq!(rig.discord.0.borrow().connects, 2);

        rig.discord.0.borrow_mut().refuse_connect = false;
        assert_eq!(rig.run(5050, 11_000), [10_050]);
        assert_eq!(rig.activities()[0]["details"], "A");
    }

    #[test]
    fn what_arrives_during_the_handshake_is_in_the_first_card() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        let mut resumed = song(1, "A");
        resumed.position_ms = 0.0;
        rig.track(0, resumed);

        // 连接发生在这次同步里; 握手期间用户跳到了 191 秒
        rig.tick(100);
        assert!(rig.activities().is_empty());
        rig.timeline(120, 191_000.0, TimelineReason::Seek);

        assert_eq!(rig.run(150, 10_000), [150]);
        assert_eq!(start(&rig.activities()[0]), WALL_T0 + 120 - 191_000);
    }

    #[test]
    fn nothing_to_show_means_no_connection_and_no_clears() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.run(0, 5000);
        assert_eq!(rig.discord.0.borrow().connects, 0);

        let mut paused = song(1, "A");
        paused.status = PlaybackStatus::Paused;
        rig.track(5000, paused);
        rig.run(5000, 20_000);
        assert_eq!(rig.discord.0.borrow().connects, 0);
        assert!(rig.activities().is_empty());
    }

    #[test]
    fn disabling_clears_and_closes() {
        let mut rig = Rig::new(DiscordThirdLine::Full, false);
        rig.track(0, song(1, "A"));
        rig.run(0, 200);

        rig.msg(1000, RpcMessage::Disable);
        rig.tick(1000);

        let pipe = rig.discord.0.borrow();
        let last_two: Vec<u32> = pipe
            .written
            .iter()
            .rev()
            .take(2)
            .map(|(op, _)| *op)
            .collect();
        assert_eq!(last_two, [OP_CLOSE, OP_FRAME]);
        assert_eq!(
            pipe.written[pipe.written.len() - 2].1["args"]["activity"],
            Value::Null
        );
    }
}
