import { api } from '$lib/api/client';

/**
 * Reads one of the app's own image routes and decodes it, or answers null
 * for the item's placeholder. Only paths from `albumArtworkPath`,
 * `artistArtworkPath` or `coverPath` are passed in: those routes answer
 * bytes from this origin and never redirect, so the canvas they are drawn
 * on stays readable.
 */
export type LoadImage = (path: string) => Promise<ImageBitmap | null>;

const COVER_TIMEOUT_MS = 15_000;

/**
 * The api client's loader. Only a 200 is drawn: a 202 (art still warming),
 * a 404 (no art), a read that fails or takes too long, or a decode that
 * fails gives the placeholder.
 */
export const loadImageFromApi: LoadImage = async (path) => {
	try {
		const response = await api.raw(path, { timeoutMs: COVER_TIMEOUT_MS });
		if (response.status !== 200) return null;
		return await createImageBitmap(await response.blob());
	} catch {
		return null;
	}
};

/**
 * A loader that reads each path once, for a sheet that draws both shapes
 * from the same covers. `close` frees the decoded images.
 */
export function onceEach(load: LoadImage): { load: LoadImage; close: () => void } {
	const seen = new Map<string, Promise<ImageBitmap | null>>();
	return {
		load: (path) => {
			let image = seen.get(path);
			if (!image) {
				image = load(path);
				seen.set(path, image);
			}
			return image;
		},
		close: () => {
			for (const image of seen.values()) void image.then((bitmap) => bitmap?.close());
			seen.clear();
		}
	};
}
