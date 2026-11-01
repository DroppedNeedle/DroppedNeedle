import type { ScanFrequency } from '$lib/types';

interface ScanFrequencyOption {
	value: ScanFrequency;
	label: string;
	/** Hidden from the picker unless already selected (kept for back-compat). */
	legacy?: boolean;
}

export const SCAN_FREQUENCY_OPTIONS: ScanFrequencyOption[] = [
	{ value: 'manual', label: 'Manual only' },
	{ value: 'daily', label: 'Daily at a set time' },
	{ value: '5min', label: 'Every 5 minutes', legacy: true },
	{ value: '10min', label: 'Every 10 minutes', legacy: true },
	{ value: '30min', label: 'Every 30 minutes' },
	{ value: '1hr', label: 'Every hour' },
	{ value: '6hr', label: 'Every 6 hours' },
	{ value: '12hr', label: 'Every 12 hours' },
	{ value: '24hr', label: 'Every 24 hours' },
	{ value: '3d', label: 'Every 3 days' },
	{ value: '7d', label: 'Every 7 days' }
];
