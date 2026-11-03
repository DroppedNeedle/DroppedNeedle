import { describe, expect, it } from 'vitest';
import { listeningSourceNotice, type SectionSources } from './listeningSourceNotice';

const answer = (overrides: Partial<SectionSources>): SectionSources => ({
	source: 'listenbrainz',
	asked_source: 'listenbrainz',
	unavailable_sources: [],
	configured: true,
	...overrides
});

describe('listeningSourceNotice', () => {
	it('stays quiet when the chosen service answered or nothing is set up', () => {
		expect(listeningSourceNotice([answer({}), undefined])).toBeNull();
		expect(
			listeningSourceNotice([answer({ configured: false, unavailable_sources: ['listenbrainz'] })])
		).toBeNull();
	});

	it('says why Last.fm stood in for ListenBrainz', () => {
		expect(
			listeningSourceNotice([
				answer({ source: 'lastfm', unavailable_sources: ['listenbrainz'] }),
				answer({})
			])
		).toBe('ListenBrainz is unavailable, so these lists come from Last.fm.');
		expect(listeningSourceNotice([answer({ source: 'lastfm' })])).toBe(
			'ListenBrainz has nothing here, so these lists come from Last.fm.'
		);
	});

	it('says when the chosen service failed', () => {
		expect(
			listeningSourceNotice([
				answer({ asked_source: 'lastfm', source: 'lastfm', unavailable_sources: ['lastfm'] })
			])
		).toBe('Last.fm is unavailable. These lists may be out of date.');
		expect(
			listeningSourceNotice([answer({ unavailable_sources: ['listenbrainz', 'lastfm'] })])
		).toBe('ListenBrainz and Last.fm are unavailable.');
	});
});
