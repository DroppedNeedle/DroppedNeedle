// The stable style hooks: `data-part` names a person's custom CSS can rely
// on across releases. docs/custom-css.md publishes this list; parts.spec.ts
// holds the docs, and every `data-part` in the source, to it. Each name is
// set in exactly one component, always through `part()`.
export const PARTS = {
	shell: 'The signed-in app: sidebar and page column together.',
	sidebar: 'The navigation rail on the left, from tablet width up.',
	'top-bar': 'The bar along the top with search, status and the theme switch.',
	'bottom-bar': 'The navigation bar along the bottom on a phone.',
	'player-bar': 'The player along the bottom while something plays.',
	'player-bar-now': "The player's now-playing art, title and artist.",
	'player-bar-controls': "The player's playback controls.",
	page: 'The page content area beside the sidebar and under the top bar.',
	'page-header': "A browse page's title block.",
	'page-title': "The title inside a page's header.",
	'section-head': "A shelf's heading row, with its See all link.",
	'section-head-title': "The title inside a shelf's heading row.",
	card: 'A panel that groups content.',
	'media-tile': 'An album, artist or playlist tile in a grid or shelf.',
	'media-tile-art': "A tile's cover or photo.",
	'media-tile-title': "A tile's name under its art.",
	modal: "A dialog's box.",
	button: 'A button, or a link drawn as one.'
} as const;

export type Part = keyof typeof PARTS;

/** The value for a `data-part` attribute. */
export function part(name: Part): Part {
	return name;
}
