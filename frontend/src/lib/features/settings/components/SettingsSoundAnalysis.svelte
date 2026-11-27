<!--
	Sound analysis on Settings > Library, for admins: the switch, what it
	costs, how far it has come and why it waits, and Pause, which stops the
	background work and keeps what is analysed serving.
-->
<script lang="ts">
	import { Pause, Play, Clock } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatRow from '$lib/ui/StatRow.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import {
		createSaveSoundAnalysisSettingsMutation,
		getSoundAnalysisSettingsQuery,
		getSoundAnalysisStatusQuery,
		type SoundAnalysisStatus
	} from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
	import {
		describeSoundState,
		duration,
		notAnalysedReason,
		waitingLine
	} from './soundAnalysisStatus';

	const section = createSettingsSection(
		getSoundAnalysisSettingsQuery(),
		createSaveSoundAnalysisSettingsMutation()
	);
	const statusQuery = getSoundAnalysisStatusQuery();

	const chip = $derived(statusQuery.data ? describeSoundState(statusQuery.data.state) : null);
	const cost = $derived(
		statusQuery.data && statusQuery.data.tracks > 0
			? ` About ${duration(statusQuery.data.library_seconds)} for this library, in the background.`
			: ' It works in the background.'
	);

	function percent(status: SoundAnalysisStatus): number {
		const done = status.analysed + status.not_analysed;
		return status.tracks > 0 ? Math.round((done / status.tracks) * 100) : 0;
	}

	function figures(status: SoundAnalysisStatus) {
		const shown = [
			{ label: 'Analysed', value: status.analysed.toLocaleString() },
			{ label: 'To go', value: status.to_go.toLocaleString() },
			{ label: 'Not read', value: status.not_analysed.toLocaleString() }
		];
		// Time left only while there is something left.
		return status.to_go > 0
			? [...shown, { label: 'Left', value: `~${duration(status.seconds_left, true)}` }]
			: shown;
	}

	async function setEnabled(on: boolean) {
		if (!section.draft) return;
		section.draft.sound_analysis_enabled = on;
		if (!on) section.draft.sound_analysis_paused = false;
		await section.save();
	}

	async function togglePause() {
		if (!section.draft) return;
		section.draft.sound_analysis_paused = !section.draft.sound_analysis_paused;
		await section.save();
	}
</script>

<Card as="section" variant="bordered">
	<div class="flex flex-col gap-4">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<h3 class="font-semibold">Sound analysis</h3>
			{#if chip}
				<StatusChip tone={chip.tone}>{chip.label}</StatusChip>
			{/if}
		</div>
		<QueryBoundary query={section} what="the sound analysis setting">
			{#snippet children(draft)}
				<div class="flex flex-col gap-4">
					<ToggleRow
						label="Analyse how tracks sound"
						description={`Reads your music to find songs that sound alike.${cost}`}
						bind:checked={() => draft.sound_analysis_enabled, setEnabled}
						disabled={section.saving}
					/>
					{#if draft.sound_analysis_enabled}
						<QueryBoundary query={statusQuery} what="how far sound analysis has come">
							{#snippet children(status)}
								<div class="flex flex-col gap-3 rounded-box border border-edge p-4">
									<div class="flex flex-wrap items-center justify-between gap-3">
										<StatRow label="Sound analysis progress" phoneRows stats={figures(status)} />
										<Button
											variant="outline"
											icon={draft.sound_analysis_paused ? Play : Pause}
											loading={section.saving}
											onclick={togglePause}
										>
											{draft.sound_analysis_paused ? 'Resume' : 'Pause'}
										</Button>
									</div>
									<progress
										class="progress progress-primary w-full"
										value={percent(status)}
										max="100"
										aria-label={`Tracks analysed: ${percent(status)}%`}
									></progress>
									{#if waitingLine(status)}
										<p class="flex items-center gap-2 text-sm">
											<Clock class="size-4 shrink-0 text-warning" aria-hidden="true" />
											<span>{waitingLine(status)}</span>
										</p>
									{/if}
									{#if status.last_not_analysed}
										<div class="flex flex-col gap-1 border-t border-line pt-3">
											<span
												class="font-mono text-[10px] font-bold tracking-widest text-muted uppercase"
											>
												Last one not read
											</span>
											<span class="text-sm break-all">
												{status.last_not_analysed.relative_path}
											</span>
											<span class="text-xs text-muted">
												{notAnalysedReason(status.last_not_analysed.outcome)}
											</span>
										</div>
									{/if}
									<p class="font-mono text-[10px] tracking-widest text-muted uppercase">
										Model {status.model} - analysis version {status.analysis_version}
									</p>
								</div>
							{/snippet}
						</QueryBoundary>
					{/if}
					{#if section.status?.tone === 'error'}
						<p class="text-sm text-error">{section.status.message}</p>
					{/if}
				</div>
			{/snippet}
		</QueryBoundary>
	</div>
</Card>
