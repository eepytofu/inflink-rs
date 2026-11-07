import { useAtomValue } from "jotai";
import { useCallback, useEffect, useRef } from "react";
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

	// 前端持有的 SMTC 可见性镜像: 发出 EnableSmtc 后为 true, Disable 后为 false。
	// 后端是纯转发的中间人, "卡片何时亮起"完全由这里决定。
	const smtcActiveRef = useRef(false);
	const deliveredSeqRef = useRef<number | null>(null);

	const configRef = useRef(config);
	useEffect(() => {
		configRef.current = config;
	}, [config]);

	const shouldConnect =
		status === "ready" && adapter && (smtcEnabled || discordEnabled);

	// 全项目唯一发出 EnableSmtc 的地方。
	// 调用方必须保证此刻会话里已有当前歌的元数据 (刚送达或此前已送达),
	// 否则系统媒体浮层会亮出一张没有歌名与封面的空白卡片。
	const activateSmtcIfNeeded = useCallback(() => {
		if (!configRef.current.smtcEnabled || smtcActiveRef.current) return;
		smtcActiveRef.current = NativeBackendInstance.enableSmtcSession();
	}, []);

	const deliverTrack = useCallback((snapshot: TrackSnapshot) => {
		deliveredSeqRef.current = NativeBackendInstance.updateTrack(snapshot)
			? snapshot.seq
			: null;
	}, []);

	useEffect(() => {
		if (!adapter) {
			return;
		}

		const nativeBackend = NativeBackendInstance;

		// 后端走内部事件，不走公开的 songChange：那个事件要等封面下载完才派发
		const onTrack = (e: InternalEventMap["track"]) => {
			deliverTrack(e.detail);
			if (deliveredSeqRef.current !== null) activateSmtcIfNeeded();
		};
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

		// 后端随页面常驻: 无论功能开关与否都初始化。dispatcher 空闲时转发成本
		// 可忽略, Discord 线程未启用时不建立连接; 只有这样, 双关期间隐藏的会话
		// 才能持续跟进当前播放, "会话常驻"的前提才对从未启用过的用户也成立。
		nativeBackend.initialize(onControl);

		// 适配器通常在后端连接之前就已经读到了当前歌曲，那时派发的事件没有人接
		const snapshot = adapter.getTrackSnapshot();
		if (snapshot) deliverTrack(snapshot);
		nativeBackend.updatePlayState(adapter.getPlaybackStatus());
		nativeBackend.updatePlayMode(adapter.getPlayMode());
		const timeline = adapter.getTimelineInfo();
		if (snapshot) {
			if (timeline) {
				nativeBackend.updateTimeline({
					...timeline,
					seq: snapshot.seq,
					reason: "progress",
				});
			}
			if (deliveredSeqRef.current !== null) activateSmtcIfNeeded();
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
			deliveredSeqRef.current = null;
			if (smtcActiveRef.current) {
				smtcActiveRef.current = false;
				nativeBackend.disableSmtcSession();
			}
		};
	}, [adapter, activateSmtcIfNeeded, deliverTrack]);

	useEffect(() => {
		const nativeBackend = NativeBackendInstance;

		if (!shouldConnect) {
			smtcActiveRef.current = false;
			nativeBackend.disableSmtcSession();
			nativeBackend.disableDiscordRpc();
			return;
		}

		if (smtcEnabled && adapter) {
			const snapshot = adapter.getTrackSnapshot();
			if (snapshot && deliveredSeqRef.current !== snapshot.seq) {
				deliverTrack(snapshot);
			}
			if (snapshot && deliveredSeqRef.current === snapshot.seq) {
				activateSmtcIfNeeded();
			}
		} else {
			smtcActiveRef.current = false;
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
		adapter,
		activateSmtcIfNeeded,
		deliverTrack,
	]);
}
