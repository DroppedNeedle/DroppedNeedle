import type { SoundAnalysisStatus } from '$lib/features/settings/queries/SectionSettingsQueries.svelte';

type Tone = 'neutral' | 'info' | 'success' | 'warning';

/** The status chip's word and tone for what sound analysis is doing. */
export function describeSoundState(state: SoundAnalysisStatus['state']): {
	label: string;
	tone: Tone;
} {
	switch (state) {
		case 'off':
			return { label: 'Off', tone: 'neutral' };
		case 'starting':
			return { label: 'Starting', tone: 'neutral' };
		case 'running':
			return { label: 'Running', tone: 'info' };
		case 'waiting':
			return { label: 'Waiting', tone: 'warning' };
		case 'paused':
			return { label: 'Paused', tone: 'neutral' };
		case 'done':
			return { label: 'Done', tone: 'success' };
	}
}

const WAITING_FOR: Record<NonNullable<SoundAnalysisStatus['waiting_for']>, string> = {
	scan: 'Waiting for the scan to finish.',
	loudness: 'Waiting for loudness to be measured first.',
	streaming: 'Waiting while someone streams through a transcode.',
	folder_missing: 'Waiting for a library folder that is not there.',
	library_off: 'Waiting while the library is switched off.',
	settings_unreadable: "Waiting: its setting can't be read. Save it again."
};

/** The line under the counts while it waits, or null. */
export function waitingLine(status: SoundAnalysisStatus): string | null {
	if (status.state === 'paused') return 'Paused. Press Resume to carry on.';
	if (status.state !== 'waiting' || !status.waiting_for) return null;
	if (status.waiting_for === 'settings_unreadable') return WAITING_FOR.settings_unreadable;
	return `${WAITING_FOR[status.waiting_for]} Carries on by itself.`;
}

type Outcome = NonNullable<SoundAnalysisStatus['last_not_analysed']>['outcome'];

const NOT_ANALYSED: Record<Exclude<Outcome, 'analysed'>, string> = {
	undecodable: 'The file would not decode. It is tried again if it changes.',
	too_short: 'The file is under 3 seconds long. It is tried again if it changes.',
	over_time: 'The file took too long to read. It is tried again if it changes.'
};

/** Why the last file was not analysed, as one plain line. */
export function notAnalysedReason(outcome: Outcome): string {
	return outcome === 'analysed' ? '' : NOT_ANALYSED[outcome];
}

/**
 * A rough length of time: "6 hours", "3 days", "40 minutes"; `short` gives
 * "6 h", "3 d", "40 min".
 */
export function duration(seconds: number, short = false): string {
	const minutes = Math.max(1, Math.round(seconds / 60));
	if (minutes < 60)
		return short ? `${minutes} min` : `${minutes} minute${minutes === 1 ? '' : 's'}`;
	const hours = Math.round(minutes / 60);
	if (hours < 48) return short ? `${hours} h` : `${hours} hour${hours === 1 ? '' : 's'}`;
	const days = Math.round(hours / 24);
	return short ? `${days} d` : `${days} days`;
}
