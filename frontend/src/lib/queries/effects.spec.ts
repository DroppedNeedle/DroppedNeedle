import { describe, expect, it, vi } from 'vitest';

vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: vi.fn() }));

import { EFFECTS, type Sweep } from './effects';
import { DownloadQueryKeyFactory } from './downloads/DownloadQueryKeyFactory';

const every = (feature: string, ...parts: string[]): Sweep => ({
	everyScope: [feature, ...parts]
});
const key = (...parts: string[]): Sweep => ({ key: parts });
const U = 'user:u';

// What each fact refreshes. A change here changes what the app reloads after
// a mutation or a server event.
const EXPECTED: Record<keyof typeof EFFECTS, Sweep[]> = {
	catalogChanged: [
		every('library'),
		every('library-management'),
		every('artist'),
		every('home'),
		every('discover'),
		every('genre'),
		every('lyrics'),
		every('local'),
		every('ratings'),
		every('downloads'),
		every('drop-import'),
		key(U, 'search')
	],
	libraryActivityChanged: [key(U, 'library', 'activity')],
	libraryOperationsChanged: [
		every('library', 'operations'),
		key('admin', 'library', 'reviews'),
		every('library-management')
	],
	localChanged: [every('local')],
	downloadsChanged: [key(U, 'downloads', 'tasks')],
	requestsChanged: [key(U, 'requests')],
	searchJobChanged: [key(U, 'downloads', 'search', 'job')],
	searchJobsChanged: [key(U, 'downloads', 'search')],
	dropImportsChanged: [every('drop-import'), { key: DownloadQueryKeyFactory.heldPrefix('u') }],
	playlistsChanged: [key(U, 'playlists')],
	newReleasesChanged: [key(U, 'following', 'new-releases-unseen')],
	concertsChanged: [key(U, 'following', 'concerts-unseen'), key(U, 'following', 'concerts')],
	wantedChanged: [key(U, 'wanted', 'list')],
	recommendationsChanged: [key(U, 'discover', 'ignored'), key(U, 'discover', 'home')]
};

describe('effects', () => {
	it.each(Object.keys(EXPECTED) as (keyof typeof EFFECTS)[])('%s refreshes its areas', (name) => {
		const build = EFFECTS[name] as (userId: string, jobId: string) => Sweep[];
		expect(JSON.parse(JSON.stringify(build('u', 'job')))).toEqual(EXPECTED[name]);
	});

	it('names every fact in the table', () => {
		expect(Object.keys(EFFECTS).sort()).toEqual(Object.keys(EXPECTED).sort());
	});
});
