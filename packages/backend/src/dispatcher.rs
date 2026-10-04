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
};

use tracing::{
    debug,
    error,
    warn,
};

use crate::{
    audio_header,
    discord,
    model::{
        AppMessage,
        CommandResult,
        CommandStatus,
        CoverUpdate,
        MetadataPayload,
        Stamp,
    },
    smtc_core::{
        self,
        Cover,
        SmtcContext,
    },
};

type Envelope = (AppMessage, Stamp);

static GLOBAL_SENDER: LazyLock<Mutex<Option<Sender<Envelope>>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn init() {
    let (tx, rx) = mpsc::channel();

    discord::init();

    thread::Builder::new()
        .name("dispatcher-thread".into())
        .spawn(move || {
            run_dispatcher_loop(&rx);
        })
        .expect("无法启动 Dispatcher 线程");

    if let Ok(mut guard) = GLOBAL_SENDER.lock() {
        *guard = Some(tx);
    }
}

pub fn shutdown() {
    if let Ok(guard) = GLOBAL_SENDER.lock() {
        if let Some(tx) = guard.as_ref() {
            if let Err(e) = tx.send((AppMessage::Shutdown, Stamp::now())) {
                error!("发送关闭信号失败: {e}");
            }
        } else {
            warn!("尝试关闭，但 Dispatcher 未初始化");
        }
    }
}

struct SmtcManager {
    ctx: Option<SmtcContext>,
}

impl SmtcManager {
    const fn new() -> Self {
        Self { ctx: None }
    }

    fn get_or_init(&mut self) -> Option<&mut SmtcContext> {
        if self.ctx.is_none() {
            match smtc_core::initialize() {
                Ok(ctx) => {
                    self.ctx = Some(ctx);
                }
                Err(e) => {
                    error!("SMTC 初始化失败: {e:?}");
                }
            }
        }
        self.ctx.as_mut()
    }

    fn shutdown(&mut self) {
        if let Some(mut ctx) = self.ctx.take() {
            let _ = smtc_core::set_enabled(&mut ctx, false);
        }
    }
}

/// SMTC 上正在显示的那首歌, 以及它的封面到了没有
///
/// 封面比文字晚到, 而且是按播放序号认领的: 快速切歌或者 A→B→A 时, 迟到的封面
/// 属于一次已经过去的播放, 不能贴到现在这首歌上。
#[derive(Default)]
struct NowShowing {
    track: Option<ShownTrack>,
}

struct ShownTrack {
    seq: u64,
    metadata: MetadataPayload,
    cover: Option<CoverUpdate>,
}

impl NowShowing {
    /// 记下新的曲目。同一次播放的补充更新保留已经到手的封面
    fn set_track(&mut self, seq: u64, metadata: MetadataPayload) {
        let cover = self
            .track
            .take()
            .filter(|shown| shown.seq == seq)
            .and_then(|shown| shown.cover);
        self.track = Some(ShownTrack {
            seq,
            metadata,
            cover,
        });
    }

    /// 封面属于正在显示的这次播放时收下它, 否则丢弃
    fn accept_cover(&mut self, cover: CoverUpdate) -> bool {
        match &mut self.track {
            Some(shown) if shown.seq == cover.seq => {
                shown.cover = Some(cover);
                true
            }
            _ => false,
        }
    }

    fn publish(&self, ctx: &SmtcContext) {
        let Some(shown) = &self.track else {
            return;
        };
        let cover = shown
            .cover
            .as_ref()
            .map_or(Cover::Pending, |c| Cover::Ready {
                bytes: c.cover_bytes.as_deref(),
                url: c.url.as_deref(),
            });
        if let Err(e) = smtc_core::update_metadata(ctx, &shown.metadata, cover) {
            error!("更新 SMTC 元数据失败: {e:?}");
        }
    }
}

fn run_dispatcher_loop(rx: &Receiver<Envelope>) {
    let mut smtc_manager = SmtcManager::new();
    let mut showing = NowShowing::default();

    while let Ok((msg, at)) = rx.recv() {
        match msg {
            AppMessage::UpdateTrack(update) => {
                showing.set_track(update.seq, update.metadata.clone());
                discord::update_track(*update, at);

                if let Some(ctx) = smtc_manager.get_or_init() {
                    showing.publish(ctx);
                }
            }
            AppMessage::UpdateCover(cover) => {
                let seq = cover.seq;
                if showing.accept_cover(cover) {
                    if let Some(ctx) = smtc_manager.get_or_init() {
                        showing.publish(ctx);
                    }
                } else {
                    debug!(seq, "封面属于已经切走的歌曲, 丢弃");
                }
            }
            AppMessage::UpdateAudioInfo(payload) => discord::update_audio_info(payload, at),
            AppMessage::ProbeAudioHeader(request) => audio_header::request(request),
            AppMessage::UpdatePlayState(payload) => {
                discord::update_play_state(payload.clone(), at);

                if let Some(ctx) = smtc_manager.get_or_init()
                    && let Err(e) = smtc_core::update_play_state(ctx, payload.status)
                {
                    error!("更新 SMTC 播放状态失败: {e:?}");
                }
            }
            AppMessage::UpdateTimeline(payload) => {
                discord::update_timeline(payload.clone(), at);

                if let Some(ctx) = smtc_manager.get_or_init()
                    && let Err(e) =
                        smtc_core::update_timeline(ctx, payload.current_time, payload.total_time)
                {
                    error!("更新 SMTC 时间线失败: {e:?}");
                }
            }
            AppMessage::UpdatePlayMode(payload) => {
                if let Some(ctx) = smtc_manager.get_or_init()
                    && let Err(e) =
                        smtc_core::update_play_mode(ctx, payload.is_shuffling, &payload.repeat_mode)
                {
                    error!("更新 SMTC 播放模式失败: {e:?}");
                }
            }
            AppMessage::EnableSmtc => {
                if let Some(ctx) = smtc_manager.get_or_init() {
                    if let Err(e) = smtc_core::set_enabled(ctx, true) {
                        error!("启用 SMTC 失败: {e:?}");
                    }
                    // 曲目快照可能在启用之前就到了, 那时候没法发布
                    showing.publish(ctx);
                }
            }
            AppMessage::DisableSmtc => {
                if let Some(ctx) = smtc_manager.get_or_init()
                    && let Err(e) = smtc_core::set_enabled(ctx, false)
                {
                    error!("禁用 SMTC 失败: {e:?}");
                }
            }
            AppMessage::EnableDiscord => discord::enable(),
            AppMessage::DisableDiscord => discord::disable(),
            AppMessage::DiscordConfig(cfg) => discord::update_config(cfg),
            AppMessage::Shutdown => {
                discord::disable();
                smtc_manager.shutdown();
                smtc_core::unregister_event_callback();
                break;
            }
        }
    }
}

