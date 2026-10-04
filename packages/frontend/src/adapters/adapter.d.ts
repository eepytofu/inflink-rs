import type {
	AudioInfo,
	CoverInfo,
	PlaybackEventMap,
	PlaybackStatus,
	PlayMode,
	RepeatMode,
	SongInfo,
	TimelineInfo,
	VolumeInfo,
} from "../types/api";
import type { TypedEventTarget } from "../utils/TypedEventTarget";

/**
 * 切歌瞬间的完整快照
 *
 * 文字、规格、播放状态和进度一次给齐，不等封面下载
 */
export interface TrackSnapshot {
	/**
	 * 每换一首歌就加一。A→B→A 里的两次 A 序号不同，迟到的封面和规格靠它
	 * 认领自己属于哪一次播放
	 */
	seq: number;
	/** 封面只带地址，不带下载好的数据 */
	song: SongInfo;
	audio: AudioInfo | null;
	/** `audio` 描述的是哪一个音频流，见 {@link AudioHeaderRequest.stream} */
	audioStream: number;
	/** 规格还在路上（音频地址请求还没回来） */
	audioPending: boolean;
	status: PlaybackStatus;
	positionMs: number;
}

export interface TrackCover {
	seq: number;
	cover: CoverInfo | null;
}

export type TimelineReason = "progress" | "seek";

export interface AudioHeaderRequest {
	seq: number;
	/**
	 * 每个音频流（歌曲加 MD5）一个编号，同一首歌换音质时会变
	 *
	 * 后端读到文件头之后凭它认领对应的那份规格，0 表示没有编号
	 */
	stream: number;
	ncmId: number;
	/** 音频流的 MD5，用来在缓存目录里认准文件 */
	md5: string;
	durationMs?: number | undefined;
}

export interface AudioHeaderResult {
	ncmId: number;
	md5: string;
	sampleRate: number;
	bitDepth?: number | undefined;
}

/**
 * 只给原生后端用的事件，不属于公开 API
 *
 * 公开的 `songChange` 要等封面下载完才派发，其他插件依赖这一点；后端需要的是
 * 不等封面的即时快照，所以另走一条通道。
 */
export interface InternalEventMap {
	track: CustomEvent<TrackSnapshot>;
	cover: CustomEvent<TrackCover>;
	timeline: CustomEvent<TimelineInfo & { seq: number; reason: TimelineReason }>;
	audioInfo: CustomEvent<{ seq: number; stream: number; info: AudioInfo }>;
	audioHeaderRequest: CustomEvent<AudioHeaderRequest>;
}

export interface INcmAdapter extends TypedEventTarget<PlaybackEventMap> {
	readonly internal: TypedEventTarget<InternalEventMap>;
	getTrackSnapshot(): TrackSnapshot | null;
	getTrackCover(): TrackCover | null;
	/** 后端连上之后调用：连接之前发出的请求没有人接，需要再发一次 */
	resendPendingRequests(): void;
	applyAudioHeader(result: AudioHeaderResult): void;

	initialize(): Promise<void>;
	dispose(): void;
	getCurrentSongInfo(): SongInfo | null;
	getPlaybackStatus(): PlaybackStatus;
	getTimelineInfo(): TimelineInfo | null;
	getPlayMode(): PlayMode;
	getVolumeInfo(): VolumeInfo;
	getCurrentAudioInfo(): AudioInfo | null;

	hasNativeSmtcSupport(): boolean;
	setInternalLogging(enabled: boolean): void;

	play(): void;
	pause(): void;
	stop(): void;
	nextSong(): void;
	previousSong(): void;
	seekTo(positionMs: number): void;
	toggleShuffle(): void;
	toggleRepeat(): void;
	setRepeatMode(mode: RepeatMode): void;
	setVolume(level: number): void;
	toggleMute(): void;

	setResolution(resolution: string): void;
}
