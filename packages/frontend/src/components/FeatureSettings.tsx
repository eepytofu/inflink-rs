/**
 * @fileoverview
 * SMTC 和 Discord 的设置
 */

import { useAtom } from "jotai";
import {
	AudioLines,
	AudioWaveform,
	Bug,
	Database,
	Edit,
	ExternalLink,
	Globe,
	Headset,
	Languages,
	Link,
	MonitorPlay,
	Palette,
	PauseCircle,
	Terminal,
	Users,
} from "lucide-react";
import { useEffect, useState } from "react";
import type {
	DiscordAppNameModeType,
	DiscordArtistSeparator,
	DiscordDisplayMode,
	DiscordThirdLine,
} from "@/types/backend";
import { LANGUAGE_OPTIONS, type Language, useT } from "../i18n";
import {
	backendLogLevelAtom,
	discordAppNameModeTypeAtom,
	discordArtistSeparatorAtom,
	discordCustomAppNameTextAtom,
	discordDisplayModeAtom,
	discordEnabledAtom,
	discordLinksAtom,
	discordShowPausedAtom,
	discordShowTranslationAtom,
	discordThirdLineAtom,
	frontendLogLevelAtom,
	internalLoggingAtom,
	languageAtom,
	resolutionAtom,
	smtcEnabledAtom,
	toThirdLine,
} from "../store";
import type { LogLevel } from "../utils/logger";
import { AnimatedLink } from "./AnimatedLink";
import { Combobox } from "./Combobox";
import styles from "./FeatureSettings.module.css";
import { Input } from "./Input";
import { SettingItem } from "./SettingItem";
import { Switch } from "./Switch";

