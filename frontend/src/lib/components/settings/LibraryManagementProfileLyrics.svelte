<!-- Lyrics and loudness: LRCLIB lyrics and ReplayGain analysis. -->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { AudioWaveform } from 'lucide-svelte';

	import LibraryManagementEditorAdvanced from './LibraryManagementEditorAdvanced.svelte';
	import LibraryManagementEditorSection from './LibraryManagementEditorSection.svelte';
	import type { EnrichmentManagementSettings } from '$lib/queries/library-management/types';
	import Card from '$lib/ui/Card.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	let { enrichment = $bindable() }: { enrichment: EnrichmentManagementSettings } = $props();

	const enabled = $derived(enrichment.lyrics.enabled || enrichment.replaygain.enabled);
	const lyricsOutputs = $derived.by(() => {
		if (!enrichment.lyrics.enabled) return 'Lyrics off';
		const outputs: string[] = [];
		if (enrichment.lyrics.write_synced) outputs.push('synced');
		if (enrichment.lyrics.write_plain) outputs.push('plain');
		return outputs.length > 0 ? `Lyrics · ${outputs.join(' + ')}` : 'Lyrics · no output';
	});
	const replayGainSummary = $derived(
		enrichment.replaygain.enabled
			? `ReplayGain · ${enrichment.replaygain.mode.replace('_', ' ')}`
			: 'ReplayGain off'
	);
</script>

<LibraryManagementEditorSection
	icon={AudioWaveform}
	title="Lyrics and loudness"
	subtitle="Optional LRCLIB lyrics and ReplayGain analysis"
	active={enabled}
	chips={[
		{ label: lyricsOutputs, active: enrichment.lyrics.enabled },
		{ label: replayGainSummary, active: enrichment.replaygain.enabled }
	]}
>
	<div class="mt-4 grid gap-4 lg:grid-cols-2">
		<Card as="section" variant="bordered" padding="md">
			<ToggleRow
				label="Fetch lyrics from LRCLIB"
				description="Exact title, artist, album, and duration matches only."
				bind:checked={enrichment.lyrics.enabled}
			/>
			<fieldset class="mt-2 grid gap-3" disabled={!enrichment.lyrics.enabled}>
				<div class="management-master-toggle">
					<ToggleRow
						label="Write plain lyrics"
						description="Uses Picard-compatible lyrics tags in every admitted format."
						bind:checked={enrichment.lyrics.write_plain}
					/>
				</div>
				<div class="management-master-toggle">
					<ToggleRow
						label="Write synchronized lyrics"
						description="Preferred when safely supported; plain lyrics remain the fallback."
						bind:checked={enrichment.lyrics.write_synced}
					/>
				</div>
				<LibraryManagementEditorAdvanced
					title="Advanced lyrics behavior"
					subtitle="Preservation and safety gates"
				>
					<div class="mt-3 grid gap-3">
						<div class="management-master-toggle">
							<ToggleRow
								label="Preserve existing lyrics"
								description="Fill only empty selected lyrics fields instead of replacing them."
								bind:checked={enrichment.lyrics.preserve_existing}
							/>
						</div>
						<div class="management-master-toggle">
							<ToggleRow
								label="Require lyrics"
								description="Hold the release when no selected lyrics format has an exact match."
								bind:checked={enrichment.lyrics.required}
							/>
						</div>
					</div>
				</LibraryManagementEditorAdvanced>
			</fieldset>
		</Card>

		<Card as="section" variant="bordered" padding="md">
			<ToggleRow
				label="Manage ReplayGain"
				description="Analyze loudness without changing audio samples."
				bind:checked={enrichment.replaygain.enabled}
			/>
			<fieldset class="mt-2 grid gap-3" disabled={!enrichment.replaygain.enabled}>
				<SelectField
					label="Existing ReplayGain values"
					help="Preserve keeps existing ReplayGain tags and performs no loudness analysis. Fill missing and Replace run analysis."
					options={[
						{ value: 'preserve', label: 'Preserve' },
						{ value: 'fill_missing', label: 'Fill missing' },
						{ value: 'replace', label: 'Replace' }
					]}
					bind:value={enrichment.replaygain.mode}
				/>
				<LibraryManagementEditorAdvanced
					title="Advanced analysis behavior"
					subtitle="Album coherence and safety gates"
				>
					<div class="mt-3 grid gap-3">
						<div class="management-master-toggle">
							<ToggleRow
								label="Album-aware analysis"
								description="Calculate coherent track and album gain/peak values."
								bind:checked={enrichment.replaygain.album_aware}
							/>
						</div>
						<div class="management-master-toggle">
							<ToggleRow
								label="Require ReplayGain"
								description="Hold the whole unit when the selected values are unavailable."
								bind:checked={enrichment.replaygain.required}
							/>
						</div>
					</div>
				</LibraryManagementEditorAdvanced>
			</fieldset>
		</Card>
	</div>
</LibraryManagementEditorSection>
