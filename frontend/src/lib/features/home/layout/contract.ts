// Home grid contract. Measured on the live 1920 home: cards are 176px with a
// 16px gutter (192 pitch), a shelf block is 383px, a genre block 182px, the
// entry cards 266px. UNIT is half the pitch so a shelf row lands on 4 rows.

/** A square cell. Columns are floor(width / UNIT), so a cell is never narrower than this. */
export const UNIT = 96;

/** Column count a saved layout is expressed in; the grid rescales it to the window. */
export const REFERENCE_COLUMNS = 18;

export type WidgetKind =
	'entry_cards' | 'prompts' | 'now_playing' | 'shelf' | 'weekly' | 'discover_teaser' | 'genre_grid';

/** How a widget fills the space it is given. */
export type GrowRule =
	/** more width means more items per row; more height means rows of cards */
	| 'wrap'
	/** the widget stretches its contents to the box */
	| 'scale'
	/** the box is the content; extra space is padding */
	| 'fixed';

export interface WidgetSize {
	minW: number;
	minH: number;
	maxW?: number;
	maxH?: number;
}

export interface WidgetContract {
	grow: GrowRule;
	size: WidgetSize;
	/** Rows the widget takes in the default layout. */
	defaultH: number;
}

export const WIDGET_CONTRACT: Record<WidgetKind, WidgetContract> = {
	entry_cards: { grow: 'fixed', size: { minW: 8, minH: 3, maxH: 3 }, defaultH: 3 },
	prompts: { grow: 'fixed', size: { minW: 6, minH: 2, maxH: 6 }, defaultH: 2 },
	now_playing: { grow: 'scale', size: { minW: 6, minH: 2, maxH: 4 }, defaultH: 2 },
	shelf: { grow: 'wrap', size: { minW: 4, minH: 3, maxH: 12 }, defaultH: 4 },
	weekly: { grow: 'wrap', size: { minW: 6, minH: 3, maxH: 8 }, defaultH: 4 },
	discover_teaser: { grow: 'fixed', size: { minW: 6, minH: 2, maxH: 3 }, defaultH: 2 },
	genre_grid: { grow: 'wrap', size: { minW: 6, minH: 2, maxH: 4 }, defaultH: 2 }
};

export interface WidgetPlacement {
	/** Stable across loads: the section key, or the fixed kind for singletons. */
	id: string;
	kind: WidgetKind;
	x: number;
	y: number;
	w: number;
	h: number;
	minW: number;
	minH: number;
	maxW?: number;
	maxH?: number;
}

export interface WidgetSpec {
	id: string;
	kind: WidgetKind;
	/** Overrides the kind's default rows when the content is taller or shorter. */
	height?: number;
	/** Narrows the kind's bounds for this widget. */
	minH?: number;
}
