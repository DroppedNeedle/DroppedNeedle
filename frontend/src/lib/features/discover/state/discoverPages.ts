import { AudioLines, Clock, History } from 'lucide-svelte';
import type { KitIcon } from '$lib/ui/Button.svelte';

/** One of Discover's own pages, linked from its header and its phone rows. */
export interface DiscoverPage {
	id: 'later' | 'weeks' | 'taste';
	label: string;
	/** The page's path under the base path. */
	path: string;
	icon: KitIcon;
}

/** Discover's pages, in the order they are linked. */
export const DISCOVER_PAGES: readonly DiscoverPage[] = [
	{ id: 'later', label: 'Later', path: '/discover/later', icon: Clock },
	{ id: 'weeks', label: 'Past weeks', path: '/discover/weeks', icon: History },
	{ id: 'taste', label: 'Your taste', path: '/discover/taste', icon: AudioLines }
];
