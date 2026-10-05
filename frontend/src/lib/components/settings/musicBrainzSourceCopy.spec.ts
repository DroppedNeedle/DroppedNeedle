import { describe, expect, it } from 'vitest';

import {
	BRAINZMASH_CONCURRENT_MAX,
	BRAINZMASH_PRIVACY_DISCLOSURE,
	BRAINZMASH_RATE_MAX,
	MORE_INFO_DISCLOSURES,
	NON_OFFICIAL_CONCURRENT_MAX,
	NON_OFFICIAL_RATE_MAX,
	OFFICIAL_CONCURRENT_MAX,
	OFFICIAL_RATE_MAX,
	UNLIMITED_RATE_SENTINEL,
	sourceBounds
} from './musicBrainzSourceCopy';

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

describe('BrainzMash source copy', () => {
	it('keeps privacy unknowns explicit and versioned', () => {
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/query terms/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/connection metadata/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/search terms/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/network or location/);
		expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/Retention, redaction/);
		expect(MORE_INFO_DISCLOSURES.brainzmash.join(' ')).toContain(BRAINZMASH_PRIVACY_DISCLOSURE);
	});
});
