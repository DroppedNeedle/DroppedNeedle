import { PROVIDER_NAMES } from '$lib/utils/serviceError';
import type { Problem, ProblemSeverity } from '../queries/problems.svelte';

/** What one row says and where it is fixed. */
export interface ProblemView {
	sentence: string;
	/** In-app path of the page that fixes it, before the base path. */
	fixHref: string | null;
}

const CLIENTS: Record<string, string> = {
	slskd: 'slskd',
	sabnzbd: 'SABnzbd',
	nzbget: 'NZBGet',
	deemix: 'Deemix'
};

/** The settings tab each stored section is saved on. */
const SECTION_TABS: Record<string, string> = {
	download_client: 'download-client',
	download_clients: 'download-client',
	deemix_connection: 'download-client',
	usenet_backend: 'download-client',
	download_policy: 'download-client',
	source_priority: 'download-client',
	indexers: 'indexers',
	prowlarr_connection: 'indexers',
	lidarr_import_connection: 'lidarr-import',
	typed_library: 'library',
	import_folder: 'library',
	library_scan_schedule: 'library',
	filesystem_watcher: 'library',
	wanted_watcher: 'library',
	edition_preferences: 'editions',
	jellyfin_connection: 'jellyfin',
	navidrome_connection: 'navidrome',
	plex_connection: 'plex',
	youtube_connection: 'youtube',
	lastfm_settings: 'lastfm',
	listenbrainz_settings: 'listenbrainz',
	spotify_settings: 'spotify',
	events_settings: 'events',
	get_it: 'get-it',
	musicbrainz_settings: 'musicbrainz',
	security_settings: 'security',
	public_address: 'security',
	oidc_connection: 'security',
	connect_apps: 'connect-apps',
	app_connect: 'connect-apps',
	backups: 'backups',
	wrapped_settings: 'wrapped',
	appearance: 'appearance'
};

/** Short names for the background loops a person may see here. */
const LOOPS: Record<string, string> = {
	'database-backup': 'Nightly backup',
	'disk-watch': 'Free space watch',
	'acquire-import-folder': 'Import folder',
	'acquire-probe-refresh': 'Download client check'
};

const ROLES: Record<string, string> = {
	data: 'The data disk',
	config: 'The config disk',
	temp: 'The temp disk',
	imports: 'The imports disk',
	library: 'A library disk',
	recycle_bin: 'The recycle bin disk',
	downloads: 'A downloads disk',
	import_folder: 'The import folder disk'
};

const settingsTab = (tab: string) => `/settings?tab=${tab}`;

function client(subject: string | null | undefined): string {
	return (subject && CLIENTS[subject]) ?? subject ?? 'A download client';
}

/** A loop's name as a person reads it. */
export function loopLabel(name: string): string {
	return LOOPS[name] ?? name;
}

/** The sentence and the fix page for one problem. */
export function describeProblem(problem: Problem): ProblemView {
	const subject = problem.subject ?? null;
	switch (problem.kind) {
		case 'settings_unreadable':
			return {
				sentence: `Saved settings don't read (${subject ?? 'a section'}). Save them again.`,
				fixHref: settingsTab((subject && SECTION_TABS[subject]) ?? 'advanced')
			};
		case 'loop_failing':
			return {
				sentence: `${loopLabel(subject ?? 'A background job')} keeps failing.`,
				fixHref: settingsTab('diagnostics')
			};
		case 'download_client_down':
			return {
				sentence: `${client(subject)} isn't answering.`,
				fixHref: settingsTab('download-client')
			};
		case 'download_folder_unusable':
			return {
				sentence: `${client(subject)}'s downloads folder can't be used.`,
				fixHref: settingsTab('download-client')
			};
		case 'recycle_bin_unavailable':
			return {
				sentence: "The recycle bin can't be used. Removals wait.",
				fixHref: '/library/management'
			};
		case 'import_folder_unusable':
			return {
				sentence: "The import folder can't be used.",
				fixHref: settingsTab('library')
			};
		case 'publish_needs_attention':
			return {
				sentence: `${problem.count} file ${problem.count === 1 ? 'change needs' : 'changes need'} you.`,
				fixHref: '/library/management'
			};
		case 'disk_nearly_full':
			return {
				sentence: 'The data disk is nearly full. Backups and uploads stop.',
				fixHref: settingsTab('backups')
			};
		case 'backup_failing':
			return {
				sentence:
					subject === 'BACKUP_NO_ROOM'
						? 'The nightly backup has no room.'
						: 'The nightly backup found the database damaged.',
				fixHref: settingsTab('backups')
			};
		case 'jobs_failed':
			return {
				sentence:
					`${problem.count} ${subject ?? ''} ${problem.count === 1 ? 'job' : 'jobs'} failed today.`.replace(
						/\s+/g,
						' '
					),
				fixHref: settingsTab('diagnostics')
			};
		case 'download_client_setup':
			return {
				sentence: `${client(subject)} is on but not set up.`,
				fixHref: settingsTab('download-client')
			};
		case 'disk_low':
			return {
				sentence: `${(subject && ROLES[subject]) ?? 'A disk'} is running low.`,
				fixHref: settingsTab('backups')
			};
		case 'backup_old':
			return {
				sentence: 'No nightly backup for two days.',
				fixHref: settingsTab('backups')
			};
		case 'database_in_memory':
			return {
				sentence: 'The database is in memory. Nothing survives a restart.',
				fixHref: settingsTab('backups')
			};
		case 'could_not_check':
			return {
				sentence:
					subject === 'folder_checks'
						? "Folder checks didn't answer in time."
						: subject === 'jobs'
							? "Failed jobs couldn't be counted."
							: `${(subject && ROLES[subject]) ?? 'A disk'} couldn't be checked.`,
				fixHref: settingsTab('diagnostics')
			};
		case 'provider_degraded':
			return {
				sentence: `${(subject && PROVIDER_NAMES[subject]) ?? subject ?? 'An outside service'} isn't answering.`,
				fixHref: subject === 'musicbrainz' ? settingsTab('musicbrainz') : null
			};
	}
}

/** "for 3 h", "for 2 d": how long since `sinceAt` (unix ms). */
export function howLong(sinceAt: number | null | undefined, now: number): string | null {
	if (sinceAt == null) return null;
	const minutes = Math.max(0, Math.floor((now - sinceAt) / 60_000));
	if (minutes < 60) return `for ${Math.max(1, minutes)} min`;
	const hours = Math.floor(minutes / 60);
	if (hours < 48) return `for ${hours} h`;
	return `for ${Math.floor(hours / 24)} d`;
}

/** Counts by severity. */
export function countBySeverity(problems: Problem[]): Record<ProblemSeverity, number> {
	const counts: Record<ProblemSeverity, number> = { broken: 0, warning: 0, info: 0 };
	for (const problem of problems) counts[problem.severity] += 1;
	return counts;
}
