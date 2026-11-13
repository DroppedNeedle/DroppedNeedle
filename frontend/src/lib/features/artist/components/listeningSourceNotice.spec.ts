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
	it('says which listening service stood in or failed, and nothing when all is well', () => {
		const cases: [SectionSources, (SectionSources | undefined)[], string | null][] = [
			[answer({}), [undefined], null],
			[answer({ configured: false, unavailable_sources: ['listenbrainz'] }), [], null],
			[
				answer({ source: 'lastfm', unavailable_sources: ['listenbrainz'] }),
				[answer({})],
				'ListenBrainz is off or unavailable, so these lists come from Last.fm.'
			],
			[
				answer({ source: 'lastfm' }),
				[],
				'ListenBrainz has nothing here, so these lists come from Last.fm.'
			],
			[
				answer({ asked_source: 'lastfm', source: 'lastfm', unavailable_sources: ['lastfm'] }),
				[],
				'Last.fm is off or unavailable. These lists may be out of date.'
			],
			[
				answer({ unavailable_sources: ['listenbrainz', 'lastfm'] }),
				[],
				'ListenBrainz and Last.fm are off or unavailable.'
			]
		];
		for (const [first, rest, expected] of cases) {
			expect(listeningSourceNotice([first, ...rest]), expected ?? 'quiet').toBe(expected);
		}
	});
});
