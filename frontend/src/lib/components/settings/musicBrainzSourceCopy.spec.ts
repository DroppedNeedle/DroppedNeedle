import { describe, expect, it } from 'vitest';

import {
	BRAINZMASH_CONCURRENT_MAX,
	BRAINZMASH_DISCLOSURE_VERSION,
	BRAINZMASH_ENDPOINT_URL,
	BRAINZMASH_LOCAL_POLICY_COPY,
	BRAINZMASH_PRIVACY_DISCLOSURE,
	BRAINZMASH_RATE_MAX,
	BRAINZMASH_SUPPORTED_ROUTE_FAMILIES,
	BRAINZMASH_TRANSPORT_DISABLED_COPY,
	COMMUNITY_CONFIRM_LABEL,
	COMMUNITY_RISK_BANNER,
	displayVerifyMessage,
	MIRROR_BANNER_LINES,
	MORE_INFO_DISCLOSURES,
	MUSICBRAINZ_GUIDE_HREF,
	MUSICBRAINZ_SOURCE_CARDS,
	NON_OFFICIAL_CONCURRENT_MAX,
	NON_OFFICIAL_RATE_MAX,
	OFFICIAL_CONCURRENT_MAX,
	OFFICIAL_RATE_MAX,
	UNLIMITED_RATE_SENTINEL,
	sourceBounds
} from './musicBrainzSourceCopy';

const OFFICIAL_503_MESSAGE = 'Connected, but rate-limited. Try lowering your rate limit.';

describe('MusicBrainz source bounds', () => {
	it('keeps Official at 1/1 and BrainzMash at the fixed local 10/1 policy', () => {
		expect(sourceBounds('official')).toEqual({
			rateMax: OFFICIAL_RATE_MAX,
			concurrentMax: OFFICIAL_CONCURRENT_MAX,
			allowUnlimitedRate: false
		});
		expect(sourceBounds('brainzmash')).toEqual({
			rateMax: BRAINZMASH_RATE_MAX,
			concurrentMax: BRAINZMASH_CONCURRENT_MAX,
			allowUnlimitedRate: false
		});
		expect(OFFICIAL_RATE_MAX).toBe(1);
		expect(OFFICIAL_CONCURRENT_MAX).toBe(1);
		expect(BRAINZMASH_RATE_MAX).toBe(10);
		expect(BRAINZMASH_CONCURRENT_MAX).toBe(1);
	});

	it('keeps mirror and community bounds separate from the fixed policy', () => {
		expect(sourceBounds('mirror')).toEqual({
			rateMax: NON_OFFICIAL_RATE_MAX,
			concurrentMax: NON_OFFICIAL_CONCURRENT_MAX,
			allowUnlimitedRate: true
		});
		expect(sourceBounds('community')).toEqual(sourceBounds('mirror'));
		expect(NON_OFFICIAL_RATE_MAX).toBe(500);
		expect(NON_OFFICIAL_CONCURRENT_MAX).toBe(64);
		expect(UNLIMITED_RATE_SENTINEL).toBe(0);
	});
});

describe('verify-result copy remap', () => {
	it('keeps the official-endpoint wording untouched for the official host', () => {
		expect(displayVerifyMessage(OFFICIAL_503_MESSAGE, 'official')).toBe(OFFICIAL_503_MESSAGE);
	});

	it('keeps BrainzMash failures explicit without suggesting a fallback', () => {
		const message = displayVerifyMessage(OFFICIAL_503_MESSAGE, 'brainzmash');
		expect(message).toContain('BrainzMash');
		expect(message).toContain('no other source');
		expect(message).not.toContain('Try lowering');
	});

	it('rewrites the 503 rate-limited advice for a mirror', () => {
		const remapped = displayVerifyMessage(OFFICIAL_503_MESSAGE, 'mirror');
		expect(remapped).toContain('503');
		expect(remapped).toContain('not a rate-limit problem');
		expect(remapped).not.toContain('Try lowering');
	});

	it('leaves unrelated messages alone', () => {
		const message = 'Could not connect to the specified endpoint';
		expect(displayVerifyMessage(message, 'official')).toBe(message);
		expect(displayVerifyMessage(message, 'brainzmash')).toBe(message);
		expect(displayVerifyMessage(message, 'mirror')).toBe(message);
	});
});

describe('BrainzMash source copy', () => {
	it('exposes four cards in the required order with BrainzMash as the only recommendation', () => {
		expect(MUSICBRAINZ_SOURCE_CARDS.map((card) => card.mode)).toEqual([
			'brainzmash',
			'official',
			'mirror',
			'community'
		]);
		expect(MUSICBRAINZ_SOURCE_CARDS[0].badge).toBe('Recommended');
		expect(MUSICBRAINZ_SOURCE_CARDS.slice(1).every((card) => card.badge === null)).toBe(true);
	});

	it('pins the approved endpoint, disclosure version, route boundary, and local policy copy', () => {
		expect(BRAINZMASH_ENDPOINT_URL).toBe('https://api.brainzmash.cc/ws/2');
		expect(BRAINZMASH_DISCLOSURE_VERSION).toMatch(/^2026-/);
		expect(BRAINZMASH_SUPPORTED_ROUTE_FAMILIES).toEqual([
			'artist',
			'release-group',
			'release',
			'recording',
			'isrc',
			'url'
		]);
		expect(BRAINZMASH_LOCAL_POLICY_COPY).toMatch(/10 requests\/second/);
		expect(BRAINZMASH_LOCAL_POLICY_COPY).toMatch(/capacity 1/);
		expect(BRAINZMASH_TRANSPORT_DISABLED_COPY).toMatch(/built-in source/);
	});

	it('keeps privacy unknowns explicit and versioned', () => {
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/query terms/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/connection metadata/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/search terms/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/network or location/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/Retention, redaction/);
		expect(MORE_INFO_DISCLOSURES.brainzmash.join(' ')).toContain(BRAINZMASH_PRIVACY_DISCLOSURE);
	});
});

describe('preserved source copy', () => {
	it('keeps official rate guidance and mirror ownership copy', () => {
		expect(MORE_INFO_DISCLOSURES.official.join(' ')).toMatch(/1 request\/second/);
		expect(MORE_INFO_DISCLOSURES.official.join(' ')).toMatch(/meaningful User-Agent/);
		expect(MORE_INFO_DISCLOSURES.mirror.join(' ')).toContain('8-16 GB');
		expect(MORE_INFO_DISCLOSURES.mirror.join(' ')).toContain('100-350 GB');
		expect(MORE_INFO_DISCLOSURES.mirror.join(' ')).toContain('replication schedule');
		expect(MORE_INFO_DISCLOSURES.mirror.join(' ')).toContain('reindex');
	});

	it('keeps community trust and acknowledgment copy', () => {
		const text = MORE_INFO_DISCLOSURES.community.join(' ');
		expect(text).toMatch(/front door/);
		expect(text).toMatch(/Be reasonable with servers/);
		expect(COMMUNITY_RISK_BANNER).toMatch(/search terms/);
		expect(COMMUNITY_CONFIRM_LABEL).toMatch(/I understand the risks/);
	});

	it('keeps the mirror guide link and warning lines', () => {
		expect(MIRROR_BANNER_LINES.join(' ')).toMatch(/reindex schedule/);
		expect(MIRROR_BANNER_LINES.join(' ')).toMatch(/replication cron/);
		expect(MIRROR_BANNER_LINES.join(' ')).toMatch(/depends on the operator/);
		expect(MUSICBRAINZ_GUIDE_HREF).toBe('/docs/musicbrainz-mirror-selfhosting.md');
	});
});
