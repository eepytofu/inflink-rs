import type {
	AudioHeaderRequest,
	TimelineReason,
	TrackSnapshot,
} from "@/adapters/adapter";
import type {
	AudioInfo,
	CoverInfo,
	PlaybackStatus,
	RepeatMode,
	SongInfo,
	TimelineInfo,
} from "@/types/api";
import type {
	AppMessage,
	AudioInfoPayload,
	BackendEvent,
	CommandResult,
	DiscordConfigPayload,
	LogEntry,
	MetadataPayload,
} from "../types/backend";
import type { LogLevel } from "../utils/logger";
import logger from "../utils/logger";

const NATIVE_API_PREFIX = "inflink.";

interface NativeApiMap {
	initialize: (args?: []) => void;
	terminate: (args?: []) => void;
	registerLogger: (args: [callback: (logJson: string) => void]) => void;
	registerEventCallback: (
		args: [callback: (eventJson: string) => void],
	) => void;
	setLogLevel: (args: [level: LogLevel]) => void;
	dispatch: (args: [commandJson: string]) => string;
	dispatchWithArrayBuffer: (
		args: [
			commandJson: string,
			size: number,
			callback: (buffer: ArrayBuffer) => void,
		],
	) => string;
}

const ALL_LOG_LEVELS: Readonly<LogLevel[]> = [
	"error",
	"warn",
	"info",
	"debug",
	"trace",
];

function isLogLevel(level: string): level is LogLevel {
	return ALL_LOG_LEVELS.some((l) => l === level);
}

class NativeBackend {
	private isActive = false;
	private coverGeneration = 0;

	private call<K extends keyof NativeApiMap>(
		func: K,
		...args: Parameters<NativeApiMap[K]>
	): ReturnType<NativeApiMap[K]> {
		const nativeArgs = args[0] ?? [];
		return betterncm_native.native_plugin.call<ReturnType<NativeApiMap[K]>>(
			`${NATIVE_API_PREFIX}${func}`,
			nativeArgs,
		);
	}

	private dispatch<T extends keyof AppMessage>(
		type: T,
		payload: AppMessage[T],
	) {
		this.handleCommandResult(
			type as string,
			this.call("dispatch", [JSON.stringify({ type, payload })]),
		);
	}

	/**
	 * 处理后端对一条命令的返回结果（`dispatch` 与 `dispatchWithArrayBuffer` 共用）
	 *
	 * 返回解析出的结果；解析不出来时返回 `undefined`。
	 */
	private handleCommandResult(
		type: string,
		resultJson: string,
	): CommandResult | undefined {
		if (!resultJson) {
			logger.error(`命令 '${type}' 未收到任何返回结果。`, "Native Bridge");
			return undefined;
		}

		try {
			const result: CommandResult = JSON.parse(resultJson);
			if (result.status === "Error") {
				logger.error(
					`后端执行命令 '${type}' 时发生错误:`,
					"Native Bridge",
					result.message,
				);
			}
			return result;
		} catch (e) {
			logger.error(
				`解析后端返回结果失败:`,
				"Native Bridge",
				e,
				"\n原始结果:",
				resultJson,
			);
			return undefined;
		}
	}

	public initialize(control_handler: (msg: BackendEvent) => void) {
		if (this.isActive) return;
		this.call("terminate");

		this.isActive = true;
		this.registerLogger();
		this.call("initialize");

		window.addEventListener("beforeunload", () => {
			if (this.isActive) {
				this.disableDiscordRpc();
				this.disableSmtcSession();
				this.call("terminate");
			}
		});

		const eventCallback = (eventJson: string) => {
			try {
				const event: BackendEvent = JSON.parse(eventJson);
				control_handler(event);
			} catch (e) {
				logger.error("解析后端事件失败:", "Native Bridge", e);
			}
		};

		this.call("registerEventCallback", [eventCallback]);
	}

	public setBackendLogLevel(level: LogLevel) {
		this.call("setLogLevel", [level]);
		logger.info(`设置后端日志级别为: ${level}`, "Native Bridge");
	}

	private registerLogger() {
		const logCallback = (logJson: string) => {
			try {
				const entry: LogEntry = JSON.parse(logJson);
				const level = entry.level.toLowerCase();

				if (!isLogLevel(level)) {
					logger.log(`[InfLink BE|${entry.target}] ${entry.message}`);
					return;
				}

				const pluginPart = "InfLink BE";
				const sourcePart = entry.target;

				const badgePluginCss = [
					"color: white",
					"background-color: #946143ff",
					"padding: 1px 4px",
					"border-radius: 3px 0 0 3px",
					"font-weight: bold",
				].join(";");

				const badgeSourceCss = [
					"color: white",
					"background-color: #5a6268",
					"padding: 1px 4px",
					"border-radius: 0 3px 3px 0",
				].join(";");

				const logMethod = console[level] ?? console.log;
				logMethod(
					`%c${pluginPart}%c${sourcePart}`,
					badgePluginCss,
					badgeSourceCss,
					entry.message,
				);
			} catch (e) {
				logger.error("解析后端日志失败:", "Native Bridge", e);
			}
		};
		this.call("registerLogger", [logCallback]);
	}

	public disable() {
		if (!this.isActive) return;
		this.isActive = false;

		this.call("terminate");
		logger.info("已终止后端", "Native Bridge");
	}

	public enableSmtcSession() {
		if (!this.isActive) return;
		this.dispatch("EnableSmtc", undefined);
		logger.info("启用 SMTC 会话", "Native Bridge");
	}

