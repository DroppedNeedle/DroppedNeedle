import { describe, expect, it } from 'vitest';

import type { components } from '$lib/api/v3/openapi';
import { toDiscoverResponseV1, toHomeSection } from './DiscoverV3Adapters';

type V3Response = components['schemas']['DiscoverResponse'];
type V3Section = components['schemas']['ChartSection'];

const albumSection = (overrides: Partial<V3Section> = {}): V3Section =>
	({
		title: 'Fresh',
		type: 'albums',
		items: [
			{
				mbid: 'album-mbid',
				name: 'Blue Lines',
				artist_name: 'Massive Attack',
				artist_mbid: 'artist-mbid',
				in_library: true
			}
		],
		...overrides
	}) as V3Section;

describe('toHomeSection', () => {
	it('maps album rows onto the shelf shape the cards read', () => {
		const section = toHomeSection(albumSection());

		expect(section.type).toBe('albums');
		expect(section.items).toHaveLength(1);
		const [album] = section.items;
		expect(album).toMatchObject({
			mbid: 'album-mbid',
			name: 'Blue Lines',
			artist_name: 'Massive Attack',
			artist_mbid: 'artist-mbid',
			in_library: true
		});
	});

	it('defaults missing rows and flags to empty shelf values', () => {
		const section = toHomeSection(albumSection({ items: undefined }));

		expect(section.items).toEqual([]);
		expect(section.source).toBeNull();
		expect(section.fallback_message).toBeNull();
	});

	it('falls back to albums for an unknown section type', () => {
		const section = toHomeSection(albumSection({ type: 'mixtapes' }));

		expect(section.type).toBe('albums');
		expect(section.items).toHaveLength(1);
	});
});

describe('toDiscoverResponseV1', () => {
	const v3 = (overrides: Partial<V3Response> = {}): V3Response =>
		({
			discover_queue_enabled: true,
			genre_artwork_schema_version: 'v2',
			refreshing: false,
			...overrides
		}) as V3Response;

	it('keeps renderable shelves while preserving the build flags', () => {
		const page = toDiscoverResponseV1(
			v3({
				fresh_releases: albumSection(),
				top_picks: {
					title: 'Top picks',
					items: [
						{
							album: {
								mbid: 'pick-mbid',
								name: 'Mezzanine',
								artist_name: 'Massive Attack',
								artist_mbid: 'artist-mbid'
							},
							match_pct: 92
						}
					]
				},
				refreshing: true,
				refresh_started_at: 1700000000
			})
		);

		expect(page.fresh_releases?.title).toBe('Fresh');
		expect(page.top_picks?.items).toHaveLength(1);
		expect(page.top_picks?.items[0]).toMatchObject({
			match_pct: 92,
			reasons: [],
			seed_artist: null
		});
		expect(page.refreshing).toBe(true);
		expect(page.refresh_started_at).toBe(1700000000);
	});

	it('flattens integration availability into the record the page reads', () => {
		const page = toDiscoverResponseV1(
			v3({
				integration_status: {
					youtube: true,
					lastfm: false,
					listenbrainz: true,
					jellyfin: false,
					download_client: true
				}
			})
		);

		expect(page.integration_status.youtube).toBe(true);
		expect(page.integration_status.lastfm).toBe(false);
	});

	it('nulls absent shelves and prompts so the page renders its empty states', () => {
		const page = toDiscoverResponseV1(v3());

		expect(page.fresh_releases).toBeNull();
		expect(page.top_picks).toBeNull();
		expect(page.service_prompts).toEqual([]);
		expect(page.integration_status).toEqual({});
	});
});
