// left to right = worst to best; mirrors backend services.native.quality_tiers
// (TIER_KEYS reversed). Shared by the quality range slider and the upgrade
// cutoff selector so the two can never disagree about the tier axis.
export interface QualityTier {
	key: string;
	label: string;
	full: string;
}

export const QUALITY_TIERS: QualityTier[] = [
	{ key: 'low', label: '<192', full: 'below 192 kbps' },
	{ key: 'mp3_192', label: '192', full: '192 kbps' },
	{ key: 'mp3_256', label: '256', full: '256 kbps' },
	{ key: 'mp3_320', label: '320', full: '320 kbps' },
	{ key: 'lossless', label: 'FLAC', full: 'FLAC / lossless' }
];

export function tierIndex(key: string): number {
	const i = QUALITY_TIERS.findIndex((t) => t.key === key);
	return i < 0 ? 0 : i;
}