	public disableSmtcSession() {
		if (!this.isActive) return;
		this.dispatch("DisableSmtc", undefined);
		logger.info("禁用 SMTC 会话", "Native Bridge");
	}

	public enableDiscordRpc() {
		if (!this.isActive) return;
		this.dispatch("EnableDiscord", undefined);
		logger.info("启用 Discord RPC", "Native Bridge");
	}

	public disableDiscordRpc() {
		if (!this.isActive) return;
		this.dispatch("DisableDiscord", undefined);
		logger.info("禁用 Discord RPC", "Native Bridge");
	}

	public updateDiscordConfig(config: DiscordConfigPayload) {
		if (!this.isActive) return;
		this.dispatch("DiscordConfig", config);
		logger.debug(`更新 Discord 配置`, "Native Bridge", config);
	}

	/**
	 * 发送切歌快照：文字、规格、播放状态和进度一次给齐，不等封面
	 */
	public updateTrack(snapshot: TrackSnapshot) {
		if (!this.isActive) return;
		this.dispatch("UpdateTrack", {
			seq: snapshot.seq,
			metadata: this.toMetadataPayload(snapshot.song),
			audio: snapshot.audio
				? this.toAudioInfoPayload(
						snapshot.seq,
						snapshot.audioStream,
						snapshot.audio,
					)
				: null,
			audioPending: snapshot.audioPending,
			status: snapshot.status,
			positionMs: snapshot.positionMs,
		});
	}

	/**
	 * 封面下载好之后单独发给后端（只有 SMTC 用得上，Discord 自己按地址取图）
	 *
	 * 拿到封面字节时走 `dispatchWithArrayBuffer`：一次调用里同时交二进制和命令，
	 * 后端读回字节后直接把它挂到这条命令上，二者不可能错配。
	 * 没有字节（下载失败或超时）时退回普通 `dispatch`，封面交给后端按 URL 取。
	 */
	public async updateCover(
		seq: number,
		cover: CoverInfo | null,
	): Promise<void> {
		if (!this.isActive) return;
		this.coverGeneration++;
		const generation = this.coverGeneration;

		let coverBytes: Uint8Array | undefined;

		if (cover?.blob) {
			try {
				coverBytes = new Uint8Array(await cover.blob.arrayBuffer());

				// 等待期间可能有更新的封面插了进来, 这时候这次更新已经没有意义了
				if (generation !== this.coverGeneration || !this.isActive) return;
			} catch (e) {
				logger.warn(
					`读取封面二进制数据失败: ${(e as Error).message}`,
					"Native Bridge",
				);
			}
		}

		const payload = { seq, url: cover?.url };

		if (!coverBytes || coverBytes.byteLength === 0) {
			// 既没有字节也没有地址：后端在切歌时已经把封面清空了，不用再发
			if (payload.url) this.dispatch("UpdateCover", payload);
			return;
		}

		const bytes = coverBytes;
		const command = JSON.stringify({ type: "UpdateCover", payload });
		const result = this.handleCommandResult(
			"UpdateCover",
			this.call("dispatchWithArrayBuffer", [
				command,
				bytes.byteLength,
				(target: ArrayBuffer) => {
					new Uint8Array(target).set(bytes);
				},
			]),
		);

		if (result?.status === "Success") {
			logger.debug(
				`封面二进制数据已随命令送达后端 (${bytes.byteLength} 字节)`,
				"Native Bridge",
			);
		}
	}

	private toMetadataPayload(songInfo: SongInfo): MetadataPayload {
		return {
			songName: songInfo.songName,
			albumName: songInfo.albumName,
			authorName: songInfo.authorName,
			cover: songInfo.cover?.url ? { url: songInfo.cover.url } : null,
			ncmId: songInfo.ncmId,
			duration: songInfo.duration,
			artists: songInfo.artists,
			albumId: songInfo.albumId,
			transName: songInfo.transName,
			kind: songInfo.type,
		};
	}

	private toAudioInfoPayload(
		seq: number,
		stream: number,
		info: AudioInfo,
	): AudioInfoPayload {
		return {
			seq,
			stream,
			ncmId: info.ncmId,
			codec: info.codec,
			bitrate: info.bitrate,
			sampleRate: info.sampleRate,
			bitDepth: info.bitDepth,
			level: info.level,
		};
	}

	public updateAudioInfo(seq: number, stream: number, info: AudioInfo) {
		if (!this.isActive) return;
		this.dispatch(
			"UpdateAudioInfo",
			this.toAudioInfoPayload(seq, stream, info),
		);
	}

	public probeAudioHeader(request: AudioHeaderRequest) {
		if (!this.isActive) return;
		this.dispatch("ProbeAudioHeader", request);
	}

	public updatePlayState(status: PlaybackStatus) {
		this.dispatch("UpdatePlayState", { status });
	}

	public updateTimeline(
		timeline: TimelineInfo & { seq: number; reason: TimelineReason },
	) {
		this.dispatch("UpdateTimeline", {
			currentTime: timeline.currentTime,
			totalTime: timeline.totalTime,
			seq: timeline.seq,
			reason: timeline.reason,
		});
	}

	public updatePlayMode(playMode: {
		isShuffling: boolean;
		repeatMode: RepeatMode;
	}) {
		this.dispatch("UpdatePlayMode", playMode);
	}
}
export const NativeBackendInstance = new NativeBackend();
