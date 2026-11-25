<!--
	The setup guide: what the server found on its network and in its mounted
	folders, in three steps. It only suggests. Each suggestion opens the form
	Settings shows with what was found filled in, and only that form's Save
	writes. It stores nothing: a step is done once its settings are saved.
-->
<script lang="ts">
	import { Flag, HardDriveDownload, Music } from 'lucide-svelte';
	import { getSetupFoundQuery } from '$lib/features/settings/queries/setupFound.svelte';
	import Button from '$lib/ui/Button.svelte';
	import PageShell from '$lib/ui/PageShell.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import Tabs, { type TabItem } from '$lib/ui/Tabs.svelte';
	import UnsavedChangesGuard from '$lib/ui/UnsavedChangesGuard.svelte';
	import SetupDownloadsStep from './SetupDownloadsStep.svelte';
	import SetupFinishStep from './SetupFinishStep.svelte';
	import SetupMusicStep from './SetupMusicStep.svelte';
	import { useSetupProgress } from './setupProgress.svelte';
	import type { SetupStep } from './steps';

	const found = getSetupFoundQuery();
	const progress = useSetupProgress();

	let step = $state<SetupStep>('music');

	const steps: TabItem<SetupStep>[] = [
		{ id: 'music', label: 'Music', icon: Music },
		{ id: 'downloads', label: 'Downloads', icon: HardDriveDownload },
		{ id: 'finish', label: 'Finish', icon: Flag }
	];
	const order: SetupStep[] = ['music', 'downloads', 'finish'];
	const nextStep = $derived(order[order.indexOf(step) + 1]);
	const stepDone = $derived(
		step === 'music'
			? progress.current.library
			: step === 'downloads'
				? progress.current.downloads
				: false
	);
</script>

<svelte:head><title>Setup guide - DroppedNeedle</title></svelte:head>

<PageShell
	title="Setup guide"
	subtitle="What DroppedNeedle found on this server. Nothing changes until you save a form."
	width="narrow"
>
	<Tabs
		tabs={steps}
		selected={step}
		onselect={(id) => (step = id)}
		label="Setup steps"
		idPrefix="setup"
	>
		<div class="space-y-4 pt-4">
			{#if stepDone}
				<StatusChip tone="success">Done</StatusChip>
			{/if}
			<QueryBoundary query={found} what="what the server found">
				{#snippet loading()}
					<div class="space-y-3" aria-label="Looking around">
						<div class="skeleton h-24 rounded-box"></div>
						<div class="skeleton h-24 rounded-box"></div>
					</div>
				{/snippet}
				{#snippet children(data)}
					{#if step === 'music'}
						<SetupMusicStep
							folders={data.folders}
							look={data.folder_look}
							acoustidSet={progress.current.acoustid}
							librarySet={progress.current.library}
						/>
					{:else if step === 'downloads'}
						<SetupDownloadsStep
							services={data.services}
							folders={data.folders}
							done={progress.current}
							usenetClientSet={progress.current.usenetClient}
							sourceSet={progress.current.downloads}
						/>
					{:else}
						<SetupFinishStep
							services={data.services}
							progress={progress.current}
							ongoto={(next) => (step = next)}
						/>
					{/if}
				{/snippet}
			</QueryBoundary>
			{#if nextStep}
				<div class="flex justify-end pt-2">
					<Button variant="outline" onclick={() => (step = nextStep)}
						>Next: {steps.find((item) => item.id === nextStep)?.label}</Button
					>
				</div>
			{/if}
		</div>
	</Tabs>
</PageShell>

<UnsavedChangesGuard />
