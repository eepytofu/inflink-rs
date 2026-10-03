import { useAtomValue } from "jotai";
import { useEffect, useRef } from "react";
import { NativeBackendInstance } from "../services/NativeBackend";
import { appConfigAtom } from "../store";
import type { PlaybackEventMap } from "../types/api";
import type { ControlMessage } from "../types/backend";
import { handleAdapterCommand } from "./handleAdapterCommand";
import type { AdapterState } from "./useInfoProvider";

export function useBackendConnection(adapterState: AdapterState) {
	const { adapter, status } = adapterState;

	const config = useAtomValue(appConfigAtom);
	const {
		smtcEnabled,
		discordEnabled,
		discordShowPaused,
		discordDisplayMode,
		appNameMode,
		discordThirdLine,
		discordArtistSeparator,
		discordShowTranslation,
		discordLinks,
	} = config;

	const hasSentInitialMetadata = useRef(false);

	const configRef = useRef(config);
	useEffect(() => {
		configRef.current = config;
	}, [config]);

	const shouldConnect =
		status === "ready" && adapter && (smtcEnabled || discordEnabled);

	useEffect(() => {
		if (!shouldConnect || !adapter) {
			NativeBackendInstance.disable();
			hasSentInitialMetadata.current = false;
			return;
		}

		const nativeBackend = NativeBackendInstance;

		const onSongChange = async (e: PlaybackEventMap["songChange"]) => {
			await nativeBackend.update(e.detail);
			if (!hasSentInitialMetadata.current) {
				hasSentInitialMetadata.current = true;
				if (configRef.current.smtcEnabled) {
					nativeBackend.enableSmtcSession();
				}
			}
		};
		const onPlayStateChange = (e: PlaybackEventMap["playStateChange"]) =>
			nativeBackend.updatePlayState(e.detail);
		const onTimelineUpdate = (e: PlaybackEventMap["timelineUpdate"]) =>
			nativeBackend.updateTimeline(e.detail);
		const onPlayModeChange = (e: PlaybackEventMap["playModeChange"]) =>
			nativeBackend.updatePlayMode(e.detail);
		const onAudioInfoChange = (e: PlaybackEventMap["audioInfoChange"]) => {
			// 没有规格时不用通知后端，后端只采用与当前歌曲 ID 相符的规格
			if (e.detail) nativeBackend.updateAudioInfo(e.detail);
		};

		const onControl = (msg: ControlMessage) => {
			handleAdapterCommand(adapter, msg);
		};

		adapter.addEventListener("songChange", onSongChange);
		adapter.addEventListener("playStateChange", onPlayStateChange);
		adapter.addEventListener("timelineUpdate", onTimelineUpdate);
		adapter.addEventListener("playModeChange", onPlayModeChange);
		adapter.addEventListener("audioInfoChange", onAudioInfoChange);

		nativeBackend.initialize(onControl);

		// 规格通常在后端连接之前就读到了
		const currentAudioInfo = adapter.getCurrentAudioInfo();
		if (currentAudioInfo) {
			nativeBackend.updateAudioInfo(currentAudioInfo);
		}

		return () => {
			adapter.removeEventListener("audioInfoChange", onAudioInfoChange);
			adapter.removeEventListener("songChange", onSongChange);
			adapter.removeEventListener("playStateChange", onPlayStateChange);
			adapter.removeEventListener("timelineUpdate", onTimelineUpdate);
			adapter.removeEventListener("playModeChange", onPlayModeChange);
			nativeBackend.disable();
			hasSentInitialMetadata.current = false;
		};
	}, [shouldConnect, adapter]);

	useEffect(() => {
		if (!shouldConnect) return;

		const nativeBackend = NativeBackendInstance;

		if (smtcEnabled) {
			nativeBackend.enableSmtcSession();
		} else {
			nativeBackend.disableSmtcSession();
		}

		if (discordEnabled) {
			nativeBackend.enableDiscordRpc();
		} else {
			nativeBackend.disableDiscordRpc();
		}

		nativeBackend.updateDiscordConfig({
			showWhenPaused: discordShowPaused,
			displayMode: discordDisplayMode,
			appNameMode: appNameMode,
			thirdLine: discordThirdLine,
			artistSeparator: discordArtistSeparator,
			showTranslation: discordShowTranslation,
			links: discordLinks,
		});
	}, [
		shouldConnect,
		smtcEnabled,
		discordEnabled,
		discordShowPaused,
		discordDisplayMode,
		appNameMode,
		discordThirdLine,
		discordArtistSeparator,
		discordShowTranslation,
		discordLinks,
	]);
}
