import { albumArtworkPath, artistArtworkPath, coverPath } from '$lib/api/covers';
import { isValidMbid } from '$lib/utils/formatting';
import type { YearReview } from '../../queries/year.svelte';
import type { LoadImage } from './loadImage';
import { INK } from './palette';
import {
	capHeight,
	drawCover,
	drawHalftone,
	drawText,
	loadFaces,
	textWidth,
	type Face
} from './paint';

export type ImageShape = 'story' | 'square';

/** Each shape's size in pixels, whatever the screen's pixel ratio. */
export const SHAPE_SIZE: Record<ImageShape, { width: number; height: number }> = {
	story: { width: 1080, height: 1920 },
	square: { width: 1080, height: 1080 }
};

type Ctx = CanvasRenderingContext2D;
type Album = YearReview['top_albums'][number];
type Artist = YearReview['top_artists'][number];

const KICKER: Face = { family: 'mono', weight: 400, size: 24, spacing: 6 };
const YEAR: Face = { family: 'display', weight: 400, size: 300, spacing: 4 };
const TAPE: Face = { family: 'mono', weight: 600, size: 22, spacing: 5 };
const HEAD: Face = { family: 'mono', weight: 600, size: 24, spacing: 5 };
const RANK: Face = { family: 'display', weight: 400, size: 36 };
const NAME: Face = { family: 'sans', weight: 700, size: 36 };
const STAT: Face = { family: 'display', weight: 400, size: 104 };
const STAT_LABEL: Face = { family: 'mono', weight: 400, size: 22, spacing: 4 };
const MARK: Face = { family: 'display', weight: 400, size: 44 };
const BRAND: Face = { family: 'mono', weight: 600, size: 24, spacing: 6 };
const BAND_YEAR: Face = { family: 'display', weight: 400, size: 52, spacing: 2 };
const BAND: Face = { family: 'mono', weight: 600, size: 24, spacing: 5 };

const MARGIN = 72;
// The bars of the barcode at the story's foot, in pixels.
const BARS = [4, 8, 4, 10, 4, 6, 4, 8, 4, 4, 10, 4, 6, 4, 8, 4];

const numbers = new Intl.NumberFormat('en-US');

function hours(review: YearReview): string {
	return numbers.format(Math.round(review.listened_ms / 3_600_000));
}

// Each image's route is picked as AlbumImage and ArtistImage pick it: the
// library's own art first, else the MusicBrainz cover. Art the library says
// is missing is never asked for.
function albumArt(album: Album | undefined, drawn: number): string | null {
	if (!album || album.art_missing) return null;
	const size = drawn > 500 ? 1200 : 500;
	if (album.cover_available) return albumArtworkPath(album.id, undefined, size);
	const group = album.release_group_mbid;
	return isValidMbid(group) && group ? coverPath('release-group', group, size) : null;
}

/** The colours of an album's placeholder come from its release group, else its name. */
function albumSeed(album: Album): string {
	const group = album.release_group_mbid;
	return !album.cover_available && isValidMbid(group) && group ? group : album.name;
}

function artistPhoto(artist: Artist | undefined): string | null {
	if (!artist || artist.art_missing) return null;
	if (isValidMbid(artist.mbid) && artist.mbid) return coverPath('artist', artist.mbid, 500);
	return artist.id ? artistArtworkPath(artist.id, 500) : null;
}

async function loadAll(load: LoadImage, paths: (string | null)[]): Promise<(ImageBitmap | null)[]> {
	return Promise.all(paths.map((path) => (path ? load(path) : Promise.resolve(null))));
}

function newCanvas(shape: ImageShape): { canvas: HTMLCanvasElement; ctx: Ctx } {
	const canvas = document.createElement('canvas');
	canvas.width = SHAPE_SIZE[shape].width;
	canvas.height = SHAPE_SIZE[shape].height;
	const ctx = canvas.getContext('2d');
	if (!ctx) throw new Error('This browser cannot draw on a canvas.');
	ctx.fillStyle = INK.page;
	ctx.fillRect(0, 0, canvas.width, canvas.height);
	return { canvas, ctx };
}

function rule(ctx: Ctx, x: number, y: number, w: number, h: number) {
	ctx.fillStyle = INK.rule;
	ctx.fillRect(x, y, w, h);
}

