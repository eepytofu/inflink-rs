import type {
	PlaybackStatus,
	PlayMode,
	RepeatMode,
	SongType,
	VolumeInfo,
} from "./api";

export type ControlMessage =
	| { type: "Play" }
	| { type: "Pause" }
	| { type: "Stop" }
	| { type: "NextSong" }
	| { type: "PreviousSong" }
	| { type: "Seek"; position_ms: number }
	| { type: "ToggleShuffle" }
	| { type: "ToggleRepeat" }
	| { type: "SetRepeat"; mode: RepeatMode }
	| { type: "SetVolume"; level: number }
	| { type: "ToggleMute" };

export type SmtcEvent =
	| { type: "Play" }
	| { type: "Pause" }
	| { type: "Stop" }
	| { type: "NextSong" }
	| { type: "PreviousSong" }
	| { type: "ToggleShuffle" }
	| { type: "ToggleRepeat" }
	| { type: "Seek"; position_ms: number };

/**
 * FFI 边界使用的元数据类型
 */
export interface MetadataPayload {
	songName: string;
	albumName: string;
	authorName: string;
	cover: MetadataCoverPayload | null;
	ncmId: number;
	duration?: number | undefined;
	artists?: MetadataArtistPayload[] | undefined;
	albumId?: number | undefined;
	transName?: string | undefined;
	kind?: SongType | undefined;
}

export interface MetadataArtistPayload {
	name: string;
	id?: number | undefined;
	transName?: string | undefined;
}

export interface AudioInfoPayload {
	ncmId: number;
	codec?: string | undefined;
	bitrate?: number | undefined;
	sampleRate?: number | undefined;
	bitDepth?: number | undefined;
	level?: string | undefined;
}

export interface MetadataCoverPayload {
	/** 封面地址。只有在二进制通道没送成时才会用到（后端直接按这个地址取图） */
	url?: string | undefined;
}
export interface PlayStatePayload {
	status: PlaybackStatus;
}
export interface TimelinePayload {
	currentTime: number;
	totalTime: number;
}

export interface PlayModePayload extends PlayMode {}
export interface VolumePayload extends VolumeInfo {}

export type AppMessage = {
	UpdateMetadata: MetadataPayload;
	UpdateAudioInfo: AudioInfoPayload;
	UpdatePlayState: PlayStatePayload;
	UpdateTimeline: TimelinePayload;
	UpdatePlayMode: PlayModePayload;

	EnableSmtc: undefined;
	DisableSmtc: undefined;

	EnableDiscord: undefined;
	DisableDiscord: undefined;
	DiscordConfig: DiscordConfigPayload;
};

export type DiscordDisplayMode = "Name" | "State" | "Details";

export type DiscordThirdLine =
	| "Album"
	| "Tier"
	| "TierAndAlbum"
	| "Full"
	| "Compact";
export type DiscordArtistSeparator = "Comma" | "Slash";

export interface DiscordConfigPayload {
	showWhenPaused: boolean;
	displayMode: DiscordDisplayMode;
	appNameMode: DiscordAppNameMode;
	thirdLine: DiscordThirdLine;
	artistSeparator: DiscordArtistSeparator;
	showTranslation: boolean;
	links: boolean;
}

export type DiscordAppNameModeType =
	| "Default"
	| "DefaultEn"
	| "Song"
	| "Artist"
	| "Album"
	| "Custom";

export type DiscordAppNameMode =
	| { type: "Default" }
	| { type: "DefaultEn" }
	| { type: "Song" }
	| { type: "Artist" }
	| { type: "Album" }
	| { type: "Custom"; value: string };

export type CommandResult = {
	status: "Success" | "Error";
	message?: string;
};

export type LogEntry = {
	level: "INFO" | "WARN" | "ERROR" | "DEBUG" | "TRACE";
	message: string;
	target: string;
};
