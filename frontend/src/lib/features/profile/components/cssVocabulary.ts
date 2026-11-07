// What the custom CSS editor offers: the theme's `--dn-*` variables, read
// from the theme file itself, and the stable style hooks from the kit's list.
import type { CodeWord } from '$lib/ui/codeEditor';
import { PARTS } from '$lib/ui/parts';
import themeCss from '../../../../theme.css?raw';
import snippets from './cssSnippets.json';

/** Every `--dn-*` name the theme sets, in the order the file first sets it. */
export const THEME_VARIABLES: readonly string[] = [
	...new Set([...themeCss.matchAll(/(--dn-[a-z0-9-]+)\s*:/g)].map((hit) => hit[1] as string))
];

export interface CssSnippet {
	id: string;
	label: string;
	css: string;
}

export const CSS_SNIPPETS: readonly CssSnippet[] = snippets.snippets;

/** What an empty editor shows. */
export const CSS_PLACEHOLDER: string = snippets.placeholder;

/** The selector for one style hook. */
export function partSelector(name: string): string {
	return snippets.part_selector.replace('{name}', name);
}

export const CSS_WORDS: readonly CodeWord[] = [
	...THEME_VARIABLES.map((name) => ({ label: name, detail: 'theme', type: 'variable' as const })),
	...Object.entries(PARTS).map(([name, what]) => ({
		label: partSelector(name),
		detail: what,
		type: 'keyword' as const
	}))
];
