import { describe, expect, it } from 'vitest';

import { ArtistReconciliationQueryKeyFactory } from './artist-reconciliation/ArtistReconciliationQueryKeyFactory';
import { AuthQueryKeyFactory } from './auth/AuthQueryKeyFactory';
import { DownloadQueryKeyFactory } from './downloads/DownloadQueryKeyFactory';
import { FollowQueryKeyFactory } from './following/FollowQueryKeyFactory';
import { FreeMusicQueryKeyFactory } from './free-music/FreeMusicQueryKeyFactory';
import { DropImportQueryKeyFactory } from './import/DropImportQueryKeyFactory';
import { LibraryManagementQueryKeyFactory } from './library-management/LibraryManagementQueryKeyFactory';
import { LibraryContributionQueryKeyFactory } from './libraryContributions/LibraryContributionQueryKeyFactory';
import { LidarrImportQueryKeyFactory } from './lidarr-import/LidarrImportQueryKeyFactory';
import { PluginQueryKeyFactory } from './plugins/PluginQueryKeyFactory';
import { ScrobblePreferencesQueryKeyFactory } from './scrobble-preferences/ScrobblePreferencesQueryKeyFactory';
import { SourcePlaylistQueryKeyFactory } from './source-playlists/SourcePlaylistQueryKeyFactory';

type UserKey = (userId: string | undefined) => readonly unknown[];

// Every user-scoped key a factory builds, by the user it belongs to.
const userKeys: Record<string, UserKey> = {
	'artist reconciliation': (u) => ArtistReconciliationQueryKeyFactory.progress(u),
	'download tasks scope': (u) => DownloadQueryKeyFactory.tasksPrefix(u),
	'download search': (u) => DownloadQueryKeyFactory.searchJob(u, 'job-1'),
	'follow status': (u) => FollowQueryKeyFactory.status('mbid-1', u),
	'followed artists': (u) => FollowQueryKeyFactory.artists(u),
	concerts: (u) => FollowQueryKeyFactory.concerts(u),
	'free music tasks': (u) => FreeMusicQueryKeyFactory.tasks(u, false),
	'drop import jobs': (u) => DropImportQueryKeyFactory.jobs(u, false),
	'library management settings': (u) => LibraryManagementQueryKeyFactory.settings(u),
	'library contributions': (u) => LibraryContributionQueryKeyFactory.detail(u, 'c-1'),
	'lidarr candidates': (u) => LidarrImportQueryKeyFactory.candidates(u),
	'plugin sources': (u) => PluginQueryKeyFactory.sources(u),
	'plugin ui': (u) => PluginQueryKeyFactory.ui(u, 'plugin-1'),
	'scrobble preferences': (u) => ScrobblePreferencesQueryKeyFactory.get(u),
	'source playlists': (u) => SourcePlaylistQueryKeyFactory.list(u, 'navidrome', 20),
	'import candidates': (u) => AuthQueryKeyFactory.importCandidates('plex', u)
};

describe('user-scoped query keys', () => {
	it.each(Object.entries(userKeys))('%s: two users never share a key', (_name, key) => {
		expect(key('user-a')).not.toEqual(key('user-b'));
		expect(key('user-a')).toContain('user-a');
	});

	it.each(Object.entries(userKeys))(
		'%s: a logged-out key carries the null segment, never a user id',
		(_name, key) => {
			const loggedOut = key(undefined);
			expect(loggedOut).toContain(null);
			expect(loggedOut).not.toContain('anon');
			expect(loggedOut).not.toContain('anonymous');
			expect(loggedOut).not.toEqual(key('user-a'));
		}
	);

	it('keys each quota by the user it describes', () => {
		expect(AuthQueryKeyFactory.userQuota('user-a')).not.toEqual(
			AuthQueryKeyFactory.userQuota('user-b')
		);
	});
});