/** The No. 1 album: its record behind, the cover, and the tape across its corner. */
function drawNumberOne(ctx: Ctx, album: Album | undefined, cover: ImageBitmap | null, top: number) {
	const size = 540;
	const cx = MARGIN + 494;
	const cy = top + size / 2;
	const r = 254;
	ctx.fillStyle = INK.vinyl;
	ctx.beginPath();
	ctx.arc(cx, cy, r, 0, Math.PI * 2);
	ctx.fill();
	ctx.strokeStyle = INK.groove;
	ctx.lineWidth = 2;
	for (let groove = r - 10; groove > 70; groove -= 8) {
		ctx.beginPath();
		ctx.arc(cx, cy, groove, 0, Math.PI * 2);
		ctx.stroke();
	}
	ctx.strokeStyle = INK.rule;
	ctx.beginPath();
	ctx.arc(cx, cy, r, 0, Math.PI * 2);
	ctx.stroke();
	ctx.fillStyle = INK.acid;
	ctx.beginPath();
	ctx.arc(cx, cy, 60, 0, Math.PI * 2);
	ctx.fill();
	ctx.fillStyle = INK.page;
	ctx.beginPath();
	ctx.arc(cx, cy, 7, 0, Math.PI * 2);
	ctx.fill();

	if (!album) return;
	drawCover(ctx, cover, albumSeed(album), MARGIN, top, size, size);

	const label = 'NO. 1 ALBUM';
	const padX = 24;
	const tapeH = 22 + capHeight(ctx, TAPE);
	ctx.save();
	ctx.translate(MARGIN + 44, top - 20);
	ctx.rotate((-3 * Math.PI) / 180);
	ctx.fillStyle = INK.acid;
	ctx.fillRect(0, 0, textWidth(ctx, label, TAPE) + padX * 2, tapeH);
	drawText(ctx, label, TAPE, padX, 11, { color: INK.acidInk });
	ctx.restore();
}

/** A ranked list of up to five names under its heading; answers its bottom. */
function drawRanked(ctx: Ctx, heading: string, names: string[], x: number, top: number): number {
	const width = 444;
	drawText(ctx, heading, HEAD, x, top, { color: INK.acid, maxWidth: width });
	let row = top + capHeight(ctx, HEAD) + 34;
	const pitch = 60;
	names.slice(0, 5).forEach((name, i) => {
		drawText(ctx, String(i + 1), RANK, x, row, { color: INK.rank });
		drawText(ctx, name, NAME, x + 52, row, { color: INK.text, maxWidth: width - 52 });
		row += pitch;
	});
	return row - pitch + capHeight(ctx, NAME);
}

function drawStats(ctx: Ctx, stats: [string, string][], top: number): number {
	const width = 1080 - MARGIN * 2;
	const numberTop = top + 30;
	const labelTop = numberTop + capHeight(ctx, STAT) + 22;
	const bottom = labelTop + capHeight(ctx, STAT_LABEL) + 30;
	rule(ctx, MARGIN, top, width, 2);
	rule(ctx, MARGIN, bottom, width, 2);
	const column = width / stats.length;
	stats.forEach(([value, label], i) => {
		const x = MARGIN + column * i + (i > 0 ? 36 : 0);
		if (i > 0) rule(ctx, MARGIN + column * i, top, 2, bottom - top);
		drawText(ctx, value, STAT, x, numberTop, { color: INK.text, maxWidth: column - 36 });
		drawText(ctx, label, STAT_LABEL, x, labelTop, { color: INK.muted });
	});
	return bottom;
}

function drawFoot(ctx: Ctx, height: number) {
	const boxH = 64;
	const top = height - 64 - boxH;
	const markW = textWidth(ctx, 'DN', MARK) + 32;
	ctx.fillStyle = INK.acid;
	ctx.beginPath();
	ctx.roundRect(MARGIN, top, markW, boxH, 8);
	ctx.fill();
	const markCap = capHeight(ctx, MARK);
	drawText(ctx, 'DN', MARK, MARGIN + 16, top + (boxH - markCap) / 2, { color: INK.acidInk });
	const brandCap = capHeight(ctx, BRAND);
	drawText(ctx, 'DROPPEDNEEDLE', BRAND, MARGIN + markW + 20, top + (boxH - brandCap) / 2, {
		color: INK.text
	});

	const barsW = BARS.reduce((sum, w) => sum + w, 0) + (BARS.length - 1) * 4;
	let x = 1080 - MARGIN - barsW;
	ctx.fillStyle = INK.text;
	for (const w of BARS) {
		ctx.fillRect(x, top + boxH - 60, w, 60);
		x += w + 4;
	}
}

