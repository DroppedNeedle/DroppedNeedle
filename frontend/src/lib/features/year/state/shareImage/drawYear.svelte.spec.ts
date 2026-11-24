import { describe, expect, it } from 'vitest';
import { binaryReply, fakeApi, textReply } from '$lib/test/api';
import type { YearReview } from '../../queries/year.svelte';
import { drawYear, toPng, type ImageShape } from './drawYear';
import { loadImageFromApi } from './loadImage';
import { placeholderColours } from './paint';

const albumArt = 'GET /api/v3/library/albums/{id}/artwork' as const;
const artistArt = 'GET /api/v3/library/artists/{id}/artwork' as const;
const groupArt = 'GET /api/v3/covers/release-group/{release_group_id}' as const;

type Album = YearReview['top_albums'][number];

function album(id: string, name: string, more: Partial<Album> = {}): Album {
	return {
		id,
		name,
		artist_name: 'Portishead',
		plays: 20,
		art_missing: false,
		cover_available: true,
		release_group_mbid: null,
		...more
	};
}

function review(albums: YearReview['top_albums']): YearReview {
	return {
		year: 2026,
		zone: 'Europe/London',
		state: 'in_progress',
		first_day: '2026-01-01',
		last_day: '2026-11-24',
		plays: 400,
		plays_without_length: 0,
		listened_ms: 118 * 3_600_000,
		artists: 40,
		days_played: 120,
		months: [10, 20, 30, 40, 50, 60, 70, 40, 30, 30, 20, 0],
		top_albums: albums,
		top_artists: [
			{ id: 'artist-1', mbid: null, name: 'Portishead', plays: 60, art_missing: false },
			{ id: null, mbid: null, name: 'Massive Attack', plays: 40, art_missing: true }
		],
		top_songs: [],
		new_artists: { count: 19, artists: [], first: null }
	};
}

/** A PNG of one solid colour, as the cover routes send. */
async function solidPng(colour: string): Promise<Blob> {
	const canvas = new OffscreenCanvas(64, 64);
	const ctx = canvas.getContext('2d');
	if (!ctx) throw new Error('no canvas');
	ctx.fillStyle = colour;
	ctx.fillRect(0, 0, 64, 64);
	return canvas.convertToBlob({ type: 'image/png' });
}

function hex(colour: string): number[] {
	return [1, 3, 5].map((i) => parseInt(colour.slice(i, i + 2), 16));
}

function pixel(canvas: HTMLCanvasElement, x: number, y: number): number[] {
	const ctx = canvas.getContext('2d');
	if (!ctx) throw new Error('no canvas');
	return [...ctx.getImageData(x, y, 1, 1).data.slice(0, 3)];
}

function near(a: number[], b: number[], tolerance = 16): boolean {
	return a.every((value, i) => Math.abs(value - b[i]) <= tolerance);
}

/** A placeholder's corners are its gradient's two ends or about its middle. */
function looksLikePlaceholder(canvas: HTMLCanvasElement, seed: string, [x, y, w, h]: number[]) {
	const { from, to } = placeholderColours(seed);
	const ends = [hex(from), hex(to)];
	const middle = ends[0].map((value, i) => (value + ends[1][i]) / 2);
	const corners = [
		[x + 4, y + 4],
		[x + w - 5, y + 4],
		[x + 4, y + h - 5],
		[x + w - 5, y + h - 5]
	].map(([cx, cy]) => pixel(canvas, cx, cy));
	return corners.every((corner) => [...ends, middle].some((colour) => near(corner, colour)));
}

async function pngSize(
	blob: Blob
): Promise<{ signature: number[]; width: number; height: number }> {
	const bytes = new DataView(await blob.arrayBuffer());
	const signature = Array.from({ length: 8 }, (_, i) => bytes.getUint8(i));
	const chunk = String.fromCharCode(...[12, 13, 14, 15].map((i) => bytes.getUint8(i)));
	expect(chunk).toBe('IHDR');
	return { signature, width: bytes.getUint32(16), height: bytes.getUint32(20) };
}

describe('the year share image', () => {
	it.each<[ImageShape, number, number]>([
		['story', 1080, 1920],
		['square', 1080, 1080]
	])('draws the %s at %ix%i as a readable PNG', async (shape, width, height) => {
		const red = await solidPng('#ff0000');
		fakeApi({
			[albumArt]: () => binaryReply(red),
			[artistArt]: () => binaryReply(red)
		});
		const canvas = await drawYear(review([album('album-1', 'Dummy')]), shape, loadImageFromApi);

		expect([canvas.width, canvas.height]).toEqual([width, height]);
		// The covers come from this origin, so the canvas is not tainted.
		expect(() => canvas.getContext('2d')?.getImageData(0, 0, width, height)).not.toThrow();
		const png = await pngSize(await toPng(canvas));
		expect(png.signature).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
		expect([png.width, png.height]).toEqual([width, height]);
	});

	it('draws a cover that is warming, missing or marked missing as its placeholder', async () => {
		const red = await solidPng('#ff0000');
		const api = fakeApi({
			[albumArt]: ({ params }) =>
				params.id === 'warming'
					? textReply(202, '')
					: params.id === 'gone'
						? textReply(404, '')
						: binaryReply(red)
		});
		const albums = [
			album('found', 'Dummy'),
			album('warming', 'Third'),
			album('gone', 'Roseland NYC Live'),
			album('marked', 'Portishead', { art_missing: true })
		];
		const canvas = await drawYear(review(albums), 'square', loadImageFromApi);

		// Four albums print two by two, 540 wide and 486 tall.
		expect(near(pixel(canvas, 270, 100), [255, 0, 0], 2)).toBe(true);
		expect(looksLikePlaceholder(canvas, 'Third', [540, 0, 540, 486])).toBe(true);
		expect(looksLikePlaceholder(canvas, 'Roseland NYC Live', [0, 486, 540, 486])).toBe(true);
		expect(looksLikePlaceholder(canvas, 'Portishead', [540, 486, 540, 486])).toBe(true);
		// The library said it has no art, so it is not asked for.
		expect(api.calls(albumArt).map((call) => call.params.id)).toEqual(['found', 'warming', 'gone']);
	});

	it('reads the release group cover for a matched album with no art of its own', async () => {
		const red = await solidPng('#ff0000');
		const group = '0b6a1c7c-5c8a-3b2e-9a49-1d4c2b5e8f3a';
		const api = fakeApi({ [groupArt]: () => binaryReply(red) });
		const matched = album('matched', 'Dummy', {
			cover_available: false,
			release_group_mbid: group
		});
		const canvas = await drawYear(review([matched]), 'square', loadImageFromApi);

		expect(api.calls(groupArt).map((call) => call.params.release_group_id)).toEqual([group]);
		expect(near(pixel(canvas, 540, 100), [255, 0, 0], 2)).toBe(true);
	});
});
