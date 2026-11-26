import { ChartPie, Clock, Disc3, History, MessageSquare, Sparkles } from 'lucide-svelte';
import type { KitIcon } from '$lib/ui/Button.svelte';

export type DiscoverPageId = 'week' | 'weeks' | 'later' | 'new-releases' | 'taste' | 'told';

/** One of Discover's pages, a tab in the strip every one of them shows. */
export interface DiscoverPage {
	id: DiscoverPageId;
	label: string;
	/** The page's path under the base path. */
	path: string;
	icon: KitIcon;
}

/** Discover's pages, in the strip's order. */
export const DISCOVER_PAGES: readonly DiscoverPage[] = [
	{ id: 'week', label: 'This week', path: '/discover', icon: Disc3 },
	{ id: 'weeks', label: 'Past weeks', path: '/discover/weeks', icon: History },
	{ id: 'later', label: 'Later', path: '/discover/later', icon: Clock },
	{
		id: 'new-releases',
		label: 'New from your artists',
		path: '/discover/new-releases',
		icon: Sparkles
	},
	{ id: 'taste', label: 'Your taste', path: '/discover/taste', icon: ChartPie },
	{ id: 'told', label: 'What you told Discover', path: '/discover/told', icon: MessageSquare }
];