export function FeatureSettings() {
	const t = useT();
	const [language, setLanguage] = useAtom(languageAtom);

	const [smtcEnabled, setSmtcEnabled] = useAtom(smtcEnabledAtom);
	const [resolution, setResolution] = useAtom(resolutionAtom);
	const [localResolution, setLocalResolution] = useState(resolution);

	const [discordEnabled, setDiscordEnabled] = useAtom(discordEnabledAtom);
	const [discordShowPaused, setDiscordShowPaused] = useAtom(
		discordShowPausedAtom,
	);
	const [discordDisplayMode, setDiscordDisplayMode] = useAtom(
		discordDisplayModeAtom,
	);
	const [appNameModeType, setAppNameModeType] = useAtom(
		discordAppNameModeTypeAtom,
	);
	const [customAppNameText, setCustomAppNameText] = useAtom(
		discordCustomAppNameTextAtom,
	);
	const [thirdLine, setThirdLine] = useAtom(discordThirdLineAtom);
	const [artistSeparator, setArtistSeparator] = useAtom(
		discordArtistSeparatorAtom,
	);
	const [showTranslation, setShowTranslation] = useAtom(
		discordShowTranslationAtom,
	);
	const [links, setLinks] = useAtom(discordLinksAtom);

	const [localCustomText, setLocalCustomText] = useState(customAppNameText);

	const [frontendLogLevel, setFrontendLogLevel] = useAtom(frontendLogLevelAtom);
	const [backendLogLevel, setBackendLogLevel] = useAtom(backendLogLevelAtom);
	const [internalLogging, setInternalLogging] = useAtom(internalLoggingAtom);

	const logLevels: LogLevel[] = ["trace", "debug", "info", "warn", "error"];
	const logLevelOptions = logLevels.map((level) => ({
		label: level,
		value: level,
	}));

	useEffect(() => {
		setLocalCustomText(customAppNameText);
	}, [customAppNameText]);

	useEffect(() => {
		setLocalResolution(resolution);
	}, [resolution]);

	const handleCustomTextCommit = () => {
		if (localCustomText !== customAppNameText) {
			setCustomAppNameText(localCustomText);
		}
	};

	const handleCustomTextKeyDown = (
		e: React.KeyboardEvent<HTMLInputElement>,
	) => {
		if (e.key === "Enter") {
			handleCustomTextCommit();
			if (e.target instanceof HTMLElement) {
				e.target.blur();
			}
		}
	};

	const handleResCommit = () => {
		const val = localResolution.trim();
		if (val && (val.toLowerCase() === "max" || /^\d+$/.test(val))) {
			setResolution(val.toLowerCase());
		} else {
			setLocalResolution(resolution);
		}
	};

	const resolutionOptions = [
		{ label: "300", value: "300" },
		{ label: "500", value: "500" },
		{ label: "1024", value: "1024" },
		{ label: "max", value: "max" },
	];

	const displayModeOptions = [
		{ label: t.optAppName, value: "Name" },
		{ label: t.optArtist, value: "State" },
		{ label: t.optSong, value: "Details" },
	];

	const appNameModeOptions = [
		{ label: t.optAppName, value: "Default" },
		{ label: t.optAppNameEn, value: "DefaultEn" },
		{ label: t.optSong, value: "Song" },
		{ label: t.optArtist, value: "Artist" },
		{ label: t.optAlbum, value: "Album" },
		{ label: t.optCustom, value: "Custom" },
	];

	// 直接拿实际效果当选项名，比起名字更一目了然
	const thirdLineOptions = [
		{ label: t.optAlbum, value: "Album" },
		{ label: `Lossless · ${t.optAlbum}`, value: "TierAndAlbum" },
		{ label: "FLAC 24-bit/48 kHz, 1695 kbps", value: "Full" },
		{ label: "FLAC 24-bit/48 kHz", value: "Compact" },
	];

	const artistSeparatorOptions = [
		{ label: t.optComma, value: "Comma" },
		{ label: t.optSlash, value: "Slash" },
	];

	return (
		<div className={styles.sectionContainerSmall}>
			<SettingItem
				icon={<Globe size={20} />}
				// 两种语言都写上，看不懂当前语言的人也能找到这一行
				title="语言 / Language"
				action={
					<Combobox
						options={LANGUAGE_OPTIONS}
						value={language}
						onChange={(val) => setLanguage(val as Language)}
						editable={false}
					/>
				}
			/>

			<h3 className={`${styles.sectionTitle} ${styles.sectionContainer}`}>
				{t.smtcSection}
			</h3>

			<SettingItem
				icon={<AudioLines size={20} />}
				title={t.smtcEnable}
				description={
					<span>
						<AnimatedLink
							onClick={() => {
								betterncm.ncm.openUrl(t.smtcDocsUrl);
							}}
							icon={<ExternalLink size={14} strokeWidth={2.5} />}
						>
							{t.smtcDocsLink}
						</AnimatedLink>
					</span>
				}
				action={
					<Switch
						checked={smtcEnabled}
						onChange={(_e, checked) => setSmtcEnabled(checked)}
					/>
				}
			/>

			<SettingItem
				visible={smtcEnabled}
				icon={<MonitorPlay size={20} />}
				title={t.coverResolution}
				description={t.coverResolutionDesc}
				action={
					<Combobox
						options={resolutionOptions}
						value={localResolution}
						onChange={setLocalResolution}
						onBlur={handleResCommit}
						allowCustomValue={true}
					/>
				}
			/>

			<h3 className={`${styles.sectionTitle} ${styles.sectionContainer}`}>
				{t.discordSection}
			</h3>

			<SettingItem
				icon={<Headset size={20} />}
				title={t.discordEnable}
				description={t.discordEnableDesc}
				action={
					<Switch
						checked={discordEnabled}
						onChange={(_e, checked) => setDiscordEnabled(checked)}
					/>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<PauseCircle size={20} />}
				title={t.showPaused}
				description={
					<span>
						{t.showPausedDesc}
						<br />
						{t.showPausedNote}
					</span>
				}
				action={
					<Switch
						checked={discordShowPaused}
						onChange={(_e, checked) => setDiscordShowPaused(checked)}
					/>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<Palette size={20} />}
				title={t.displayMode}
				description={t.displayModeDesc}
				action={
					<Combobox
						options={displayModeOptions}
						value={discordDisplayMode}
						onChange={(val) => setDiscordDisplayMode(val as DiscordDisplayMode)}
						editable={false}
					/>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<Edit size={20} />}
				title={t.appName}
				description={
					<span>
						{t.appNameDesc}
						<br />
						{t.appNameNote}
					</span>
				}
				action={
					<div className={styles.flexRow}>
						<Combobox
							options={appNameModeOptions}
							value={appNameModeType}
							onChange={(val) =>
								setAppNameModeType(val as DiscordAppNameModeType)
							}
							editable={false}
						/>
						{appNameModeType === "Custom" && (
							<Input
								style={{ width: 140 }}
								placeholder={t.customNamePlaceholder}
								value={localCustomText}
								onChange={(e) => setLocalCustomText(e.target.value)}
								onBlur={handleCustomTextCommit}
								onKeyDown={handleCustomTextKeyDown}
							/>
						)}
					</div>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<AudioWaveform size={20} />}
				title={t.thirdLine}
				description={t.thirdLineDesc}
				action={
					<Combobox
						options={thirdLineOptions}
						value={toThirdLine(thirdLine)}
						onChange={(val) => setThirdLine(val as DiscordThirdLine)}
						editable={false}
					/>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<Users size={20} />}
				title={t.artistSeparator}
				description={t.artistSeparatorDesc}
				action={
					<Combobox
						options={artistSeparatorOptions}
						value={artistSeparator}
						onChange={(val) =>
							setArtistSeparator(val as DiscordArtistSeparator)
						}
						editable={false}
					/>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<Languages size={20} />}
				title={t.showTranslation}
				description={t.showTranslationDesc}
				action={
					<Switch
						checked={showTranslation}
						onChange={(_e, checked) => setShowTranslation(checked)}
					/>
				}
			/>

			<SettingItem
				visible={discordEnabled}
				icon={<Link size={20} />}
				title={t.links}
				description={t.linksDesc}
				action={
					<Switch
						checked={links}
						onChange={(_e, checked) => setLinks(checked)}
					/>
				}
			/>

			<h3 className={styles.sectionTitle}>{t.advancedSection}</h3>

			<SettingItem
				icon={<Terminal size={20} />}
				title={t.frontendLogLevel}
				action={
					<Combobox
						options={logLevelOptions}
						value={frontendLogLevel}
						onChange={(val) => setFrontendLogLevel(val as LogLevel)}
						editable={false}
					/>
				}
			/>

			<SettingItem
				icon={<Database size={20} />}
				title={t.backendLogLevel}
				action={
					<Combobox
						options={logLevelOptions}
						value={backendLogLevel}
						onChange={(val) => setBackendLogLevel(val as LogLevel)}
						editable={false}
					/>
				}
			/>

			{import.meta.env.DEV ? (
				<SettingItem
					icon={<Bug size={20} />}
					title={t.internalLogging}
					description={t.internalLoggingDesc}
					action={
						<Switch
							checked={internalLogging}
							onChange={(_e, checked) => setInternalLogging(checked)}
						/>
					}
				/>
			) : null}
		</div>
	);
}
