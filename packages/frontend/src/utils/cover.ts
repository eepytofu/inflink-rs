import type { CoverInfo, SongInfo } from "../types/api";
import logger from "./logger";

const COVER_TIMEOUT_MS = 8000;

export class CoverManager {
	private fetchController: AbortController | null = null;

	public async getCover(
		songInfo: SongInfo,
		resolution: string,
	): Promise<{ songInfo: SongInfo; cover: CoverInfo | null }> {
		this.fetchController?.abort();

		const currentCover = songInfo.cover;
		if (!currentCover?.url) {
			return { songInfo, cover: currentCover };
		}

		const thumbnailUrl = this.createImageUrl(currentCover.url, resolution);
		if (!thumbnailUrl) {
			return { songInfo, cover: null };
		}

		const controller = new AbortController();
		this.fetchController = controller;
		const { signal } = controller;

		// 没有超时的话，一次卡住的下载会让 songChange 永远不派发
		let timedOut = false;
		const timeout = window.setTimeout(() => {
			timedOut = true;
			controller.abort();
		}, COVER_TIMEOUT_MS);

		try {
			const response = await fetch(thumbnailUrl, { signal });
			if (!response.ok) {
				throw new Error(`HTTP 错误, 状态码: ${response.status}`);
			}

			const blob = await response.blob();
			return { songInfo, cover: { blob, url: currentCover.url } };
		} catch (e) {
			// 被下一首歌的请求取消时照旧抛出；超时则按下载失败处理，只带地址
			if ((e as Error).name === "AbortError" && !timedOut) {
				throw e;
			}

			logger.warn(
				`获取封面失败: ${timedOut ? "超时" : (e as Error).message}`,
				"CoverManager",
			);
			return { songInfo, cover: currentCover };
		} finally {
			clearTimeout(timeout);
			if (this.fetchController?.signal === signal) {
				this.fetchController = null;
			}
		}
	}

	private createImageUrl(url: string, resolution: string): string {
		if (!url.startsWith("http")) {
			return url;
		}

		const baseUrl = url.split("?")[0];

		const imageParams = new URLSearchParams({
			enlarge: "1",
			type: "jpeg",
			quality: "90",
		});

		if (resolution.toLowerCase() === "max") {
			// 不添加 thumbnail 参数
		} else if (/^\d+$/.test(resolution)) {
			const size = parseInt(resolution, 10);
			imageParams.append("thumbnail", `${size}y${size}`);
		}

		const processedUrl = `${baseUrl}?imageView&${imageParams.toString()}`;

		return `orpheus://cache/?${processedUrl}`;
	}
}
