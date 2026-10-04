import { useAtomValue } from "jotai";
import { useEffect, useRef } from "react";
import type { InternalEventMap, TrackSnapshot } from "../adapters/adapter";
import { NativeBackendInstance } from "../services/NativeBackend";
import { appConfigAtom } from "../store";
import type { PlaybackEventMap } from "../types/api";
import type { BackendEvent } from "../types/backend";
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

		const sendTrack = (snapshot: TrackSnapshot) => {
			nativeBackend.updateTrack(snapshot);
			if (!hasSentInitialMetadata.current) {
				hasSentInitialMetadata.current = true;
				if (configRef.current.smtcEnabled) {
					nativeBackend.enableSmtcSession();
				}
			}
		};

		// 后端走内部事件，不走公开的 songChange：那个事件要等封面下载完才派发
		const onTrack = (e: InternalEventMap["track"]) => sendTrack(e.detail);
		const onCover = (e: InternalEventMap["cover"]) =>
			nativeBackend.updateCover(e.detail.seq, e.detail.cover);
		const onTimeline = (e: InternalEventMap["timeline"]) =>
			nativeBackend.updateTimeline(e.detail);
		const onAudioInfo = (e: InternalEventMap["audioInfo"]) =>
			nativeBackend.updateAudioInfo(
				e.detail.seq,
				e.detail.stream,
				e.detail.info,
			);
		const onAudioHeaderRequest = (e: InternalEventMap["audioHeaderRequest"]) =>
			nativeBackend.probeAudioHeader(e.detail);
		const onPlayStateChange = (e: PlaybackEventMap["playStateChange"]) =>
			nativeBackend.updatePlayState(e.detail);
		const onPlayModeChange = (e: PlaybackEventMap["playModeChange"]) =>
			nativeBackend.updatePlayMode(e.detail);

		const onControl = (msg: BackendEvent) => {
			if (msg.type === "AudioHeader") {
				adapter.applyAudioHeader({
					ncmId: msg.ncm_id,
					md5: msg.md5,
					sampleRate: msg.sample_rate,
					bitDepth: msg.bit_depth ?? undefined,
				});
				return;
			}
			handleAdapterCommand(adapter, msg);
		};

		adapter.internal.addEventListener("track", onTrack);
		adapter.internal.addEventListener("cover", onCover);
		adapter.internal.addEventListener("timeline", onTimeline);
		adapter.internal.addEventListener("audioInfo", onAudioInfo);
		adapter.internal.addEventListener(
			"audioHeaderRequest",
			onAudioHeaderRequest,
		);
		adapter.addEventListener("playStateChange", onPlayStateChange);
		adapter.addEventListener("playModeChange", onPlayModeChange);

		nativeBackend.initialize(onControl);

		// 适配器通常在后端连接之前就已经读到了当前歌曲，那时派发的事件没有人接
		const snapshot = adapter.getTrackSnapshot();
		if (snapshot) {
			sendTrack(snapshot);
			const cover = adapter.getTrackCover();
			if (cover) nativeBackend.updateCover(cover.seq, cover.cover);
			adapter.resendPendingRequests();
		}

		return () => {
			adapter.internal.removeEventListener("track", onTrack);
			adapter.internal.removeEventListener("cover", onCover);
			adapter.internal.removeEventListener("timeline", onTimeline);
			adapter.internal.removeEventListener("audioInfo", onAudioInfo);
			adapter.internal.removeEventListener(
				"audioHeaderRequest",
				onAudioHeaderRequest,
			);
			adapter.removeEventListener("playStateChange", onPlayStateChange);
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
