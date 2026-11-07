import { Check, Copy, X } from "lucide-react";
import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type {
	INcmAdapter,
	InternalEventMap,
	TrackSnapshot,
} from "@/adapters/adapter";
import type { PaletteMode } from "@/hooks/useNcmTheme";
import { useT } from "@/i18n";
import type {
	PlaybackEventMap,
	PlaybackStatus,
	PlayMode,
	TimelineInfo,
	VolumeInfo,
} from "@/types/api";
import { copyTextToClipboard } from "@/utils/clipboard";
import styles from "./index.module.css";

interface DebugData {
	track: TrackSnapshot | null;
	playbackStatus: PlaybackStatus | null;
	timelineInfo: TimelineInfo | null;
	playMode: PlayMode | null;
	volumeInfo: VolumeInfo | null;
}

const INITIAL_DATA: DebugData = {
	track: null,
	playbackStatus: null,
	timelineInfo: null,
	playMode: null,
	volumeInfo: null,
};

const DEFAULT_WIDTH = 480;
const DEFAULT_HEIGHT = 550;
const EDGE_MARGIN = 120;
const HEADER_MARGIN_BOTTOM = 40;

function useDebugData(adapter: INcmAdapter | null): DebugData {
	const [data, setData] = useState<DebugData>(INITIAL_DATA);

	useEffect(() => {
		if (!adapter) {
			setData(INITIAL_DATA);
			return;
		}

		setData({
			track: adapter.getTrackSnapshot(),
			playbackStatus: adapter.getPlaybackStatus(),
			timelineInfo: adapter.getTimelineInfo(),
			playMode: adapter.getPlayMode(),
			volumeInfo: adapter.getVolumeInfo(),
		});

		const onTrack = (e: InternalEventMap["track"]) =>
			setData((prev) => ({
				...prev,
				track: e.detail,
				timelineInfo: adapter.getTimelineInfo(),
			}));
		const onAudioInfoChange = () =>
			setData((prev) => ({ ...prev, track: adapter.getTrackSnapshot() }));
		const onPlayStateChange = (e: PlaybackEventMap["playStateChange"]) =>
			setData((prev) => ({ ...prev, playbackStatus: e.detail }));
		const onRawTimelineUpdate = (e: PlaybackEventMap["rawTimelineUpdate"]) =>
			setData((prev) => ({ ...prev, timelineInfo: e.detail }));
		const onTimeline = (e: InternalEventMap["timeline"]) => {
			if (e.detail.reason === "seek") {
				setData((prev) => ({ ...prev, timelineInfo: e.detail }));
			}
		};
		const onPlayModeChange = (e: PlaybackEventMap["playModeChange"]) =>
			setData((prev) => ({ ...prev, playMode: e.detail }));
		const onVolumeChange = (e: PlaybackEventMap["volumeChange"]) =>
			setData((prev) => ({ ...prev, volumeInfo: e.detail }));

		adapter.internal.addEventListener("track", onTrack);
		adapter.internal.addEventListener("timeline", onTimeline);
		adapter.addEventListener("audioInfoChange", onAudioInfoChange);
		adapter.addEventListener("playStateChange", onPlayStateChange);
		adapter.addEventListener("rawTimelineUpdate", onRawTimelineUpdate);
		adapter.addEventListener("playModeChange", onPlayModeChange);
		adapter.addEventListener("volumeChange", onVolumeChange);

		return () => {
			adapter.internal.removeEventListener("track", onTrack);
			adapter.internal.removeEventListener("timeline", onTimeline);
			adapter.removeEventListener("audioInfoChange", onAudioInfoChange);
			adapter.removeEventListener("playStateChange", onPlayStateChange);
			adapter.removeEventListener("rawTimelineUpdate", onRawTimelineUpdate);
			adapter.removeEventListener("playModeChange", onPlayModeChange);
			adapter.removeEventListener("volumeChange", onVolumeChange);
		};
	}, [adapter]);

	return data;
}

function DebugSection({
	title,
	children,
}: {
	title: string;
	children: ReactNode;
}) {
	return (
		<section className={styles.section}>
			<h4 className={styles.sectionTitle}>{title}</h4>
			<div className={styles.sectionBody}>{children}</div>
		</section>
	);
}

function DebugRow({
	label,
	value,
	mono = false,
}: {
	label: string;
	value: ReactNode;
	mono?: boolean;
}) {
	const { debug } = useT();
	const valueRef = useRef<HTMLSpanElement>(null);
	const [copied, setCopied] = useState(false);

	const handleCopy = async () => {
		const text = valueRef.current?.textContent;
		if (!text) return;
		if (await copyTextToClipboard(text)) {
			setCopied(true);
			setTimeout(() => setCopied(false), 1200);
		}
	};

	return (
		<div className={styles.row}>
			<span className={styles.label}>{label}</span>
			{value == null ? (
				<span className={`${styles.value} ${styles.valueNull}`}>—</span>
			) : (
				<span
					ref={valueRef}
					className={`${styles.value} ${mono ? styles.valueMono : ""}`}
				>
					{value}
				</span>
			)}
			<button
				type="button"
				className={styles.copyButton}
				disabled={value == null}
				onClick={handleCopy}
				aria-label={debug.copy}
				title={debug.copy}
			>
				{copied ? <Check size={12} /> : <Copy size={12} />}
			</button>
		</div>
	);
}