async function drawStory(review: YearReview, load: LoadImage): Promise<HTMLCanvasElement> {
	const albums = review.top_albums;
	const artists = review.top_artists;
	const [cover, photo] = await loadAll(load, [albumArt(albums[0], 540), artistPhoto(artists[0])]);

	const stats: [string, string][] = [[hours(review), 'HOURS']];
	if (review.new_artists) stats.push([numbers.format(review.new_artists.count), 'NEW ARTISTS']);
	const albumNames = albums.slice(0, 5).map((album) => album.name);
	const artistNames = artists.slice(0, 5).map((artist) => artist.name);
	await loadFaces([
		[KICKER, 'MY YEAR ON THE NEEDLE'],
		[YEAR, String(review.year)],
		[TAPE, 'NO. 1 ALBUM'],
		[HEAD, 'TOP ARTISTS TOP ALBUMS'],
		[RANK, '12345'],
		[NAME, [...artistNames, ...albumNames].join(' ') || 'H'],
		[STAT, stats.map(([value]) => value).join(' ')],
		[STAT_LABEL, 'HOURS NEW ARTISTS'],
		[MARK, 'DN'],
		[BRAND, 'DROPPEDNEEDLE']
	]);

	const { canvas, ctx } = newCanvas('story');
	drawHalftone(ctx, photo, { x: 440, y: -40, size: 720 }, 14);
	drawText(ctx, 'MY YEAR ON THE NEEDLE', KICKER, MARGIN, 80, { color: INK.muted });
	const yearTop = 80 + capHeight(ctx, KICKER) + 48;
	drawText(ctx, String(review.year), YEAR, MARGIN - 6, yearTop, { color: INK.acid });
	const coverTop = yearTop + capHeight(ctx, YEAR) + 72;
	drawNumberOne(ctx, albums[0], cover, coverTop);

	const listsTop = coverTop + 540 + 64;
	let listsBottom = listsTop;
	if (artistNames.length > 0) {
		listsBottom = drawRanked(ctx, 'TOP ARTISTS', artistNames, MARGIN, listsTop);
	}
	if (albumNames.length > 0) {
		const column = artistNames.length > 0 ? MARGIN + 444 + 48 : MARGIN;
		listsBottom = Math.max(
			listsBottom,
			drawRanked(ctx, 'TOP ALBUMS', albumNames, column, listsTop)
		);
	}
	drawStats(ctx, stats, listsBottom + 64);
	drawFoot(ctx, canvas.height);
	return canvas;
}

/** The grid's columns for a number of albums: never a hole, never a stand-in. */
function gridColumns(albums: number): number {
	if (albums >= 9) return 3;
	if (albums >= 4) return 2;
	return 1;
}

async function drawSquare(review: YearReview, load: LoadImage): Promise<HTMLCanvasElement> {
	const columns = gridColumns(review.top_albums.length);
	const shown = review.top_albums.slice(0, columns * columns);
	const cell = 1080 / columns;
	const gridH = 972;
	const cellH = gridH / columns;
	const covers = await loadAll(
		load,
		shown.map((album) => albumArt(album, cell))
	);
	const year = String(review.year);
	const time = `${hours(review)} HOURS`;
	await loadFaces([
		[BAND_YEAR, year],
		[BAND, `${time} DROPPEDNEEDLE`]
	]);

	const { canvas, ctx } = newCanvas('square');
	if (shown.length === 0) drawCover(ctx, null, '', 0, 0, 1080, gridH);
	shown.forEach((album, i) => {
		const x = (i % columns) * cell;
		const y = Math.floor(i / columns) * cellH;
		drawCover(ctx, covers[i], albumSeed(album), x, y, cell, cellH);
	});
	const bandMiddle = gridH + (1080 - gridH) / 2;
	drawText(ctx, year, BAND_YEAR, 32, bandMiddle - capHeight(ctx, BAND_YEAR) / 2, {
		color: INK.acid
	});
	const bandTop = bandMiddle - capHeight(ctx, BAND) / 2;
	drawText(ctx, time, BAND, 540, bandTop, { color: INK.text, align: 'center' });
	drawText(ctx, 'DROPPEDNEEDLE', BAND, 1080 - 32, bandTop, { color: INK.text, align: 'right' });
	return canvas;
}

/**
 * Draw a person's year as a share image in one fixed dark look. It shows
 * the year, their top artists and albums and their hours, and never their
 * name or the server's address. Covers come through `load`; one that
 * cannot be had is drawn as its placeholder.
 */
export function drawYear(
	review: YearReview,
	shape: ImageShape,
	load: LoadImage
): Promise<HTMLCanvasElement> {
	return shape === 'story' ? drawStory(review, load) : drawSquare(review, load);
}

/** The canvas as a PNG. */
export function toPng(canvas: HTMLCanvasElement): Promise<Blob> {
	return new Promise((resolve, reject) => {
		canvas.toBlob(
			(blob) => (blob ? resolve(blob) : reject(new Error('The image could not be made.'))),
			'image/png'
		);
	});
}
