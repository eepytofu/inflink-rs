/**
 * @fileoverview
 * 设置界面的文案。只有两种语言，所以用一张表而不是引入 i18n 库
 */

import { useAtomValue } from "jotai";
import { languageAtom } from "./store";

const zh = {
	pageTitle: "InfLink-rs 设置",
	loading: "正在初始化...",
	initFailedTitle: "插件初始化失败",
	initFailedBody:
		"部分组件未能初始化, 请尝试重启网易云音乐, 或者打开控制台查看详细信息",
	errorMessage: "错误信息",
	unknownError: "未知错误",
	close: "关闭",

	versionWarningTitle: "InfLink-rs 可能无法在当前的网易云音乐版本上运行",
	versionWarningBody: (version: string) =>
		`InfLink-rs 未在此版本 (${version}) 上进行过测试，可能会导致功能异常或不稳定`,
	versionWarningLink: "访问 GitHub 仓库以了解更多信息",

	smtcSection: "SMTC 设置",
	smtcEnable: "启用 SMTC 集成",
	smtcDocsLink: "在微软文档中查看",
	smtcDocsUrl:
		"https://learn.microsoft.com/zh-cn/windows/uwp/audio-video-camera/integrate-with-systemmediatransportcontrols",
	coverResolution: "封面分辨率",
	coverResolutionDesc: "较高的分辨率可能会降低信息更新速度",

	discordSection: "Discord Rich Presence 设置",
	discordEnable: "启用 Discord RPC 集成",
	discordEnableDesc: "将当前播放的歌曲同步显示到 Discord 状态中",
	showPaused: "暂停时显示状态",
	showPausedDesc: "暂停时保留 Discord 状态的显示",
	showPausedNote: "注：由于 Discord 的限制，已播放时间将变为 00:00",
	displayMode: "简略信息",
	displayModeDesc: "向其他人展示的简略信息",
	appName: "自定义应用名称",
	appNameDesc: "会显示在 “Listening to” 后面",
	appNameNote:
		"如果在 “简略信息” 设置中选择了 “应用名称”，简略信息也会显示此名称",
	customNamePlaceholder: "自定义名称...",
	thirdLine: "第三行内容",
	thirdLineDesc:
		"音质是网易云实际下发的档位和规格，可能与你选择的不同，读取不到时显示专辑名",
	artistSeparator: "歌手分隔符",
	artistSeparatorDesc: "有多位歌手时用来分隔歌手名",
	showTranslation: "显示翻译名",
	showTranslationDesc: "在歌曲名和歌手名后面附上网易云提供的译名",
	links: "可点击链接",
	linksDesc: "点击歌曲名、歌手名和封面可以打开对应的网易云页面",

	advancedSection: "高级选项",
	frontendLogLevel: "前端日志级别",
	backendLogLevel: "后端日志级别",
	internalLogging: "内部日志转发",
	internalLoggingDesc: "仅供调试",

	optAppName: "应用名称",
	optAppNameEn: "应用名称（英文）",
	optSong: "歌曲名",
	optArtist: "歌手名",
	optAlbum: "专辑名",
	optCustom: "自定义文本",
	optComma: "逗号",
	optSlash: "斜杠",
};

type Dictionary = typeof zh;

const en: Dictionary = {
	pageTitle: "InfLink-rs Settings",
	loading: "Initializing...",
	initFailedTitle: "Plugin failed to initialize",
	initFailedBody:
		"Some components failed to initialize. Try restarting NetEase Cloud Music, or open the console for details",
	errorMessage: "Error",
	unknownError: "Unknown error",
	close: "Close",

	versionWarningTitle:
		"InfLink-rs may not work on this version of NetEase Cloud Music",
	versionWarningBody: (version: string) =>
		`InfLink-rs has not been tested on this version (${version}) and may misbehave or be unstable`,
	versionWarningLink: "Visit the GitHub repository for more information",

	smtcSection: "SMTC",
	smtcEnable: "Enable SMTC integration",
	smtcDocsLink: "View in Microsoft docs",
	smtcDocsUrl:
		"https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/integrate-with-systemmediatransportcontrols",
	coverResolution: "Cover resolution",
	coverResolutionDesc: "Higher resolutions may slow down metadata updates",

	discordSection: "Discord Rich Presence",
	discordEnable: "Enable Discord RPC integration",
	discordEnableDesc: "Show the current song in your Discord status",
	showPaused: "Show when paused",
	showPausedDesc: "Keep the Discord status visible while paused",
	showPausedNote:
		"Note: due to a Discord limitation, elapsed time will read 00:00",
	displayMode: "Compact status",
	displayModeDesc: "The short status other people see",
	appName: "Application name",
	appNameDesc: "Shown after “Listening to”",
	appNameNote:
		"If “Compact status” is set to “App name”, the compact status shows this name too",
	customNamePlaceholder: "Custom name...",
	thirdLine: "Third line",
	thirdLineDesc:
		"Quality is the tier and specs NetEase actually delivered, which may differ from what you selected. Falls back to the album name when unknown",
	artistSeparator: "Artist separator",
	artistSeparatorDesc: "Separates artist names when there are several",
	showTranslation: "Show translated names",
	showTranslationDesc:
		"Append NetEase's translated names after the song and artist names",
	links: "Clickable links",
	linksDesc:
		"Clicking the song, artists and cover opens the matching NetEase page",

	advancedSection: "Advanced",
	frontendLogLevel: "Frontend log level",
	backendLogLevel: "Backend log level",
	internalLogging: "Forward internal logs",
	internalLoggingDesc: "For debugging only",

	optAppName: "App name",
	optAppNameEn: "App name (English)",
	optSong: "Song",
	optArtist: "Artist",
	optAlbum: "Album",
	optCustom: "Custom text",
	optComma: "Comma",
	optSlash: "Slash",
};

const dictionaries = { zh, en };

export type Language = keyof typeof dictionaries;

export const LANGUAGE_OPTIONS: { label: string; value: Language }[] = [
	{ label: "简体中文", value: "zh" },
	{ label: "English", value: "en" },
];

export function useT(): Dictionary {
	const language = useAtomValue(languageAtom);
	return dictionaries[language] ?? zh;
}
