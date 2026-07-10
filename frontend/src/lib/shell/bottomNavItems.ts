/*
 * bottomNavItems — the destinations a user can pin to the mobile bottom bar's
 * four configurable slots (the fifth slot, Menu, is fixed). Consumed by
 * AppBottomNav (rendering) and SettingsAppearance (the picker).
 */
import type { Icon as IconType } from 'lucide-svelte';
import {
	House,
	Compass,
	Search,
	Library,
	RadioTower,
	History,
	ListMusic,
	Heart,
	Download,
	Activity,
	Inbox
} from 'lucide-svelte';

export interface BottomNavItem {
	id: string;
	label: string;
	icon: typeof IconType;
	/** navigation target; omitted for action items (search opens the palette) */
	href?: string;
	/** path prefix used for the active state ('/' matches exactly) */
	match?: string;
	action?: 'search';
}

export const BOTTOM_NAV_ITEMS: BottomNavItem[] = [
	{ id: 'home', label: 'Explore', icon: House, href: '/', match: '/' },
	{ id: 'discovery', label: 'Discovery', icon: Compass, href: '/discover', match: '/discover' },
	{ id: 'search', label: 'Search', icon: Search, action: 'search', match: '/search' },
	{ id: 'collection', label: 'Collection', icon: Library, href: '/library', match: '/library' },
	{ id: 'stations', label: 'Stations', icon: RadioTower, href: '/stations', match: '/stations' },
	{
		id: 'recent',
		label: 'Recent',
		icon: History,
		href: '/library/tracks?sort=recent',
		match: '/library/tracks'
	},
	{ id: 'playlists', label: 'Playlists', icon: ListMusic, href: '/playlists', match: '/playlists' },
	{ id: 'following', label: 'Following', icon: Heart, href: '/following', match: '/following' },
	{ id: 'downloads', label: 'Downloads', icon: Download, href: '/downloads', match: '/downloads' },
	{ id: 'activity', label: 'Activity', icon: Activity, href: '/activity', match: '/activity' },
	{ id: 'requests', label: 'Requests', icon: Inbox, href: '/requests', match: '/requests' }
];

export function bottomNavItem(id: string): BottomNavItem {
	return BOTTOM_NAV_ITEMS.find((i) => i.id === id) ?? BOTTOM_NAV_ITEMS[0];
}
