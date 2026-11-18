import { describe, expect, it } from 'vitest';

import {
	BRAINZMASH_ACTIVE_SOURCE_COPY,
	BRAINZMASH_LOCAL_POLICY_COPY,
	BRAINZMASH_PRIVACY_DISCLOSURE,
	BRAINZMASH_RATE_MAX,
	MORE_INFO_DISCLOSURES,
	NON_OFFICIAL_RATE_MAX,
	OFFICIAL_RATE_MAX,
	rateAfterSourceSwitch,
	sourceBounds
} from './musicBrainzSourceCopy';

describe('MusicBrainz source copy', () => {
	it('keeps each source within its rate bound and its privacy unknowns explicit', () => {
		{
			// Keeps Official at 1 and BrainzMash at one request a second, unlimited off.
			expect(sourceBounds('official')).toEqual({
				rateMax: OFFICIAL_RATE_MAX,
				allowUnlimitedRate: false
			});
			expect(sourceBounds('brainzmash')).toEqual({
				rateMax: BRAINZMASH_RATE_MAX,
				allowUnlimitedRate: false
			});
			expect(OFFICIAL_RATE_MAX).toBe(1);
			expect(BRAINZMASH_RATE_MAX).toBe(1);
			expect(BRAINZMASH_ACTIVE_SOURCE_COPY).toContain('1 request/second');
			expect(BRAINZMASH_LOCAL_POLICY_COPY).toContain('1 request/second');
			expect(BRAINZMASH_LOCAL_POLICY_COPY).toContain('token capacity 1');
			// Switching to BrainzMash shows that fixed rate. A custom mirror keeps its own.
			expect(rateAfterSourceSwitch('brainzmash', 'mirror', 10)).toBe(1);
			expect(rateAfterSourceSwitch('official', 'brainzmash', 10)).toBe(1);
			expect(rateAfterSourceSwitch('mirror', 'brainzmash', 10)).toBe(1);
			expect(rateAfterSourceSwitch('community', 'mirror', 4)).toBe(4);
		}

		{
			// Keeps mirror and community bounds separate from the fixed policy.
			expect(sourceBounds('mirror')).toEqual({
				rateMax: NON_OFFICIAL_RATE_MAX,
				allowUnlimitedRate: true
			});
			expect(sourceBounds('community')).toEqual(sourceBounds('mirror'));
			expect(NON_OFFICIAL_RATE_MAX).toBe(500);
		}

		{
			// Keeps privacy unknowns explicit and versioned.
			expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/query terms/);
			expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/connection metadata/);
			expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/search terms/);
			expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/network or location/);
			expect(BRAINZMASH_PRIVACY_DISCLOSURE).toMatch(/Retention, redaction/);
			expect(MORE_INFO_DISCLOSURES.brainzmash.join(' ')).toContain(BRAINZMASH_PRIVACY_DISCLOSURE);
		}
	});
});
