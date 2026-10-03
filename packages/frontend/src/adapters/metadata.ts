import type { ArtistInfo } from "@/types/api";
import type { Artist } from "@/types/ncm";

/**
 * 把网易云内部的 ID 转成曲库 ID
 *
 * 本地歌曲之类的条目 ID 可能是空串、0 或非数字，这些都不能用来拼链接
 */
export function parseCatalogId(id: unknown): number | undefined {
	if (typeof id === "number") {
		return Number.isSafeInteger(id) && id > 0 ? id : undefined;
	}
	if (typeof id === "string" && /^\d+$/.test(id)) {
		const parsed = Number.parseInt(id, 10);
		return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined;
	}
	return undefined;
}

function nonEmpty(text: unknown): string | undefined {
	return typeof text === "string" && text.trim() !== "" ? text : undefined;
}

export function toArtistInfos(
	artists: Artist[] | null | undefined,
): ArtistInfo[] | undefined {
	const result: ArtistInfo[] = [];
	for (const artist of artists ?? []) {
		const name = nonEmpty(artist?.name);
		if (!name) continue;
		result.push({
			name,
			id: parseCatalogId(artist.id),
			transName: nonEmpty(artist.transName) ?? nonEmpty(artist.trans),
		});
	}
	return result.length > 0 ? result : undefined;
}

export function firstNonEmpty(
	texts: (string | null | undefined)[] | null | undefined,
): string | undefined {
	if (!Array.isArray(texts)) return undefined;
	for (const text of texts) {
		const value = nonEmpty(text);
		if (value) return value;
	}
	return undefined;
}
