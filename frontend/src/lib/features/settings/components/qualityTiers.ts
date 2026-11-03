// left to right = worst to best; mirrors the server's TIER_KEYS_WORST_FIRST
// (kernel runtime_config, acquisition section). Consumed by the acquisition
// order editor, the upgrade cutoff selector and the requests page tier labels
// so none can disagree about the tier axis. Labels use lossy wording
// throughout ("MP3" never appears).
export interface QualityTier {
	key: string;
	label: string;
	full: string;
}

export const QUALITY_TIERS: QualityTier[] = [
	{ key: 'low', label: 'Lossy below 192', full: 'Lossy below 192 kbps' },
	{ key: 'mp3_192', label: 'Lossy 192-255', full: 'Lossy 192-255 kbps' },
	{ key: 'mp3_256', label: 'Lossy 256-319', full: 'Lossy 256-319 kbps' },
	{ key: 'mp3_320', label: 'Lossy 320 kbps', full: 'Lossy 320 kbps' },
	{ key: 'lossless', label: 'Lossless', full: 'Lossless' }
];

export function tierIndex(key: string): number {
	const i = QUALITY_TIERS.findIndex((t) => t.key === key);
	return i < 0 ? 0 : i;
}
