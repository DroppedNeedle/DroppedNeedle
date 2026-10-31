import { describe, expect, it } from 'vitest';
import { listeningSourceNotice, type SectionSources } from './listeningSourceNotice';

const answer = (overrides: Partial<SectionSources>): SectionSources => ({
	source: 'listenbrainz',
	asked_source: 'listenbrainz',
	unavailable_sources: [],
	...overrides
});

describe('listeningSourceNotice', () => {
	it('stays quiet when the asked service answered', () => {
		expect(listeningSourceNotice([answer({}), undefined])).toBeNull();
	});

	it('says ListenBrainz is down and Last.fm stood in', () => {
		expect(
			listeningSourceNotice([
				answer({ source: 'lastfm', unavailable_sources: ['listenbrainz'] }),
				answer({})
			])
		).toBe('ListenBrainz is not available right now, so these lists come from Last.fm.');
	});

	it('says Last.fm had nothing and ListenBrainz stood in', () => {
		expect(
			listeningSourceNotice([answer({ source: 'listenbrainz', asked_source: 'lastfm' })])
		).toBe('Last.fm has nothing for this artist, so these lists come from ListenBrainz.');
	});

	it('says when neither service answers', () => {
		expect(
			listeningSourceNotice([
				answer({ asked_source: 'lastfm', unavailable_sources: ['lastfm', 'listenbrainz'] })
			])
		).toMatch(/^Neither ListenBrainz nor Last\.fm is available/);
	});

	it('says when the asked service failed and nothing stood in', () => {
		expect(
			listeningSourceNotice([
				answer({ asked_source: 'lastfm', source: 'lastfm', unavailable_sources: ['lastfm'] })
			])
		).toBe('Last.fm is not available right now, so these lists may be out of date.');
	});
});
