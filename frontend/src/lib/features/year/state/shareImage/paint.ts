import { coverPlaceholder } from '$lib/ui/coverPlaceholder';
import { COVER, COVER_INK, FAMILY, INK } from './palette';

type Ctx = CanvasRenderingContext2D;

/** A face at a pixel size, as `ctx.font` takes it, with its fallbacks. */
export interface Face {
	family: keyof typeof FAMILY;
	weight: 400 | 600 | 700;
	size: number;
	/** Extra pixels after each character, drawn by hand (no `ctx.letterSpacing`). */
	spacing?: number;
}

export function fontOf(face: Face): string {
	return `${face.weight} ${face.size}px ${FAMILY[face.family]}`;
}

/**
 * Load every face for the strings it draws, so the canvas never paints a
 * fallback while a self-hosted font is still on its way. A face that fails
 * to load leaves its fallback in `ctx.font`.
 */
export async function loadFaces(uses: [Face, string][]): Promise<void> {
	await Promise.all(
		uses.map(([face, text]) => document.fonts.load(fontOf(face), text).catch(() => []))
	);
}

/** The height of a capital in this face: lines are placed by their cap top. */
export function capHeight(ctx: Ctx, face: Face): number {
	ctx.font = fontOf(face);
	return ctx.measureText('H').actualBoundingBoxAscent;
}

function widthOf(ctx: Ctx, text: string, spacing: number): number {
	if (!spacing) return ctx.measureText(text).width;
	let width = 0;
	for (const char of text) width += ctx.measureText(char).width + spacing;
	return width - spacing;
}

/** `text` cut with an ellipsis to fit `maxWidth`, measured in the current font. */
function fit(ctx: Ctx, text: string, spacing: number, maxWidth: number): string {
	if (widthOf(ctx, text, spacing) <= maxWidth) return text;
	const chars = [...text];
	while (chars.length > 0) {
		chars.pop();
		const cut = `${chars.join('').trimEnd()}…`;
		if (widthOf(ctx, cut, spacing) <= maxWidth) return cut;
	}
	return '';
}

export interface TextOptions {
	color: string;
	align?: 'left' | 'center' | 'right';
	maxWidth?: number;
}

/**
 * Draw one line with its cap top at `top`, cut to `maxWidth` when given.
 * Answers the width drawn.
 */
export function drawText(
	ctx: Ctx,
	text: string,
	face: Face,
	x: number,
	top: number,
	{ color, align = 'left', maxWidth }: TextOptions
): number {
	const spacing = face.spacing ?? 0;
	const baseline = top + capHeight(ctx, face);
	const line = maxWidth === undefined ? text : fit(ctx, text, spacing, maxWidth);
	const width = widthOf(ctx, line, spacing);
	let left = align === 'center' ? x - width / 2 : align === 'right' ? x - width : x;
	ctx.fillStyle = color;
	ctx.textAlign = 'left';
	ctx.textBaseline = 'alphabetic';
	if (!spacing) {
		ctx.fillText(line, left, baseline);
		return width;
	}
	for (const char of line) {
		ctx.fillText(char, left, baseline);
		left += ctx.measureText(char).width + spacing;
	}
	return width;
}

export function textWidth(ctx: Ctx, text: string, face: Face): number {
	ctx.font = fontOf(face);
	return widthOf(ctx, text, face.spacing ?? 0);
}

/** Draw `image` over the box, cropped from its centre to the box's shape. */
function drawCropped(
	ctx: Ctx,
	image: CanvasImageSource & { width: number; height: number },
	x: number,
	y: number,
	w: number,
	h: number
) {
	const scale = Math.max(w / image.width, h / image.height);
	const sw = w / scale;
	const sh = h / scale;
	ctx.drawImage(image, (image.width - sw) / 2, (image.height - sh) / 2, sw, sh, x, y, w, h);
}

/**
 * The placeholder pair coverPlaceholder() picks for a seed, as colours. Its
 * answer names theme tokens; the image reads them from its own palette.
 */