function formatMs(ms: number | undefined) {
	if (ms === undefined) return null;
	const s = Math.floor(ms / 1000);
	const m = Math.floor(s / 60);
	return `${m}:${String(s % 60).padStart(2, "0")} (${ms} ms)`;
}

export function DebugPanel({
	adapter,
	theme,
	onClose,
}: {
	adapter: INcmAdapter | null;
	theme: PaletteMode;
	onClose: () => void;
}) {
	const t = useT();
	const { debug } = t;
	const data = useDebugData(adapter);
	const song = data.track?.song;
	const audio = data.track?.audio;

	const panelRef = useRef<HTMLDivElement>(null);
	const dragStateRef = useRef<{
		pointerX: number;
		pointerY: number;
		left: number;
		top: number;
	} | null>(null);

	const [pos, setPos] = useState(() => ({
		left: Math.max(0, Math.round((window.innerWidth - DEFAULT_WIDTH) / 2)),
		top: Math.max(0, Math.round((window.innerHeight - DEFAULT_HEIGHT) / 2)),
	}));

	const handleHeaderPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
		if (e.button !== 0) return;
		if (e.target instanceof Element && e.target.closest("button")) return;
		dragStateRef.current = {
			pointerX: e.clientX,
			pointerY: e.clientY,
			left: pos.left,
			top: pos.top,
		};
		e.currentTarget.setPointerCapture(e.pointerId);
	};

	const handleHeaderPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
		const state = dragStateRef.current;
		if (!state) return;
		const width = panelRef.current?.offsetWidth ?? DEFAULT_WIDTH;
		const left = Math.min(
			Math.max(state.left + e.clientX - state.pointerX, EDGE_MARGIN - width),
			window.innerWidth - EDGE_MARGIN,
		);
		const top = Math.min(
			Math.max(state.top + e.clientY - state.pointerY, 0),
			Math.max(0, window.innerHeight - HEADER_MARGIN_BOTTOM),
		);
		setPos({ left, top });
	};

	const handleHeaderPointerUp = () => {
		dragStateRef.current = null;
	};

	return createPortal(
		<div
			ref={panelRef}
			data-theme={theme}
			className={styles.panel}
			style={{
				left: pos.left,
				top: pos.top,
				width: DEFAULT_WIDTH,
				height: DEFAULT_HEIGHT,
			}}
		>
			<div
				className={styles.header}
				onPointerDown={handleHeaderPointerDown}
				onPointerMove={handleHeaderPointerMove}
				onPointerUp={handleHeaderPointerUp}
				onPointerCancel={handleHeaderPointerUp}
			>
				<h4 className={styles.title}>{t.debugPanel}</h4>
				<button
					type="button"
					className={styles.closeButton}
					onClick={onClose}
					aria-label={t.close}
				>
					<X size={16} />
				</button>
			</div>

			<div className={styles.content}>
				<DebugSection title={debug.track}>
					<DebugRow label={t.optSong} value={song?.songName} />
					{song?.transName && (
						<DebugRow label={debug.translatedSong} value={song.transName} />
					)}
					<DebugRow label={t.optAlbum} value={song?.albumName} />
					{song?.albumTransName && (
						<DebugRow
							label={debug.translatedAlbum}
							value={song.albumTransName}
						/>
					)}
					<DebugRow label={t.optArtist} value={song?.authorName} />
					<DebugRow label={debug.trackId} value={song?.ncmId} mono />
					<DebugRow
						label={debug.duration}
						value={formatMs(song?.duration)}
						mono
					/>
				</DebugSection>

				<DebugSection title={debug.deliveredAudio}>
					<DebugRow
						label={debug.codec}
						value={audio?.codec?.toUpperCase()}
						mono
					/>
					<DebugRow
						label={debug.bitrate}
						value={audio?.bitrate && `${Math.round(audio.bitrate / 1000)} kbps`}
						mono
					/>
					<DebugRow
						label={debug.sampleRate}
						value={audio?.sampleRate && `${audio.sampleRate / 1000} kHz`}
						mono
					/>
					<DebugRow
						label={debug.bitDepth}
						value={audio?.bitDepth && `${audio.bitDepth}-bit`}
						mono
					/>
				</DebugSection>

				<DebugSection title={debug.playbackStatus}>
					<DebugRow label={debug.playPause} value={data.playbackStatus} mono />
				</DebugSection>

				<DebugSection title={debug.progress}>
					<DebugRow
						label={debug.position}
						value={formatMs(data.timelineInfo?.currentTime)}
						mono
					/>
					<DebugRow
						label={debug.duration}
						value={formatMs(data.timelineInfo?.totalTime)}
						mono
					/>
				</DebugSection>

				<DebugSection title={debug.playMode}>
					<DebugRow
						label={debug.shuffle}
						value={
							data.playMode == null
								? null
								: data.playMode.isShuffling
									? debug.on
									: debug.off
						}
					/>
					<DebugRow
						label={debug.repeat}
						value={data.playMode?.repeatMode}
						mono
					/>
				</DebugSection>

				<DebugSection title={debug.volume}>
					<DebugRow
						label={debug.volume}
						value={
							data.volumeInfo == null
								? null
								: `${Math.round(data.volumeInfo.volume * 100)}%`
						}
						mono
					/>
					<DebugRow
						label={debug.muted}
						value={
							data.volumeInfo == null
								? null
								: data.volumeInfo.isMuted
									? debug.yes
									: debug.no
						}
					/>
				</DebugSection>
			</div>
		</div>,
		document.body,
	);
}
