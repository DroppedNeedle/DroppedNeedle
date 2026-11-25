import { untrack } from 'svelte';

/**
 * Fills a settings form's empty fields with what the setup guide found, once
 * the section has loaded and once per suggestion. Nothing is saved: the form's
 * own Save does that, and its Discard puts the saved values back.
 * Call while a component initialises: it registers an effect.
 */
export function useSuggestion<T extends object>(
	section: { readonly draft: T | undefined },
	suggested: () => string | undefined,
	fill: (draft: T, value: string) => void
): void {
	let filled: string | undefined;
	$effect(() => {
		const value = suggested();
		const draft = section.draft;
		if (!value || draft === undefined || value === filled) return;
		filled = value;
		untrack(() => fill(draft, value));
	});
}

/** A connection form: the address when none is set, switched on with it. */
export function fillConnection(
	draft: { url: string; enabled?: boolean },
	url: string,
	switchOn: boolean
): void {
	if (draft.url.trim()) return;
	draft.url = url;
	if (switchOn) draft.enabled = true;
}