export function placeholderColours(seed: string) {
	const look = coverPlaceholder(seed);
	const match = /(\d+)deg, var\(--color-cover-([\w-]+)\), var\(--color-cover-([\w-]+)\)/.exec(
		look.background
	);
	const pale = look.ink.includes('ink-pale');
	return {
		angle: Number(match?.[1] ?? 135),
		from: COVER[match?.[2] ?? 'neutral-from'] ?? COVER['neutral-from'],
		to: COVER[match?.[3] ?? 'neutral-to'] ?? COVER['neutral-to'],
		ink: pale ? COVER_INK.pale : COVER_INK.saturated
	};
}

/** The web UI's placeholder: its gradient, with a record drawn on top. */
function drawPlaceholder(ctx: Ctx, seed: string, x: number, y: number, w: number, h: number) {
	const { angle, from, to, ink } = placeholderColours(seed);
	// A CSS gradient angle: 0deg points up, turning clockwise, and the line
	// is as long as the box's projection on it.
	const rad = (angle * Math.PI) / 180;
	const dx = Math.sin(rad);
	const dy = -Math.cos(rad);
	const half = (Math.abs(w * dx) + Math.abs(h * dy)) / 2;
	const cx = x + w / 2;
	const cy = y + h / 2;
	const gradient = ctx.createLinearGradient(
		cx - dx * half,
		cy - dy * half,
		cx + dx * half,
		cy + dy * half
	);
	gradient.addColorStop(0, from);
	gradient.addColorStop(1, to);
	ctx.fillStyle = gradient;
	ctx.fillRect(x, y, w, h);

	const r = Math.min(w, h) / 4;
	ctx.save();
	ctx.globalAlpha = 0.8;
	ctx.strokeStyle = ink;
	const ring = (radius: number, width: number, alpha = 1) => {
		ctx.globalAlpha = 0.8 * alpha;
		ctx.lineWidth = width;
		ctx.beginPath();
		ctx.arc(cx, cy, radius, 0, Math.PI * 2);
		ctx.stroke();
	};
	ring(r * 0.92, r * 0.1);
	ring(r * 0.72, r * 0.03, 0.6);
	ring(r * 0.58, r * 0.03, 0.6);
	ring(r * 0.1, r * 0.2);
	ctx.restore();
}

/** A cover at full colour, or the item's placeholder when there is none. */
export function drawCover(
	ctx: Ctx,
	image: ImageBitmap | null,
	seed: string,
	x: number,
	y: number,
	w: number,
	h: number
) {
	if (image) drawCropped(ctx, image, x, y, w, h);
	else drawPlaceholder(ctx, seed, x, y, w, h);
}

/**
 * A field of dots in the image's light ink over a circle, heaviest in the
 * middle and fading out at its edge. With a photo, each cell's dot grows
 * with the brightness of the photo there; without one the dots are even.
 */
export function drawHalftone(
	ctx: Ctx,
	photo: ImageBitmap | null,
	box: { x: number; y: number; size: number },
	cell: number
) {
	const cells = Math.ceil(box.size / cell);
	let light: Uint8ClampedArray | null = null;
	if (photo) {
		const sample = document.createElement('canvas');
		sample.width = cells;
		sample.height = cells;
		const sctx = sample.getContext('2d', { willReadFrequently: true });
		if (sctx) {
			sctx.imageSmoothingQuality = 'high';
			drawCropped(sctx, photo, 0, 0, cells, cells);
			light = sctx.getImageData(0, 0, cells, cells).data;
		}
	}
	const centre = box.size / 2;
	const solid = box.size * 0.21;
	const gone = box.size * 0.48;
	ctx.save();
	ctx.fillStyle = INK.text;
	ctx.globalAlpha = 0.85;
	for (let row = 0; row < cells; row++) {
		for (let col = 0; col < cells; col++) {
			const cx = col * cell + cell / 2;
			const cy = row * cell + cell / 2;
			const d = Math.hypot(cx - centre, cy - centre);
			if (d >= gone) continue;
			const fade = d <= solid ? 1 : 1 - (d - solid) / (gone - solid);
			let level = 0.3;
			if (light) {
				const i = (row * cells + col) * 4;
				level = (0.2126 * light[i] + 0.7152 * light[i + 1] + 0.0722 * light[i + 2]) / 255;
			}
			const r = (cell / 2) * Math.sqrt(level) * fade;
			if (r < 0.6) continue;
			ctx.beginPath();
			ctx.arc(box.x + cx, box.y + cy, r, 0, Math.PI * 2);
			ctx.fill();
		}
	}
	ctx.restore();
}
