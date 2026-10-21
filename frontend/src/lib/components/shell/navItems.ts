// Every navigation destination, once. The sidebar, the phone's bottom bar and
// its More sheet all render from this list, so a destination is added, moved
// or hidden here and nowhere else.
import type { KitIcon } from '$lib/ui/Button.svelte';
import {
	Cog,
	Compass,
	Download,
	Heart,
	House,
	Inbox,
	LibraryBig,
	ListChecks,
	ListMusic,
	Menu,
	Search,
	Settings,
	ShieldCheck,
	Trash2
} from 'lucide-svelte';
import { withBasePath } from '$lib/utils/basePath';

/** Who sees a destination: everyone, only when a download client is set up, or admins. */
export type NavAudience = 'everyone' | 'download-client' | 'admin';

/** A live marker on a destination; each nav draws it in its own style. */
export type NavBadge = 'library-sync' | 'downloads' | 'following' | 'approvals' | 'update';

/** The sidebar's sections, top to bottom. `footer` is the icon row at the bottom. */
export type SidebarGroup = 'search' | 'browse' | 'requests' | 'admin' | 'footer';

/** Whether the current path (without the base path) highlights a destination. */
export type NavMatch = (path: string) => boolean;

export interface NavItem {
	id: string;
	label: string;
	icon: KitIcon;
	/** A small second icon in the icon's corner, in the library-management colour. */
	iconMark?: KitIcon;
	/** App path without the base path. Absent for Search, which opens the search dialog. */
	href?: string;
	/** Where it points instead while an update is waiting. */
	updateHref?: string;
	audience: NavAudience;
	badge?: NavBadge;
	/** In the sidebar; with no `active` it never highlights there. */
	sidebar?: { group: SidebarGroup; active?: NavMatch };
	/** In the phone's bottom bar, in list order. */
	bottom?: { active: NavMatch };
	/** In the phone's More sheet; `badge: false` leaves the badge off there. */
	more?: { active: NavMatch; badge?: false };
}

export interface NavViewer {
	isAdmin: boolean;
	downloadClient: boolean;
}

export const exactly =
	(target: string): NavMatch =>
	(path) =>
		path === target;

export const within =
	(prefix: string): NavMatch =>
	(path) =>
		path === prefix || path.startsWith(`${prefix}/`);

// Library Management, the Review Queue and the Recycle Bin live under /library but are their
// own destinations.
const inLibrary: NavMatch = (path) =>
	within('/library')(path) &&
	!within('/library/management')(path) &&
	!within('/library/review')(path) &&
	!within('/library/recycle-bin')(path);

export const navItems: NavItem[] = [
	{
		id: 'home',
		label: 'Home',
		icon: House,
		href: '/',
		audience: 'everyone',
		sidebar: { group: 'browse' },
		bottom: { active: exactly('/') }
	},
	{
		id: 'discover',
		label: 'Discover',
		icon: Compass,
		href: '/discover',
		audience: 'everyone',
		sidebar: { group: 'browse' },
		bottom: { active: within('/discover') }
	},
	{
		id: 'search',
		label: 'Search',
		icon: Search,
		audience: 'everyone',
		sidebar: { group: 'search' },
		bottom: { active: within('/search') }
	},
	{
		id: 'library',
		label: 'Library',
		icon: Menu,
		href: '/library',
		audience: 'everyone',
		badge: 'library-sync',
		sidebar: { group: 'browse', active: inLibrary },
		bottom: { active: inLibrary }
	},
	{
		id: 'downloads',
		label: 'Downloads',
		icon: Download,
		href: '/downloads',
		audience: 'everyone',
		badge: 'downloads',
		sidebar: { group: 'browse' },
		more: { active: within('/downloads'), badge: false }
	},
	{
		id: 'following',
		label: 'Following',
		icon: Heart,
		href: '/following',
		audience: 'everyone',
		badge: 'following',
		sidebar: { group: 'browse', active: within('/following') },
		more: { active: within('/following'), badge: false }
	},
	{
		id: 'playlists',
		label: 'Playlists',
		icon: ListMusic,
		href: '/playlists',
		audience: 'download-client',
		sidebar: { group: 'browse', active: within('/playlists') },
		more: { active: within('/playlists') }
	},
	{
		id: 'requests',
		label: 'Requests',
		icon: Inbox,
		href: '/requests',
		audience: 'everyone',
		sidebar: { group: 'requests' },
		more: { active: within('/requests') }
	},
	{
		id: 'library-management',
		label: 'Library Management',
		icon: LibraryBig,
		iconMark: Cog,
		href: '/library/management',
		audience: 'admin',
		sidebar: { group: 'admin', active: within('/library/management') },
		more: { active: within('/library/management') }
	},
	{
		id: 'approvals',
		label: 'Approvals',
		icon: ShieldCheck,
		href: '/requests?tab=approvals',
		audience: 'admin',
		badge: 'approvals',
		sidebar: { group: 'admin' },
		more: { active: within('/requests') }
	},
	{
		id: 'review',
		label: 'Review Queue',
		icon: ListChecks,
		href: '/library/review',
		audience: 'admin',
		sidebar: { group: 'admin', active: within('/library/review') },
		more: { active: within('/library/review') }
	},
	{
		id: 'recycle-bin',
		label: 'Recycle Bin',
		icon: Trash2,
		href: '/library/recycle-bin',
		audience: 'admin',
		sidebar: { group: 'admin', active: within('/library/recycle-bin') },
		more: { active: within('/library/recycle-bin') }
	},
	{
		id: 'settings',
		label: 'Settings',
		icon: Settings,
		href: '/settings',
		updateHref: '/settings?tab=about',
		audience: 'admin',
		badge: 'update',
		sidebar: { group: 'footer' },
		bottom: { active: within('/settings') }
	}
];

export function canSee(item: NavItem, viewer: NavViewer): boolean {
	if (item.audience === 'admin') return viewer.isAdmin;
	if (item.audience === 'download-client') return viewer.downloadClient;
	return true;
}

export function navHref(item: NavItem, updateAvailable: boolean): string {
	const path = updateAvailable && item.updateHref ? item.updateHref : (item.href ?? '/');
	return withBasePath(path);
}

/** The More tab highlights on any path the sheet holds, whoever is viewing. */
export function isMoreActive(path: string): boolean {
	return navItems.some((item) => item.more?.active(path));
}