pub fn send_command(json: &str, binary: Option<Vec<u8>>) -> String {
    // 进度锚点要的是命令到达的时刻, 不是 dispatcher 线程轮到它的时刻
    let at = Stamp::now();

    let mut command: AppMessage = match serde_json::from_str(json) {
        Ok(cmd) => cmd,
        Err(e) => {
            return serde_json::to_string(&CommandResult {
                status: CommandStatus::Error,
                message: Some(format!("JSON 解析失败: {e}")),
            })
            .expect("序列化错误响应时出错");
        }
    };

    // 二进制数据由调用方在渲染线程上就地取到 (`dispatchWithArrayBuffer`),
    // 跟着命令一起进来 —— 二者在同一个参数表里到达, 不存在错配的可能。
    if let Some(bytes) = binary {
        match &mut command {
            AppMessage::UpdateCover(update) => update.cover_bytes = Some(bytes),
            _ => {
                return serde_json::to_string(&CommandResult {
                    status: CommandStatus::Error,
                    message: Some("该命令不接受随附的二进制数据".to_owned()),
                })
                .expect("序列化错误响应时出错");
            }
        }
    }

    if let Ok(guard) = GLOBAL_SENDER.lock()
        && let Some(tx) = guard.as_ref()
    {
        if let Err(e) = tx.send((command, at)) {
            return error_result(format!("发送消息到 Actor 失败: {e}"));
        }
        return serde_json::to_string(&CommandResult {
            status: CommandStatus::Success,
            message: None,
        })
        .expect("序列化成功响应时出错");
    }

    error_result("Dispatcher 未初始化".into())
}

fn error_result(msg: String) -> String {
    serde_json::to_string(&CommandResult {
        status: CommandStatus::Error,
        message: Some(msg),
    })
    .expect("序列化错误结果时出错")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(name: &str) -> MetadataPayload {
        MetadataPayload {
            song_name: name.to_string(),
            author_name: String::new(),
            album_name: String::new(),
            cover: None,
            ncm_id: Some(1),
            duration: None,
            artists: Vec::new(),
            album_id: None,
            trans_name: None,
            kind: None,
        }
    }

    fn cover(seq: u64) -> CoverUpdate {
        CoverUpdate {
            seq,
            url: None,
            cover_bytes: Some(vec![seq as u8]),
        }
    }

    fn shown_cover(showing: &NowShowing) -> Option<u8> {
        showing
            .track
            .as_ref()
            .and_then(|t| t.cover.as_ref())
            .and_then(|c| c.cover_bytes.as_ref())
            .map(|bytes| bytes[0])
    }

    #[test]
    fn a_late_cover_for_the_first_a_never_lands_on_the_second_a() {
        let mut showing = NowShowing::default();
        showing.set_track(1, metadata("A"));
        showing.set_track(2, metadata("B"));
        showing.set_track(3, metadata("A"));

        // 歌曲 ID 相同, 但这是上一次播放 A 时请求的封面
        assert!(!showing.accept_cover(cover(1)));
        assert!(!showing.accept_cover(cover(2)));
        assert_eq!(shown_cover(&showing), None);

        assert!(showing.accept_cover(cover(3)));
        assert_eq!(shown_cover(&showing), Some(3));
    }

    #[test]
    fn a_new_track_starts_without_a_cover_but_a_refresh_keeps_it() {
        let mut showing = NowShowing::default();
        showing.set_track(1, metadata("A"));
        assert!(showing.accept_cover(cover(1)));

        // 同一次播放的补充更新
        showing.set_track(1, metadata("A"));
        assert_eq!(shown_cover(&showing), Some(1));

        showing.set_track(2, metadata("B"));
        assert_eq!(shown_cover(&showing), None);
    }

    #[test]
    fn a_cover_with_nothing_showing_is_dropped() {
        let mut showing = NowShowing::default();
        assert!(!showing.accept_cover(cover(1)));
    }
}
