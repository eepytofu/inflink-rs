import type {
	AudioHeaderResult,
	INcmAdapter,
	InternalEventMap,
	TimelineReason,
	TrackCover,
	TrackSnapshot,
} from "@/adapters/adapter";
import { PlayModeController } from "@/adapters/playModeController";
import type {
	AudioInfo,
	PlaybackEventMap,
	PlaybackStatus,
	PlayMode,
	RepeatMode,
	SongInfo,
	TimelineInfo,
	VolumeInfo,
} from "@/types/api";
import {
	CoverManager,
	type TypedEventListenerOrEventListenerObject,
	TypedEventTarget,
} from "@/utils";
import logger from "@/utils/logger";

const TIMELINE_INTERVAL_MS = 1000;

type AudioDataListener = TypedEventListenerOrEventListenerObject<
	PlaybackEventMap,
	"audioDataUpdate"
>;

export abstract class BaseNcmAdapter
	extends TypedEventTarget<PlaybackEventMap>
	implements INcmAdapter
{
	protected playState: PlaybackStatus = "Paused";
	protected musicDuration = 0;
	protected musicPlayProgress = 0;
	protected volume = 1.0;
	protected isMuted = false;
	protected resolutionSetting = "500";
	protected audioInfo: AudioInfo | null = null;
	/** `audioInfo` 描述的是哪一个音频流，0 表示没有编号 */
	protected audioStream = 0;

	protected readonly coverManager = new CoverManager();
	protected readonly playModeController = new PlayModeController();

	protected lastDispatchedSongId: string | number | null = null;
	protected lastDispatchedCoverUrl: string | undefined = undefined;

	public readonly internal = new TypedEventTarget<InternalEventMap>();
	protected trackSeq = 0;
	private currentSong: SongInfo | null = null;
	private trackCover: TrackCover | null = null;
	private lastTimelineDispatch = Number.NEGATIVE_INFINITY;

	protected abstract onAudioDataSubscriptionStarted(): void;
	protected abstract onAudioDataSubscriptionEnded(): void;

	private audioDataListeners = new Set<AudioDataListener>();

	public abstract initialize(): Promise<void>;
	public abstract dispose(): void;

	public abstract getCurrentSongInfo(): SongInfo | null;
	public abstract getPlayMode(): PlayMode;

	public abstract hasNativeSmtcSupport(): boolean;
	public abstract setInternalLogging(enabled: boolean): void;

	public abstract play(): void;
	public abstract pause(): void;
	public abstract nextSong(): void;
	public abstract previousSong(): void;
	public abstract seekTo(positionMs: number): void;
	public abstract setVolume(level: number): void;
	public abstract toggleMute(): void;

	protected abstract applyInternalPlayMode(mode: PlayMode): void;

	public getPlaybackStatus(): PlaybackStatus {
		return this.playState;
	}

	public getTimelineInfo(): TimelineInfo | null {
		if (this.musicDuration > 0) {
			return {
				currentTime: this.musicPlayProgress,
				totalTime: this.musicDuration,
			};
		}
		return null;
	}

	public getVolumeInfo(): VolumeInfo {
		return { volume: this.volume, isMuted: this.isMuted };
	}

	public getCurrentAudioInfo(): AudioInfo | null {
		return this.audioInfo;
	}

	protected updateAudioInfo(info: AudioInfo | null): void {
		if (JSON.stringify(info) === JSON.stringify(this.audioInfo)) return;
		this.audioInfo = info;
		this.dispatch("audioInfoChange", info);
		if (info) {
			this.internal.dispatch("audioInfo", {
				seq: this.trackSeq,
				stream: this.audioStream,
				info,
			});
		}
	}

	/**
	 * 告诉后端这首歌确定读不到规格，音质行可以退回专辑名了
	 */
	protected reportAudioUnavailable(): void {
		if (!this.currentSong) return;
		this.internal.dispatch("audioInfo", {
			seq: this.trackSeq,
			stream: 0,
			info: { ncmId: this.currentSong.ncmId },
		});
	}

	/** 切到新歌时调用，此时 `trackSeq` 已经是新歌的序号 */
	protected onTrackChanged(): void {}

	/** 规格是否还在路上 */
	protected isAudioPending(): boolean {
		return false;
	}

	public getTrackSnapshot(): TrackSnapshot | null {
		const song = this.currentSong;
		if (!song) return null;

		const audio = this.audioInfo?.ncmId === song.ncmId ? this.audioInfo : null;
		return {
			seq: this.trackSeq,
			song,
			audio,
			audioStream: audio ? this.audioStream : 0,
			audioPending: audio === null && this.isAudioPending(),
			status: this.playState,
			positionMs: this.musicPlayProgress,
		};
	}

	public getTrackCover(): TrackCover | null {
		return this.trackCover?.seq === this.trackSeq ? this.trackCover : null;
	}

	public resendPendingRequests(): void {}

	public applyAudioHeader(_result: AudioHeaderResult): void {}

	public setResolution(resolution: string): void {
		this.resolutionSetting = resolution;
	}

	public stop(): void {
		this.pause();
		this.seekTo(0);
	}

	public toggleShuffle(): void {
		const currentMode = this.getPlayMode();
		const nextMode = this.playModeController.getNextShuffleMode(currentMode);
		this.applyInternalPlayMode(nextMode);
	}

	public toggleRepeat(): void {
		const currentMode = this.getPlayMode();
		const nextMode = this.playModeController.getNextRepeatMode(currentMode);
		this.applyInternalPlayMode(nextMode);
	}

	public setRepeatMode(mode: RepeatMode): void {
		const currentMode = this.getPlayMode();
		const nextMode = this.playModeController.getRepeatMode(mode, currentMode);
		this.applyInternalPlayMode(nextMode);
	}

	protected processSongInfoChange(currentSongInfo: SongInfo | null): void {
		if (!currentSongInfo) {
			return;
		}

		const isNewSong =
			String(currentSongInfo.ncmId) !== String(this.lastDispatchedSongId);

		const currentCoverUrl = currentSongInfo.cover?.url;
		const isCoverChanged = currentCoverUrl !== this.lastDispatchedCoverUrl;

		if (isNewSong || isCoverChanged) {
			this.lastDispatchedSongId = currentSongInfo.ncmId;
			this.lastDispatchedCoverUrl = currentCoverUrl;

			this.currentSong = currentSongInfo;

			if (isNewSong) {
				this.trackSeq++;
				this.trackCover = null;
				this.musicPlayProgress = 0;
				if (currentSongInfo.duration && currentSongInfo.duration > 0) {
					this.musicDuration = currentSongInfo.duration;
				} else {
					this.musicDuration = 0;
				}
				this.onTrackChanged();
			}

			// 后端不等封面：文字、规格和进度现在就发，封面下载好了再单独补上
			const snapshot = this.getTrackSnapshot();
			if (snapshot) {
				this.internal.dispatch("track", snapshot);
			}

			if (isNewSong) {
				this.dispatchTimelineUpdateNow();
			}

			const seq = this.trackSeq;
			this.coverManager
				.getCover(currentSongInfo, this.resolutionSetting)
				.then((result) => {
					// 按播放序号认领：A→B→A 时，第一次 A 的封面不能贴到第二次 A 上
					if (seq === this.trackSeq) {
						this.trackCover = { seq, cover: result.cover };
						this.internal.dispatch("cover", this.trackCover);
					}

					if (
						String(result.songInfo.ncmId) === String(this.lastDispatchedSongId)
					) {
						this.dispatch("songChange", {
							...result.songInfo,
							cover: result.cover,
						});
					}
				})
				.catch((error: Error) => {
					if (error.name === "AbortError") {
						return;
					}

					logger.error(`获取封面时错误: ${error.message}`, "BaseNcmAdapter");
				});
		}
	}

	protected updatePlayState(newState: PlaybackStatus): void {
		if (this.playState !== newState) {
			this.playState = newState;
			this.dispatch("playStateChange", this.playState);
		}
	}

	/**
	 * 判定一次原生进度事件是否属于当前曲目
	 *
	 * playId 形如 "${songId}_${suffix}"；切歌后旧音频管线仍会短暂推送旧
	 * 曲目的进度，归属不符时必须丢弃，否则会把新曲的进度锚点盖回旧值。
	 * 解析失败或尚未建立曲目标识时按旧行为放行（fail-open）。
	 */
	protected isProgressForCurrentTrack(playId: string | undefined): boolean {
		if (!playId) return true;
		const eventSongId = Number.parseInt(playId, 10);
		if (Number.isNaN(eventSongId)) return true;
		if (this.lastDispatchedSongId === null) return true;
		return String(eventSongId) === String(this.lastDispatchedSongId);
	}

	protected updateTimeline(currentTime: number, totalTime?: number): void {
		this.musicPlayProgress = currentTime;
		if (totalTime !== undefined && totalTime > 0) {
			this.musicDuration = totalTime;
		}

		this.dispatch("rawTimelineUpdate", {
			currentTime: this.musicPlayProgress,
			totalTime: this.musicDuration,
		});

		// 用时间戳而不是定时器来限流：窗口最小化时定时器会被大幅推迟，
		// 靠定时器解除限流的话进度更新会跟着卡住
		if (performance.now() - this.lastTimelineDispatch >= TIMELINE_INTERVAL_MS) {
			this.dispatchTimelineUpdateNow();
		}
	}

	protected resetTimelineThrottle(): void {
		this.lastTimelineDispatch = Number.NEGATIVE_INFINITY;
	}

	protected dispatchTimelineUpdateNow(): void {
		this.lastTimelineDispatch = performance.now();
		this.dispatch("timelineUpdate", {
			currentTime: this.musicPlayProgress,
			totalTime: this.musicDuration,
		});
		this.dispatchInternalTimeline("progress");
	}

	/**
	 * 用户主动跳转。公开的 `timelineUpdate` 不在这里派发（紧随其后的进度事件
	 * 会派发），但后端需要知道这是一次跳转，哪怕只跳了一点点
	 */
	protected dispatchSeek(): void {
		this.dispatchInternalTimeline("seek");
	}

	private dispatchInternalTimeline(reason: TimelineReason): void {
		this.internal.dispatch("timeline", {
			seq: this.trackSeq,
			currentTime: this.musicPlayProgress,
			totalTime: this.musicDuration,
			reason,
		});
	}

	protected updateVolume(volume: number, isMuted: boolean): void {
		if (this.volume !== volume || this.isMuted !== isMuted) {
			this.volume = volume;
			this.isMuted = isMuted;

			this.dispatch("volumeChange", {
				volume: this.volume,
				isMuted: this.isMuted,
			});
		}
	}

	public override addEventListener<T extends keyof PlaybackEventMap & string>(
		type: T,
		listener: TypedEventListenerOrEventListenerObject<
			PlaybackEventMap,
			T
		> | null,
		options?: boolean | AddEventListenerOptions,
	): void {
		super.addEventListener(type, listener, options);

		// 主要是为了让其他插件使用者可以直接 addEventListener("audioDataUpdate", ...)
		// 或者 removeEventListener("audioDataUpdate", ...) 而不需要先调用其他方法
		// 或者用别的特殊通道来开启音频管线和监听音频数据
		if (type === "audioDataUpdate" && listener) {
			const targetListener = listener as AudioDataListener;

			const isNew = !this.audioDataListeners.has(targetListener);
			if (isNew) {
				this.audioDataListeners.add(targetListener);

				if (this.audioDataListeners.size === 1) {
					this.onAudioDataSubscriptionStarted();
				}
			}
		}
	}

	public override removeEventListener<
		T extends keyof PlaybackEventMap & string,
	>(
		type: T,
		callback: TypedEventListenerOrEventListenerObject<
			PlaybackEventMap,
			T
		> | null,
		options?: EventListenerOptions | boolean,
	): void {
		super.removeEventListener(type, callback, options);

		if (type === "audioDataUpdate" && callback) {
			const targetCallback = callback as AudioDataListener;

			if (this.audioDataListeners.has(targetCallback)) {
				this.audioDataListeners.delete(targetCallback);

				if (this.audioDataListeners.size === 0) {
					this.onAudioDataSubscriptionEnded();
				}
			}
		}
	}
}
