/**
 * @fileoverview InfLink-rs Plugin API
 *
 * 供其他插件使用的类型定义文件
 * 将此文件复制到你的项目中即可获得 InfLink-rs 暴露的 `window.InfLinkApi` 的类型提示
 */

export type PlaybackStatus = "Playing" | "Paused";

export interface CoverInfo {
	blob?: Blob | undefined;
	url?: string | undefined;
}

/**
 * @since 插件版本 3.4.0
 */
export interface ArtistInfo {
	name: string;
	/** 网易云的艺术家 ID。本地歌曲、播客等没有曲库 ID 的情况下不存在 */
	id?: number | undefined;
	/** 网易云提供的译名，没有时不存在 */
	transName?: string | undefined;
}

/**
 * - `song`: 曲库里的歌曲，`ncmId` 是歌曲 ID
 * - `podcast`: 播客节目，`ncmId` 是节目 ID
 * - `local`: 没有匹配到曲库的本地文件，`ncmId` 为 0
 * @since 插件版本 3.4.0
 */
export type SongType = "song" | "podcast" | "local";

export interface SongInfo {
	songName: string;
	albumName: string;
	/** 所有艺术家的名字，以 " / " 连接 */
	authorName: string;
	cover: CoverInfo | null;
	/** 歌曲ID */
	ncmId: number;
	/** 单位毫秒 */
	duration?: number | undefined;

	/**
	 * 结构化的艺术家列表，顺序与网易云一致
	 * @since 插件版本 3.4.0
	 */
	artists?: ArtistInfo[] | undefined;
	/**
	 * 网易云的专辑 ID
	 * @since 插件版本 3.4.0
	 */
	albumId?: number | undefined;
	/**
	 * 网易云提供的歌名译名
	 * @since 插件版本 3.4.0
	 */
	transName?: string | undefined;
	/**
	 * 歌曲别名（副标题），例如 "电视剧《xxx》片尾曲"
	 * @since 插件版本 3.4.0
	 */
	alias?: string[] | undefined;
	/**
	 * @since 插件版本 3.4.0
	 */
	type?: SongType | undefined;
}

/**
 * 当前音频流的真实规格
 *
 * 所有字段都来自网易云实际下发的音频流信息，拿不到的字段不存在，
 * 不会用音质档位的宣传参数去填充
 * @since 插件版本 3.4.0
 */
export interface AudioInfo {
	/** 这份规格对应的歌曲 ID，与 `SongInfo.ncmId` 一致 */
	ncmId: number;
	/** 编码格式，小写，例如 "flac"、"mp3" */
	codec?: string | undefined;
	/** 实际平均码率，单位 bit/s */
	bitrate?: number | undefined;
	/** 采样率，单位 Hz */
	sampleRate?: number | undefined;
	/** 位深 */
	bitDepth?: number | undefined;
	/** 网易云实际下发的音质档位，例如 "lossless"、"hires" */
	level?: string | undefined;
}

export interface TimelineInfo {
	/** 单位毫秒 */
	currentTime: number;
	/** 单位毫秒 */
	totalTime: number;
}

export interface VolumeInfo {
	/**
	 * 0 ~ 1 的浮点数
	 */
	volume: number;
	isMuted: boolean;
}

export type RepeatMode = "None" | "Track" | "List" | "AI";

export interface PlayMode {
	isShuffling: boolean;
	repeatMode: RepeatMode;
}

/**
 * 网易云音乐 C++ 后端抛出的音频数据
 * @since 插件版本 3.2.11
 */
export interface AudioDataInfo {
	/**
	 * 原始音频数据
	 *
	 * 这是一个 48000Hz int16 2通道的 PCM 数据
	 */
	data: ArrayBuffer;
	/**
	 * 数据对应的时间戳，单位为毫秒
	 */
	pts: number;
}

export interface PlaybackEventMap {
	songChange: CustomEvent<SongInfo>;
	playStateChange: CustomEvent<PlaybackStatus>;
	timelineUpdate: CustomEvent<TimelineInfo>;
	rawTimelineUpdate: CustomEvent<TimelineInfo>;
	playModeChange: CustomEvent<PlayMode>;
	volumeChange: CustomEvent<VolumeInfo>;

	/**
	 * 当前音频流的规格发生变化，切歌或在同一首歌内切换音质时触发
	 *
	 * 规格可能晚于 `songChange` 到达。拿不到规格时为 `null`
	 * @since 插件版本 3.4.0
	 */
	audioInfoChange: CustomEvent<AudioInfo | null>;

	/**
	 * C++ 后端抛出的音频数据
	 *
	 * 注意监听此事件可能会对性能有一定影响
	 * @since 插件版本 3.2.11
	 */
	audioDataUpdate: CustomEvent<AudioDataInfo>;
}

/**
 * 可以给其它插件用的接口
 */
export interface IInfLinkApi {
	/**
	 * 当前 InfLink 插件的版本号
	 * @since 插件版本 3.2.11
	 */
	readonly version: string;

	getPlaybackStatus(): PlaybackStatus;
	getCurrentSong(): SongInfo | null;
	getTimeline(): TimelineInfo | null;
	getPlayMode(): PlayMode;
	getVolume(): VolumeInfo;
	/**
	 * 获取当前歌曲的音频流规格，拿不到时返回 `null`
	 * @since 插件版本 3.4.0
	 */
	getCurrentAudioInfo(): AudioInfo | null;

	play(): void;
	pause(): void;
	stop(): void;
	next(): void;
	previous(): void;
	seekTo(positionMs: number): void;

	toggleShuffle(): void;
	/**
	 * 切换循环播放模式 (顺序播放 -> 列表循环 -> 单曲循环)
	 */
	toggleRepeat(): void;
	/**
	 * 设置循环播放模式
	 * @param mode "None" | "Track" | "List" | "AI"
	 */
	setRepeatMode(mode: RepeatMode): void;

	/**
	 * 设置音量
	 * @param level 音量大小，范围从 0.0 到 1.0
	 */
	setVolume(level: number): void;
	toggleMute(): void;

	addEventListener<K extends keyof PlaybackEventMap>(
		type: K,
		listener: (ev: PlaybackEventMap[K]) => void,
	): void;

	removeEventListener<K extends keyof PlaybackEventMap>(
		type: K,
		listener: (ev: PlaybackEventMap[K]) => void,
	): void;
}

declare global {
	interface Window {
		/**
		 * InfLink-rs 插件提供的、可供其他插件使用的接口
		 */
		InfLinkApi?: IInfLinkApi;
	}
}
