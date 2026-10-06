import { expect, it } from 'vitest';
import type { components } from '$lib/api/v3/openapi';
import { mergeArtistImages, toArtistInfoBasic } from './catalogAdapters';

type V3ArtistInfo = components['schemas']['ArtistInfo'];

const header: V3ArtistInfo = {
	musicbrainz_id: 'artist-1',
	name: 'Grimes',
	images: { banner_url: 'https://img/banner.jpg' },
	tags: [],
	aliases: [],
	external_links: [],
	in_library: true,
	appears_in_library: false,
	followed: false,
	auto_download: false,
	auto_download_state: 'none',
	release_group_count: 3,
	albums: [],
	eps: [],
	singles: [],
	source: 'musicbrainz',
	service_status: null
};

it('flattens header images and fills gaps from the extended read', () => {
	const basic = toArtistInfoBasic(header);
	expect(basic.banner_url).toBe('https://img/banner.jpg');
	expect(basic).not.toHaveProperty('images');

	const merged = mergeArtistImages(basic, {
		banner_url: 'https://img/other.jpg',
		thumb_url: 'https://img/thumb.jpg'
	});
	expect(merged.banner_url).toBe('https://img/banner.jpg');
	expect(merged.thumb_url).toBe('https://img/thumb.jpg');
});
