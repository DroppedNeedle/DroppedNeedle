// The dots of the page frame (FritFrame): graded halftone dots that grow
// toward an anchor, with low densities dropped out in a 4x4 ordered (Bayer)
// pattern, like printed frit on glass. Content the page shows is kept
// clear: no dot inside a padded box around it, and dots shrink as they near it.

/** One field of dots, densest at (x, y) and gone at its ellipse's edge. */
export interface FritField {
	x: number;
	y: number;
	rx: number;
	ry: number;
	/** Strength at the anchor, 0 to 1. */
	k: number;
}

/** A box the dots keep away from, in the canvas's own pixels. */
export interface FritClear {
	left: number;
	top: number;
	right: number;
	bottom: number;
}

const BAYER = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5].map((v) => (v + 0.5) / 16);

export const PRINT_MS = 1500;
/** No dot this close to content, then dots grow back over the next SOFT. */
export const CLEAR_PAD = 12;
const SOFT = 14;

export function easeOut(t: number): number {
	return 1 - Math.pow(1 - Math.min(Math.max(t, 0), 1), 3);
}

/** 0 inside a box's padding, rising to 1 once SOFT clear of it. */
function clearance(x: number, y: number, boxes: FritClear[]): number {
	let least = 1;
	for (const b of boxes) {
		const dx = Math.max(b.left - x, 0, x - b.right);
		const dy = Math.max(b.top - y, 0, y - b.bottom);
		const room = Math.sqrt(dx * dx + dy * dy) - CLEAR_PAD;
		if (room <= 0) return 0;
		if (room < SOFT) least = Math.min(least, room / SOFT);
	}
	return least;
}

/**
 * Draws the fields into a canvas already scaled to CSS pixels. `progress`
 * runs 0 to 1 over the print-in: the dots nearest an anchor press in first.
 */
export function drawFrit(
	ctx: CanvasRenderingContext2D,
	fields: FritField[],
	step: number,
	progress: number,
	width: number,
	height: number,
	clear: FritClear[] = []
): void {
	const reach = CLEAR_PAD + SOFT;
	ctx.clearRect(0, 0, width, height);
	ctx.beginPath();
	for (const f of fields) {
		if (f.rx <= 0 || f.ry <= 0) continue;
		const x0 = Math.max(0, f.x - f.rx);
		const x1 = Math.min(width, f.x + f.rx);
		const y0 = Math.max(0, f.y - f.ry);
		const y1 = Math.min(height, f.y + f.ry);
		const near = clear.filter(
			(b) =>
				b.right + reach > x0 && b.left - reach < x1 && b.bottom + reach > y0 && b.top - reach < y1
		);
		for (let y = Math.floor(y0 / step) * step + step / 2; y < y1; y += step) {
			const row = near.filter((b) => b.bottom + reach > y && b.top - reach < y);
			for (let x = Math.floor(x0 / step) * step + step / 2; x < x1; x += step) {
				const dx = (x - f.x) / f.rx;
				const dy = (y - f.y) / f.ry;
				const d = Math.sqrt(dx * dx + dy * dy);
				if (d >= 1) continue;
				const reveal = (progress * 1.3 - d) / 0.3;
				if (reveal <= 0) continue;
				const room = row.length > 0 ? clearance(x, y, row) : 1;
				if (room <= 0) continue;
				const t = Math.pow(1 - d, 1.15) * f.k * easeOut(reveal) * room;
				const cell = (Math.round(y / step) & 3) * 4 + (Math.round(x / step) & 3);
				if (t < 0.3 && t < BAYER[cell] * 0.42) continue;
				const r = Math.max(0.7, t * step * 0.56);
				ctx.moveTo(x + r, y);
				ctx.arc(x, y, r, 0, Math.PI * 2);
			}
		}
	}
	ctx.fill();
}
