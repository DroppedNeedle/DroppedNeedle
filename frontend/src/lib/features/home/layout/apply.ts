import {
	REFERENCE_COLUMNS,
	WIDGET_CONTRACT,
	type WidgetPlacement,
	type WidgetSpec
} from './contract';

/** The layout shape this client writes. */
export const LAYOUT_VERSION = 1;

interface SavedWidget {
	id: string;
	x: number;
	y: number;
	w: number;
	h: number;
}

/**
 * A saved layout on today's widgets: cells are clamped to the widget's
 * contract, ids the page no longer offers are dropped, and, unless the layout
 * is the admin's forced one, a widget the save never knew about joins at the
 * bottom.
 */
export function applySavedLayout(
	saved: SavedWidget[],
	specs: WidgetSpec[],
	appendMissing = true
): WidgetPlacement[] {
	const byId = new Map(specs.map((spec) => [spec.id, spec]));
	const placed = new Set<string>();
	const out: WidgetPlacement[] = [];
	for (const widget of saved) {
		const spec = byId.get(widget.id);
		if (!spec) continue;
		placed.add(spec.id);
		const { size } = WIDGET_CONTRACT[spec.kind];
		const minH = spec.minH ?? size.minH;
		const w = Math.min(Math.max(widget.w, size.minW), size.maxW ?? REFERENCE_COLUMNS);
		const h = Math.min(Math.max(widget.h, minH), size.maxH ?? 48);
		out.push({
			id: spec.id,
			kind: spec.kind,
			x: Math.min(Math.max(widget.x, 0), REFERENCE_COLUMNS - w),
			y: Math.max(widget.y, 0),
			w,
			h,
			minW: size.minW,
			minH,
			maxW: size.maxW,
			maxH: size.maxH
		});
	}
	if (!appendMissing) return out;
	const added = specs.filter((spec) => !placed.has(spec.id));
	if (added.length === 0) return out;
	const bottom = out.reduce((low, place) => Math.max(low, place.y + place.h), 0);
	let y = bottom;
	for (const spec of added) {
		const { size } = WIDGET_CONTRACT[spec.kind];
		const h = spec.height ?? WIDGET_CONTRACT[spec.kind].defaultH;
		out.push({
			id: spec.id,
			kind: spec.kind,
			x: 0,
			y,
			w: REFERENCE_COLUMNS,
			h,
			minW: size.minW,
			minH: spec.minH ?? size.minH,
			maxW: size.maxW,
			maxH: size.maxH
		});
		y += h;
	}
	return out;
}
